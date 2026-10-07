//! Shared by the CLI's end-to-end tests: mock servers, a fake `ssh` on PATH, and a
//! rig that runs the real binary with every side effect redirected into a temp dir.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use offrig_core::config::Config;

pub type Hits = Arc<Mutex<Vec<String>>>;

/// A tiny HTTP server. `handler(route, body)` answers `(status, body)`; the route is
/// `"METHOD /path"` without the query.
pub fn mock(handler: impl Fn(&str, &str) -> (u16, String) + Send + 'static) -> (String, Hits) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let hits: Hits = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").split('?').next().unwrap_or("");
            let route = format!("{method} {path}");
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            let _ = reader.read_exact(&mut body);
            let body = String::from_utf8_lossy(&body).to_string();
            log.lock().expect("hits").push(route.clone());
            let (status, text) = handler(&route, &body);
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = reader.get_mut().write_all(resp.as_bytes());
        }
    });
    (format!("http://{addr}"), hits)
}

pub fn count(h: &Hits, route: &str) -> usize {
    h.lock()
        .expect("hits")
        .iter()
        .filter(|x| *x == route)
        .count()
}

pub fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("offrig-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

pub fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr").port()
}

/// Build (once per test process; a no-op when fresh) and locate the fake ssh: an
/// example of offrig-core. Cargo does not build examples for every kind of test run,
/// so build it here, into the same target directory and with the same flags.
fn fake_ssh_exe() -> PathBuf {
    static EXE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        let exe = std::env::current_exe().expect("test exe");
        let target = exe
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("target dir");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "--locked", "--quiet", "-p", "offrig-core"])
            .args(["--example", "fake_ssh"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("CARGO_TARGET_DIR", target)
            .status()
            .expect("run cargo");
        assert!(status.success(), "building the fake ssh failed");
        target
            .join("debug")
            .join("examples")
            .join(format!("fake_ssh{}", std::env::consts::EXE_SUFFIX))
    })
    .clone()
}

/// The fake ssh/scp/ssh-keygen/zed binaries, copied into `<dir>/bin` under those names.
pub fn install_fake_tools(dir: &Path) -> PathBuf {
    let src = fake_ssh_exe();
    assert!(src.is_file(), "{} is missing", src.display());
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    for name in ["ssh", "scp", "ssh-keygen", "zed"] {
        std::fs::copy(
            &src,
            bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
        )
        .expect("copy fake tool");
    }
    bin
}

/// A pod model server: answers what the tunnel checks ask.
pub fn ollama_routes(models: Vec<String>) -> impl Fn(&str, &str) -> (u16, String) + Send + 'static {
    move |route, _| {
        match route {
        "GET /api/version" => (200, r#"{"version":"0.35.0"}"#.into()),
        "GET /v1/models" => (
            200,
            serde_json::json!({"data": models.iter().map(|m| serde_json::json!({"id": m})).collect::<Vec<_>>()})
                .to_string(),
        ),
        "GET /api/tags" => (
            200,
            serde_json::json!({"models": models.iter().map(|m| serde_json::json!({"name": m, "size": 1000})).collect::<Vec<_>>()})
                .to_string(),
        ),
        "GET /api/ps" => (
            200,
            serde_json::json!({"models": models.iter().map(|m| serde_json::json!({"name": m, "size_vram": 2_000_000_000u64})).collect::<Vec<_>>()})
                .to_string(),
        ),
        "POST /api/show" => (
            200,
            r#"{"capabilities":["tools","vision"],"model_info":{"llama.context_length":32768}}"#
                .into(),
        ),
        "POST /v1/chat/completions" => (
            200,
            "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\",\"tool_calls\":[{\"function\":{\"name\":\"read_file\"}}]}}]}\n\ndata: [DONE]\n\n"
                .into(),
        ),
        _ => (404, "{}".into()),
    }
    }
}

/// One isolated rig for the `offrig` binary.
pub struct Rig {
    pub dir: PathBuf,
    pub cfg: Config,
    pub runpod: String,
    pub runpod_hits: Hits,
    pub ollama_hits: Hits,
    pub fake: PathBuf,
    bin: PathBuf,
    ollama_addr: String,
}

impl Rig {
    /// `runpod` answers the mock RunPod; the pod's model server serves `models`.
    pub fn new(
        name: &str,
        tweak: impl FnOnce(&mut Config),
        models: &[&str],
        runpod: impl Fn(&str, &str) -> (u16, String) + Send + 'static,
    ) -> Rig {
        let dir = temp(name);
        let (runpod_url, runpod_hits) = mock(runpod);
        let (ollama_url, ollama_hits) = mock(ollama_routes(
            models.iter().map(|m| (*m).to_string()).collect(),
        ));
        let mut cfg = Config {
            tunnel_port: free_port(),
            zed_provider: "covtest".into(),
            ssh_alias: "covtest-pod".into(),
            role_os_dir: None,
            ..Config::default()
        };
        tweak(&mut cfg);
        cfg.save_to(&dir.join("cfg").join("config.toml"))
            .expect("write config");
        let fake = dir.join("fake");
        std::fs::create_dir_all(&fake).expect("fake dir");
        let bin = install_fake_tools(&dir);
        Rig {
            dir,
            cfg,
            runpod: runpod_url,
            runpod_hits,
            ollama_hits,
            fake,
            bin,
            ollama_addr: ollama_url.trim_start_matches("http://").to_string(),
        }
    }

    /// Replace the fake ssh's answers (a JSON list of rules; see the fake's docs).
    pub fn rules(&self, rules: serde_json::Value) {
        std::fs::write(self.fake.join("rules.json"), rules.to_string()).expect("rules");
        for e in std::fs::read_dir(&self.fake).expect("fake dir").flatten() {
            if e.file_name().to_string_lossy().ends_with(".count") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    pub fn fake_calls(&self) -> String {
        std::fs::read_to_string(self.fake.join("calls.log")).unwrap_or_default()
    }

    pub fn settings(&self) -> PathBuf {
        self.dir.join("home").join("zed").join("settings.json")
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_offrig"));
        let path = std::env::join_paths(std::iter::once(self.bin.clone()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .expect("path");
        c.args(args)
            .current_dir(&self.dir)
            .env("OFFRIG_CONFIG_DIR", self.dir.join("cfg"))
            .env("OFFRIG_TEST_HOME", self.dir.join("home"))
            .env("RUNPOD_API_KEY", "rpa_TESTKEY_0123456789")
            .env("OFFRIG_TEST_RUNPOD_BASE", &self.runpod)
            .env("FAKE_SSH_DIR", &self.fake)
            .env("FAKE_SSH_UPSTREAM", &self.ollama_addr)
            .env("PATH", path)
            .env(
                offrig_core::zed::api_key_env_name(&self.cfg.zed_provider),
                "offrig-tunnel",
            )
            .env_remove("RUST_BACKTRACE");
        c
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run offrig")
    }
}

pub fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

pub fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}
