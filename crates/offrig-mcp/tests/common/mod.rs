//! Shared by the end-to-end tests: a tiny HTTP server standing in for RunPod or the
//! pod's Ollama, and a fresh temp project directory.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

pub type Hits = Arc<Mutex<Vec<String>>>;

/// A small HTTP server standing in for RunPod. `handler(route, body, nth)`.
pub fn mock(
    handler: impl Fn(&str, &str, usize) -> (u16, String) + Send + 'static,
) -> (String, Hits) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
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
            let path = parts
                .next()
                .unwrap_or("")
                .split('?')
                .next()
                .unwrap_or("")
                .to_string();
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
            let nth = {
                let mut l = log.lock().expect("hits");
                let n = l.iter().filter(|h| **h == route).count();
                l.push(route.clone());
                n
            };
            let (status, text) = handler(&route, &body, nth);
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = reader.get_mut().write_all(resp.as_bytes());
        }
    });
    (url, hits)
}

pub fn count(h: &Hits, route: &str) -> usize {
    h.lock()
        .expect("hits")
        .iter()
        .filter(|x| *x == route)
        .count()
}

pub fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("offrig-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// Build (once per test process; a no-op when fresh) and locate the fake ssh: an
/// example of offrig-core. Cargo does not build examples for every kind of test run,
/// so build it here, into the same target directory and with the same flags.
fn fake_ssh_exe() -> std::path::PathBuf {
    use std::path::Path;
    static EXE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        let exe = std::env::current_exe().expect("test exe");
        let target = exe
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("target dir");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let status = std::process::Command::new(cargo)
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

/// A temp `bin` with the fake ssh, scp, ssh-keygen and zed in it, and a temp dir the
/// fake logs to and takes its rules from. Returns `(bin, fake_dir)`.
pub fn install_fake_tools(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
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
    let fake = dir.join("fake");
    std::fs::create_dir_all(&fake).expect("fake dir");
    (bin, fake)
}

/// PATH with `bin` in front.
pub fn path_with(bin: &std::path::Path) -> std::ffi::OsString {
    std::env::join_paths(
        std::iter::once(bin.to_path_buf()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )),
    )
    .expect("path")
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}
