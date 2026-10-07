use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::AtomicU8;
use std::sync::mpsc;

use offrig_core::config::ModelEntry;

const CHILD: &str = "OFFRIG_APP_TEST_CHILD";

const POD: &str = r#"{"id":"p1","name":"offrig-small","desiredStatus":"RUNNING","costPerHr":0.5,"publicIp":"203.0.113.9","portMappings":{"22":22022},"ports":["22/tcp"],"gpuCount":1}"#;
const ACCOUNT: &str =
    r#"{"data":{"myself":{"clientBalance":20.0,"currentSpendPerHr":0.5,"spendLimit":null}}}"#;
const OFFERS: &str = r#"{"data":{"gpuTypes":[{"id":"NVIDIA RTX 2000 Ada Generation","displayName":"RTX 2000 Ada","memoryInGb":16,"secureCloud":true,"lowestPrice":{"uninterruptablePrice":0.3,"stockStatus":"High"}}]}}"#;
const MODELS: &str = r#"{"data":[{"id":"podonly:1b"}]}"#;

// --- tests that run in a child process with their own environment -----------
//
// The worker reads process-wide state: the RunPod base URL and key, offrig's
// config directory, the home directory (ssh config, Zed settings) and PATH (ssh,
// zed). Setting those in the test process would be unsound and would let tests
// disturb each other, so each such test re-runs itself in a child process whose
// environment the parent sets, and only the child runs the body. `ssh` and `zed`
// there are `tests/support/fake_tools.rs`; the pod and its Ollama are servers on
// 127.0.0.1. Nothing reaches RunPod, a real ssh or a real Zed.

pub(crate) struct Rig {
    dir: PathBuf,
    base: String,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

/// A port for a child's mock RunPod. The parent cannot hand over a bound socket, so
/// it names a port in a block of its own (by process id), counted up per test, and
/// the child binds it. Ephemeral ports (what `free_port` gives) lie above this range.
fn reserve_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let base = 20_000 + (std::process::id() % 250) as u16 * 40;
    loop {
        let port = base + NEXT.fetch_add(1, Ordering::SeqCst) % 40;
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

/// The fake `ssh` and `zed` (`tests/support/fake_tools.rs`), compiled with `rustc`
/// the first time any test needs them and kept in the temp directory under a name
/// that carries a hash of the source. Cargo does not build it for us: coverage runs
/// use `cargo test --tests`, which skips examples and second binaries.
fn fake_tools() -> PathBuf {
    use std::hash::{DefaultHasher, Hash, Hasher};
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_tools.rs");
            let text = std::fs::read_to_string(&src).expect("fake_tools.rs");
            let mut hash = DefaultHasher::new();
            text.hash(&mut hash);
            let dir =
                std::env::temp_dir().join(format!("offrig-app-fake-tools-{:016x}", hash.finish()));
            let exe = dir.join(format!("fake_tools{}", std::env::consts::EXE_SUFFIX));
            if !exe.is_file() {
                std::fs::create_dir_all(&dir).expect("temp dir");
                let tmp = dir.join(format!(
                    "build-{}{}",
                    std::process::id(),
                    std::env::consts::EXE_SUFFIX
                ));
                let status = Command::new("rustc")
                    .args(["--edition", "2021", "-C", "opt-level=0", "-o"])
                    .arg(&tmp)
                    .arg(&src)
                    .status()
                    .expect("run rustc");
                assert!(status.success(), "rustc could not build {}", src.display());
                // Another test process may have won the race; either copy is the same.
                if std::fs::rename(&tmp, &exe).is_err() {
                    let _ = std::fs::remove_file(&tmp);
                }
                assert!(exe.is_file(), "{} was not built", exe.display());
            }
            exe
        })
        .clone()
}

impl Rig {
    /// In the parent: run `test` again in a child and return `None`. In the
    /// child: return the rig for the body to use.
    pub(crate) fn enter(module: &str, test: &str) -> Option<Rig> {
        Rig::enter_with(module, test, true)
    }

    /// `zed` says whether the child's PATH has a `zed` on it. Without, PATH holds
    /// nothing else either, so no real `zed` can be found.
    pub(crate) fn enter_with(module: &str, test: &str, zed: bool) -> Option<Rig> {
        if let Some(dir) = std::env::var_os(CHILD) {
            return Some(Rig {
                dir: PathBuf::from(dir),
                base: std::env::var("OFFRIG_TEST_RUNPOD_BASE").expect("runpod base"),
            });
        }
        let dir = std::env::temp_dir().join(format!("offrig-app-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["bin", "fake", "home", "cfg"] {
            std::fs::create_dir_all(dir.join(d)).expect("temp dir");
        }
        let tools = fake_tools();
        let names: &[&str] = if zed { &["ssh", "zed"] } else { &["ssh"] };
        for name in names {
            let to = dir
                .join("bin")
                .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            // A link, not a copy: a file being written while another test forks a
            // process cannot be executed ("text file busy").
            #[cfg(unix)]
            std::os::unix::fs::symlink(&tools, &to).expect("link the fake tool");
            #[cfg(not(unix))]
            std::fs::copy(&tools, &to).expect("copy the fake tool");
        }
        let mut path = vec![dir.join("bin")];
        if zed {
            path.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
        }
        let module = module.split_once("::").map_or("", |(_, rest)| rest);
        let out = Command::new(std::env::current_exe().expect("test exe"))
            .args([&format!("{module}::{test}"), "--exact", "--test-threads=1"])
            .env(CHILD, &dir)
            .env("OFFRIG_CONFIG_DIR", dir.join("cfg"))
            .env("HOME", dir.join("home"))
            .env("USERPROFILE", dir.join("home"))
            .env("RUNPOD_API_KEY", "test-key")
            .env("OFFRIG_API_KEY", "offrig-tunnel")
            .env(
                "OFFRIG_TEST_RUNPOD_BASE",
                format!("http://127.0.0.1:{}", reserve_port()),
            )
            .env("FAKE_SSH_DIR", dir.join("fake"))
            .env("PATH", std::env::join_paths(path).expect("PATH"))
            .output()
            .expect("run the test in a child process");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{test} failed in its child process:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
        None
    }

    /// Set what the fake `ssh` answers (see `tests/support/fake_tools.rs`).
    fn fake(&self, name: &str, text: &str) {
        std::fs::write(self.dir.join("fake").join(name), text).expect("write fake answer");
    }

    /// Every remote script the fake `ssh` was asked to run.
    fn ssh_calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("fake").join("calls")).unwrap_or_default()
    }

    /// A mock RunPod on the address the child's environment names.
    pub(crate) fn runpod(
        &self,
        handler: impl Fn(&str, &str, usize) -> (u16, String) + Send + 'static,
    ) -> Mock {
        let port = self.base.rsplit(':').next().expect("port");
        serve(
            TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind the reserved port"),
            handler,
        )
    }

    /// The pod's Ollama, reached through the fake ssh tunnel.
    fn pod_ollama(&self) -> Mock {
        let m = mock(ollama_reply);
        self.fake("upstream", &m.addr);
        self.fake("models", MODELS);
        m
    }

    #[cfg(unix)]
    fn zed_settings(&self) -> PathBuf {
        self.dir.join("home").join(".config/zed/settings.json")
    }

    #[cfg(unix)]
    fn ssh_config(&self) -> PathBuf {
        self.dir.join("home").join(".ssh/config")
    }
}

// --- servers ------------------------------------------------------------------

/// A tiny HTTP server. `handler` gets the route ("GET /pods"), the request body
/// and how many times that route was hit before.
pub(crate) struct Mock {
    url: String,
    addr: String,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    pub(crate) fn count(&self, route: &str) -> usize {
        self.hits
            .lock()
            .expect("hits lock")
            .iter()
            .filter(|h| *h == route)
            .count()
    }
}

fn mock(handler: impl Fn(&str, &str, usize) -> (u16, String) + Send + 'static) -> Mock {
    serve(TcpListener::bind("127.0.0.1:0").expect("bind"), handler)
}

fn serve(
    listener: TcpListener,
    handler: impl Fn(&str, &str, usize) -> (u16, String) + Send + 'static,
) -> Mock {
    let addr = listener.local_addr().expect("addr").to_string();
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(std::result::Result::ok) {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let route = format!(
                "{} {}",
                parts.next().unwrap_or(""),
                parts.next().unwrap_or("").split('?').next().unwrap_or("")
            );
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
            let nth = {
                let mut l = log.lock().expect("hits lock");
                let n = l.iter().filter(|h| **h == route).count();
                l.push(route.clone());
                n
            };
            let (status, text) = handler(&route, &String::from_utf8_lossy(&body), nth);
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = reader.get_mut().write_all(resp.as_bytes());
        }
    });
    Mock {
        url: format!("http://{addr}"),
        addr,
        hits,
    }
}

/// RunPod's answers for a healthy account whose pod list is `pods`.
fn runpod_reply(route: &str, body: &str, pods: &str) -> (u16, String) {
    match route {
        "GET /pods" => (200, pods.to_string()),
        "GET /pods/p1" | "POST /pods" => (200, POD.to_string()),
        "DELETE /pods/p1" => (200, "{}".into()),
        "POST /graphql" if body.contains("myself") => (200, ACCOUNT.into()),
        "POST /graphql" => (200, OFFERS.into()),
        _ => (404, "{}".into()),
    }
}

fn ollama_reply(route: &str, _body: &str, _nth: usize) -> (u16, String) {
    match route {
        "GET /api/version" => (200, r#"{"version":"0.35.0"}"#.into()),
        "GET /api/tags" => (
            200,
            r#"{"models":[{"name":"podonly:1b","size":1000000000,"details":{"parameter_size":"1B","quantization_level":"Q4_0"}}]}"#.into(),
        ),
        "POST /api/show" => (
            200,
            r#"{"capabilities":["tools"],"model_info":{"llama.context_length":4096}}"#.into(),
        ),
        "GET /v1/models" => (200, MODELS.into()),
        "POST /v1/chat/completions" => (
            200,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"name\":\"read_file\"}}]}}]}\n\n",
                "data: [DONE]\n\n"
            )
            .into(),
        ),
        _ => (404, "{}".into()),
    }
}

// --- the worker under test --------------------------------------------------------

fn test_cfg() -> Config {
    let mut profiles = offrig_core::config::default_profiles();
    for p in &mut profiles {
        if p.name == "small" {
            p.models = vec![ModelEntry {
                name: "podonly:1b".into(),
                size_gb: 1.0,
                tools: true,
                images: false,
            }];
        }
    }
    Config {
        tunnel_port: free_port(),
        active_profile: "small".into(),
        auto_stop_idle_minutes: Some(30),
        profiles,
        ..Config::default()
    }
}

fn worker(cfg: Config, rp: &Mock) -> (Worker, mpsc::Receiver<Update>) {
    let (tx, rx) = mpsc::channel();
    let rp = RunPod::new("test-key", &rp.url, &format!("{}/graphql", rp.url));
    let w = Worker {
        session: Session::with_client(cfg, rp),
        out: Outbox::new(tx, egui::Context::default()),
        tunnel: None,
        want_tunnel: false,
        idle: None,
        previous_default: None,
        cancel: Arc::new(AtomicBool::new(false)),
    };
    (w, rx)
}

fn drain(rx: &mpsc::Receiver<Update>) -> Vec<Update> {
    rx.try_iter().collect()
}

fn logs(us: &[Update]) -> Vec<String> {
    us.iter()
        .filter_map(|u| match u {
            Update::Log(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn errors(us: &[Update]) -> Vec<String> {
    us.iter()
        .filter_map(|u| match u {
            Update::Error(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn has_log(us: &[Update], needle: &str) -> bool {
    logs(us).iter().any(|l| l.contains(needle))
}

fn kind(u: &Update) -> &'static str {
    match u {
        Update::Config(_) => "Config",
        Update::Account(_) => "Account",
        Update::Pods(_) => "Pods",
        Update::Offers(..) => "Offers",
        Update::Log(_) => "Log",
        Update::Error(_) => "Error",
        Update::Busy(_) => "Busy",
        Update::Pull { .. } => "Pull",
        Update::Tunnel(_) => "Tunnel",
        Update::PodModels(_) => "PodModels",
        Update::Guard(_) => "Guard",
        Update::Check(_) => "Check",
        Update::Gpu(_) => "Gpu",
        Update::Idle(_) => "Idle",
        Update::Zed(_) => "Zed",
    }
}

fn kinds(us: &[Update]) -> Vec<&'static str> {
    us.iter().map(kind).collect()
}

fn tunnel_is_up(w: &mut Worker) -> bool {
    w.tunnel.as_mut().is_some_and(Tunnel::is_alive)
}

// --- plain tests ----------------------------------------------------------------------

#[test]
fn only_the_long_commands_show_a_busy_label() {
    let quiet = [
        Cmd::Refresh,
        Cmd::Offers(1),
        Cmd::SetProfile("small".into()),
        Cmd::SetAutoStop(None),
        Cmd::CloseTunnel,
        Cmd::OpenZedRemote,
    ];
    for c in &quiet {
        assert_eq!(busy_label(c), None, "{c:?}");
    }
    let labelled = [
        (Cmd::Launch, "Launching the pod"),
        (Cmd::OpenTunnel, "Opening the tunnel"),
        (Cmd::Pull("m:1b".into()), "Pulling m:1b"),
        (
            Cmd::ConfigureZed {
                default_model: None,
            },
            "Writing Zed settings",
        ),
        (Cmd::RemoveZed, "Removing the Zed provider"),
        (Cmd::Guard, "Running guard checks"),
        (Cmd::Check("m:1b".into()), "Testing m:1b"),
    ];
    for (c, label) in labelled {
        assert_eq!(busy_label(&c).as_deref(), Some(label), "{c:?}");
    }
}

#[test]
fn the_outbox_survives_a_closed_window() {
    let (tx, rx) = mpsc::channel();
    let out = Outbox::new(tx, egui::Context::default());
    out.send(Update::Log("first".into()));
    assert!(matches!(rx.try_recv(), Ok(Update::Log(s)) if s == "first"));
    drop(rx);
    out.send(Update::Log("nobody is listening".into()));
}

#[test]
fn worker_events_become_updates() {
    let Some(rig) = Rig::enter(module_path!(), "worker_events_become_updates") else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, "[]"));
    let (w, rx) = worker(test_cfg(), &rp);
    let pod: Pod = serde_json::from_str(POD).expect("pod json");
    {
        let mut on = w.events();
        on(Event::Step("a step".into()));
        on(Event::Warn("a warning".into()));
        on(Event::Pod(Box::new(pod)));
        on(Event::Pull {
            model: "m:1b".into(),
            state: PullState::Done,
        });
        on(Event::Waiting {
            attempt: 2,
            waited_secs: 60,
            limit_secs: 600,
        });
    }
    let us = drain(&rx);
    assert_eq!(
        kinds(&us),
        ["Log", "Log", "Log", "Pull"],
        "Waiting says nothing"
    );
    let l = logs(&us);
    assert_eq!(l[0], "a step");
    assert_eq!(l[1], "a warning");
    assert!(
        l[2].contains("pod p1 reachable at") && l[2].contains("22022"),
        "{}",
        l[2]
    );
    assert!(
        matches!(&us[3], Update::Pull { model, state } if model == "m:1b" && *state == PullState::Done)
    );
}

// --- read-only commands ------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn refresh_reports_pods_zed_and_the_balance() {
    let Some(rig) = Rig::enter(module_path!(), "refresh_reports_pods_zed_and_the_balance") else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::Refresh);
    let us = drain(&rx);
    assert_eq!(
        kinds(&us),
        ["Pods", "Zed", "Account"],
        "no busy banner for a refresh"
    );
    assert!(matches!(&us[0], Update::Pods(p) if p.len() == 1 && p[0].id == "p1"));
    assert!(
        matches!(&us[1], Update::Zed(z) if z.key_env),
        "OFFRIG_API_KEY is set"
    );
    assert!(matches!(&us[2], Update::Account(a) if (a.client_balance - 20.0).abs() < 1e-9));
}

#[cfg(unix)]
#[test]
fn a_balance_outage_does_not_hide_the_pods() {
    let Some(rig) = Rig::enter(module_path!(), "a_balance_outage_does_not_hide_the_pods") else {
        return;
    };
    let rp = rig.runpod(|r, b, _| {
        if r == "POST /graphql" {
            (500, r#"{"error":"down"}"#.into())
        } else {
            runpod_reply(r, b, &format!("[{POD}]"))
        }
    });
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::Refresh);
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Pods", "Zed", "Log"]);
    assert!(has_log(&us, "balance unavailable"), "{:?}", logs(&us));
    assert!(errors(&us).is_empty());
}

#[test]
fn a_failed_refresh_is_an_error_banner_but_only_a_log_line_when_background() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "a_failed_refresh_is_an_error_banner_but_only_a_log_line_when_background",
    ) else {
        return;
    };
    let rp = rig.runpod(|_, _, _| (503, r#"{"error":"unavailable"}"#.into()));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::Refresh);
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Error"]);
    assert!(errors(&us)[0].contains("503") && errors(&us)[0].contains("list pods"));
    w.quiet(Cmd::Refresh);
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Log"]);
    assert!(
        logs(&us)[0].starts_with("refresh failed: "),
        "{:?}",
        logs(&us)
    );
}

#[test]
fn offers_arrive_tagged_with_their_gpu_count_or_fail_loudly() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "offers_arrive_tagged_with_their_gpu_count_or_fail_loudly",
    ) else {
        return;
    };
    let bad = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&bad);
    let rp = rig.runpod(move |r, b, _| {
        if flag.load(Ordering::SeqCst) {
            (500, "{}".into())
        } else {
            runpod_reply(r, b, "[]")
        }
    });
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::Offers(2));
    let us = drain(&rx);
    assert!(matches!(&us[..], [Update::Offers(2, o)]
        if o.len() == 1 && o[0].id == "NVIDIA RTX 2000 Ada Generation" && o[0].price_per_hr == Some(0.3)));
    bad.store(true, Ordering::SeqCst);
    w.do_cmd(Cmd::Offers(2));
    assert_eq!(kinds(&drain(&rx)), ["Error"]);
}

// --- settings -----------------------------------------------------------------------------

#[test]
fn switching_profile_saves_it_and_asks_for_that_profiles_offers() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "switching_profile_saves_it_and_asks_for_that_profiles_offers",
    ) else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, "[]"));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::SetProfile("medium".into()));
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Config", "Offers"]);
    assert!(matches!(&us[0], Update::Config(c) if c.active_profile == "medium"));
    assert!(matches!(&us[1], Update::Offers(1, _)));
    let saved = Config::load().expect("saved config loads");
    assert_eq!(saved.active_profile, "medium");

    w.do_cmd(Cmd::SetProfile("no-such-profile".into()));
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Error"]);
    assert!(errors(&us)[0].contains("no-such-profile"));
    assert_eq!(
        w.session.cfg.active_profile, "medium",
        "a bad name changes nothing"
    );
    assert_eq!(Config::load().expect("config").active_profile, "medium");
}

#[test]
fn auto_stop_is_saved_and_rearms_the_idle_tracker() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "auto_stop_is_saved_and_rearms_the_idle_tracker",
    ) else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, "[]"));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::SetAutoStop(Some(45)));
    let us = drain(&rx);
    assert!(matches!(&us[..], [Update::Config(c)] if c.auto_stop_idle_minutes == Some(45)));
    assert_eq!(
        w.idle.as_ref().map(|t| t.limit),
        Some(Duration::from_secs(45 * 60))
    );
    assert_eq!(
        Config::load().expect("config").auto_stop_idle_minutes,
        Some(45)
    );

    w.do_cmd(Cmd::SetAutoStop(None));
    let us = drain(&rx);
    assert!(matches!(&us[..], [Update::Config(c)] if c.auto_stop_idle_minutes.is_none()));
    assert!(w.idle.is_none());
}

#[test]
fn closing_the_tunnel_clears_the_wish_for_one() {
    let Some(rig) = Rig::enter(module_path!(), "closing_the_tunnel_clears_the_wish_for_one") else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, "[]"));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.want_tunnel = true;
    w.do_cmd(Cmd::CloseTunnel);
    assert!(!w.want_tunnel && w.tunnel.is_none());
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Tunnel", "Log"]);
    assert!(matches!(&us[0], Update::Tunnel(false)));
    assert_eq!(logs(&us), ["tunnel closed"]);
}

// --- refusals before anything is touched ------------------------------------------------------

#[test]
fn launching_a_job_profile_is_refused_before_anything_is_rented() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "launching_a_job_profile_is_refused_before_anything_is_rented",
    ) else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, "[]"));
    let cfg = Config {
        active_profile: "job".into(),
        ..test_cfg()
    };
    let (mut w, rx) = worker(cfg, &rp);
    w.do_cmd(Cmd::Launch);
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Busy", "Error", "Busy"]);
    assert!(matches!(&us[0], Update::Busy(Some(l)) if l == "Launching the pod"));
    assert!(matches!(&us[2], Update::Busy(None)));
    assert!(errors(&us)[0].contains("job"), "{:?}", errors(&us));
    assert_eq!(rp.count("POST /pods"), 0);
}

#[test]
fn commands_that_need_a_pod_name_the_missing_one() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "commands_that_need_a_pod_name_the_missing_one",
    ) else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, "[]"));
    let (mut w, rx) = worker(test_cfg(), &rp);
    for cmd in [
        Cmd::Guard,
        Cmd::OpenTunnel,
        Cmd::OpenZedRemote,
        Cmd::Pull("podonly:2b".into()),
        Cmd::Check("podonly:1b".into()),
        Cmd::ConfigureZed {
            default_model: None,
        },
    ] {
        w.do_cmd(cmd.clone());
        let us = drain(&rx);
        let errs = errors(&us);
        assert_eq!(errs.len(), 1, "{cmd:?}: {errs:?}");
        assert!(errs[0].contains("offrig-small"), "{cmd:?}: {}", errs[0]);
    }
    assert!(w.tunnel.is_none());
    assert!(rig.ssh_calls().is_empty(), "no pod, no ssh");
}

#[test]
fn a_pull_of_a_badly_named_model_never_reaches_the_pod() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "a_pull_of_a_badly_named_model_never_reaches_the_pod",
    ) else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::Pull("rm -rf /; x".into()));
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Busy", "Error", "Busy"]);
    assert!(errors(&us)[0].contains("not a valid model name"));
    assert!(rig.ssh_calls().is_empty());
}

// --- the tunnel --------------------------------------------------------------------------------

#[test]
fn a_tunnel_that_cannot_start_reports_why() {
    let Some(rig) = Rig::enter(module_path!(), "a_tunnel_that_cannot_start_reports_why") else {
        return;
    };
    rig.fake("tunnel_fail", "Permission denied (publickey).");
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    let err = w.open_tunnel().expect_err("ssh refuses");
    assert!(
        chain(&err).contains("tunnel exited") && chain(&err).contains("Permission denied"),
        "{}",
        chain(&err)
    );
    assert!(
        w.want_tunnel,
        "the user still wants one; the supervisor retries"
    );
    assert!(w.tunnel.is_none());
    assert!(drain(&rx).is_empty());
}

#[test]
fn the_supervisor_leaves_a_live_or_unwanted_tunnel_alone() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "the_supervisor_leaves_a_live_or_unwanted_tunnel_alone",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.supervise_tunnel();
    assert!(drain(&rx).is_empty(), "no tunnel wanted, none open");
    w.open_tunnel().expect("tunnel opens");
    assert!(tunnel_is_up(&mut w));
    let up = drain(&rx);
    assert_eq!(kinds(&up), ["Log", "Log", "Tunnel"], "{:?}", logs(&up));
    assert!(has_log(&up, "answering through the tunnel"));
    w.supervise_tunnel();
    assert!(drain(&rx).is_empty(), "a live tunnel is left alone");
    assert_eq!(rp.count("GET /pods"), 0);
    w.want_tunnel = false;
    w.tunnel.as_mut().expect("tunnel").stop();
    w.supervise_tunnel();
    assert!(
        drain(&rx).is_empty(),
        "a tunnel the user closed stays closed"
    );
}

#[test]
fn a_dropped_tunnel_is_not_reopened_to_a_pod_that_is_gone_or_unreachable() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "a_dropped_tunnel_is_not_reopened_to_a_pod_that_is_gone_or_unreachable",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    // 0: the pod is listed, 1: the list is empty, 2: RunPod errors.
    let mode = Arc::new(AtomicU8::new(0));
    let m = Arc::clone(&mode);
    let rp = rig.runpod(move |r, b, _| match (r, m.load(Ordering::SeqCst)) {
        ("GET /pods", 1) => (200, "[]".into()),
        ("GET /pods", 2) => (500, "{}".into()),
        _ => runpod_reply(r, b, &format!("[{POD}]")),
    });
    let (mut w, rx) = worker(test_cfg(), &rp);

    // The pod is gone: stop wishing for a tunnel.
    w.open_tunnel().expect("tunnel opens");
    drain(&rx);
    w.tunnel.as_mut().expect("tunnel").stop();
    mode.store(1, Ordering::SeqCst);
    w.supervise_tunnel();
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Tunnel", "Log", "Log"]);
    assert!(matches!(&us[0], Update::Tunnel(false)));
    assert!(has_log(&us, "tunnel dropped") && has_log(&us, "reopening"));
    assert!(has_log(&us, "the pod is gone; tunnel closed"));
    assert!(!w.want_tunnel && w.tunnel.is_none());

    // RunPod cannot be asked: keep wishing, say so.
    w.open_tunnel().expect("tunnel opens");
    drain(&rx);
    w.tunnel.as_mut().expect("tunnel").stop();
    mode.store(2, Ordering::SeqCst);
    w.supervise_tunnel();
    let us = drain(&rx);
    assert!(has_log(&us, "tunnel not reopened"), "{:?}", logs(&us));
    assert!(
        w.want_tunnel && w.tunnel.is_none(),
        "it will try again next tick"
    );
}

#[cfg(unix)]
#[test]
fn a_dropped_tunnel_is_reopened_while_the_pod_is_up() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "a_dropped_tunnel_is_reopened_while_the_pod_is_up",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.open_tunnel().expect("tunnel opens");
    drain(&rx);
    w.tunnel.as_mut().expect("tunnel").stop();
    w.supervise_tunnel();
    let us = drain(&rx);
    assert_eq!(
        kinds(&us),
        ["Tunnel", "Log", "Log", "Log", "Tunnel"],
        "{:?}",
        logs(&us)
    );
    assert!(matches!(&us[0], Update::Tunnel(false)));
    assert!(matches!(&us[4], Update::Tunnel(true)));
    assert!(tunnel_is_up(&mut w) && w.want_tunnel);
    let config = std::fs::read_to_string(rig.ssh_config()).expect("ssh config written");
    assert!(config.contains("HostName 203.0.113.9") && config.contains("Port 22022"));
}

// --- GPUs and the idle stop -----------------------------------------------------------------------

fn busy_gpu() -> &'static str {
    "0, NVIDIA Test GPU, 90, 2048, 24576\n"
}

fn idle_gpu() -> &'static str {
    "0, NVIDIA Test GPU, 0, 512, 24576\n"
}

#[test]
fn sampling_needs_a_tunnel_and_reports_what_the_gpus_say() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "sampling_needs_a_tunnel_and_reports_what_the_gpus_say",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    rig.fake("gpu", busy_gpu());

    w.sample_gpus();
    assert!(drain(&rx).is_empty(), "no tunnel, no sample");
    assert!(rig.ssh_calls().is_empty());

    w.open_tunnel().expect("tunnel opens");
    drain(&rx);
    w.sample_gpus();
    let us = drain(&rx);
    assert!(
        matches!(&us[..], [Update::Gpu(g)]
        if g.len() == 1 && g[0].util_pct == 90 && g[0].mem_used_mb == 2048
            && g[0].mem_total_mb == 24576 && g[0].name == "NVIDIA Test GPU"),
        "without an idle tracker only the reading is sent"
    );

    w.idle = Some(IdleTracker::new(Duration::from_secs(1800)));
    w.sample_gpus();
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Gpu", "Idle"]);
    assert!(matches!(&us[1], Update::Idle(Idle::Busy)));

    // ssh fails: no readings, which is never a reason to stop.
    rig.fake("fail", "Connection timed out");
    w.sample_gpus();
    let us = drain(&rx);
    assert!(matches!(&us[0], Update::Gpu(g) if g.is_empty()));
    assert!(matches!(&us[1], Update::Idle(Idle::Unknown)));
    assert_eq!(rp.count("DELETE /pods/p1"), 0);
}

#[test]
fn a_pod_idle_past_the_limit_is_terminated_and_its_tunnel_dropped() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "a_pod_idle_past_the_limit_is_terminated_and_its_tunnel_dropped",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    let delete_fails = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&delete_fails);
    let rp = rig.runpod(move |r, b, _| {
        if r == "DELETE /pods/p1" && flag.load(Ordering::SeqCst) {
            (500, r#"{"error":"nope"}"#.into())
        } else {
            runpod_reply(r, b, &format!("[{POD}]"))
        }
    });
    let (mut w, rx) = worker(test_cfg(), &rp);
    rig.fake("gpu", idle_gpu());
    w.open_tunnel().expect("tunnel opens");
    drain(&rx);

    // A tracker that has watched the GPUs sit idle for two minutes, limit one.
    let mut tracker = IdleTracker::new(Duration::from_secs(60));
    let two_minutes_ago = Instant::now()
        .checked_sub(Duration::from_secs(120))
        .expect("the machine has been up for two minutes");
    let stats = remote::parse_gpu_stats(idle_gpu());
    assert_eq!(
        tracker.observe(&stats, two_minutes_ago),
        Idle::Idle(Duration::ZERO)
    );
    w.idle = Some(tracker);

    w.sample_gpus();
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Gpu", "Idle", "Log", "Tunnel"]);
    assert!(matches!(&us[1], Update::Idle(Idle::Stop)));
    assert!(has_log(
        &us,
        "every GPU idle for 1 min: terminating offrig-small"
    ));
    assert!(matches!(&us[3], Update::Tunnel(false)));
    assert_eq!(rp.count("DELETE /pods/p1"), 1);
    assert!(
        w.tunnel.is_none() && !w.want_tunnel,
        "the supervisor must not reopen it"
    );

    // The same again, but RunPod refuses the delete: that is an error banner.
    delete_fails.store(true, Ordering::SeqCst);
    w.open_tunnel().expect("tunnel opens");
    drain(&rx);
    w.sample_gpus();
    let us = drain(&rx);
    assert!(
        errors(&us)[0].starts_with("auto-stop failed: "),
        "{:?}",
        errors(&us)
    );
}

#[test]
fn nothing_is_terminated_when_the_idle_pod_is_already_gone() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "nothing_is_terminated_when_the_idle_pod_is_already_gone",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, "[]"));
    let (mut w, rx) = worker(test_cfg(), &rp);
    rig.fake("gpu", idle_gpu());
    w.open_tunnel().expect("tunnel opens");
    drain(&rx);
    w.idle = Some(IdleTracker::new(Duration::ZERO));
    w.sample_gpus();
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Gpu", "Idle"]);
    assert!(matches!(&us[1], Update::Idle(Idle::Stop)));
    assert_eq!(rp.count("DELETE /pods/p1"), 0);
    assert!(
        tunnel_is_up(&mut w),
        "with no pod to end, the tunnel is left to the supervisor"
    );
}

// --- the periodic tick and the thread ---------------------------------------------------------------

#[cfg(unix)]
#[test]
fn the_tick_refreshes_and_samples_only_when_due() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "the_tick_refreshes_and_samples_only_when_due",
    ) else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    let now = Instant::now();
    let mut due = Schedule {
        refresh: now + Duration::from_secs(5),
        gpu: now + Duration::from_secs(5),
    };
    w.tick(now, &mut due);
    assert!(drain(&rx).is_empty(), "nothing is due yet");
    assert_eq!(rp.count("GET /pods"), 0);

    let later = now + Duration::from_secs(10);
    w.tick(later, &mut due);
    assert_eq!(kinds(&drain(&rx)), ["Pods", "Zed", "Account"]);
    assert_eq!(due.refresh, later + REFRESH_EVERY);
    assert_eq!(
        due.gpu,
        later + GPU_EVERY,
        "the GPU sample came due too (no tunnel: silent)"
    );
    assert_eq!(rp.count("GET /pods"), 1);
}

#[cfg(unix)]
fn recv_until(rx: &mpsc::Receiver<Update>, seen: &mut Vec<Update>, done: impl Fn(&Update) -> bool) {
    loop {
        let u = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the worker thread keeps talking");
        let stop = done(&u);
        seen.push(u);
        if stop {
            return;
        }
    }
}

#[cfg(unix)]
#[test]
fn the_worker_thread_announces_itself_obeys_commands_and_ends_with_the_window() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "the_worker_thread_announces_itself_obeys_commands_and_ends_with_the_window",
    ) else {
        return;
    };
    let _rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (upd_tx, upd_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    spawn(
        test_cfg(),
        Outbox::new(upd_tx, egui::Context::default()),
        cmd_rx,
        Arc::new(AtomicBool::new(false)),
    );

    let mut seen = Vec::new();
    recv_until(&upd_rx, &mut seen, |u| matches!(u, Update::Offers(..)));
    assert_eq!(
        kinds(&seen),
        ["Config", "Pods", "Zed", "Account", "Offers"],
        "config first, then a refresh, then the market"
    );

    // Longer than the worker's one-second receive timeout, so its idle loop runs.
    std::thread::sleep(Duration::from_millis(1300));
    cmd_tx.send(Cmd::CloseTunnel).expect("worker listens");
    let mut seen = Vec::new();
    recv_until(
        &upd_rx,
        &mut seen,
        |u| matches!(u, Update::Log(s) if s == "tunnel closed"),
    );
    assert!(matches!(seen[0], Update::Tunnel(false)));

    drop(cmd_tx);
    loop {
        match upd_rx.recv_timeout(Duration::from_secs(20)) {
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("the worker thread did not end"),
        }
    }
}

#[test]
fn a_config_the_session_refuses_is_reported_and_ends_the_thread() {
    let Some(_rig) = Rig::enter(
        module_path!(),
        "a_config_the_session_refuses_is_reported_and_ends_the_thread",
    ) else {
        return;
    };
    let (upd_tx, upd_rx) = mpsc::channel();
    let (_cmd_tx, cmd_rx) = mpsc::channel();
    let cfg = Config {
        // The local Ollama's port: a dead tunnel could fall through to the local GPU.
        tunnel_port: 11434,
        ..test_cfg()
    };
    spawn(
        cfg,
        Outbox::new(upd_tx, egui::Context::default()),
        cmd_rx,
        Arc::new(AtomicBool::new(false)),
    );
    let first = upd_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("an error first");
    assert!(
        matches!(&first, Update::Error(e) if e.contains("11434")),
        "{first:?}"
    );
    let second = upd_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("then the config");
    assert!(matches!(&second, Update::Config(c) if c.tunnel_port == 11434));
    assert!(
        matches!(
            upd_rx.recv_timeout(Duration::from_secs(20)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ),
        "the thread ends"
    );
}

#[test]
fn shutdown_now_terminates_from_its_own_thread_and_says_how_it_went() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "shutdown_now_terminates_from_its_own_thread_and_says_how_it_went",
    ) else {
        return;
    };
    let fail = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&fail);
    let rp = rig.runpod(move |r, b, _| {
        if r == "DELETE /pods/p1" && flag.load(Ordering::SeqCst) {
            (500, r#"{"error":"nope"}"#.into())
        } else {
            runpod_reply(r, b, "[]")
        }
    });
    let pod: Pod = serde_json::from_str(POD).expect("pod json");
    let (tx, rx) = mpsc::channel();
    let out = Outbox::new(tx, egui::Context::default());

    shutdown_now(pod.clone(), out.clone());
    let u = rx.recv_timeout(Duration::from_secs(20)).expect("an update");
    assert!(
        matches!(&u, Update::Log(s) if s == "terminated offrig-small (p1)"),
        "{u:?}"
    );
    assert_eq!(rp.count("DELETE /pods/p1"), 1);

    fail.store(true, Ordering::SeqCst);
    shutdown_now(pod, out);
    let u = rx.recv_timeout(Duration::from_secs(20)).expect("an update");
    assert!(
        matches!(&u, Update::Error(s) if s.starts_with("shutting down offrig-small: ")),
        "{u:?}"
    );
}

// --- commands that write ssh config, Zed settings or run zed (unix: HOME is the temp dir) --------------

#[cfg(unix)]
#[test]
fn opening_the_tunnel_writes_the_ssh_alias_and_lists_the_pods_models() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "opening_the_tunnel_writes_the_ssh_alias_and_lists_the_pods_models",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::OpenTunnel);
    let us = drain(&rx);
    assert_eq!(
        kinds(&us),
        ["Busy", "Log", "Log", "Tunnel", "PodModels", "Busy"],
        "{:?}",
        logs(&us)
    );
    assert!(matches!(&us[0], Update::Busy(Some(l)) if l == "Opening the tunnel"));
    assert!(matches!(&us[4], Update::PodModels(t)
        if t.len() == 1 && t[0].name == "podonly:1b" && t[0].details.parameter_size == "1B"));
    assert!(matches!(&us[5], Update::Busy(None)));
    assert!(tunnel_is_up(&mut w) && w.want_tunnel);
    let config = std::fs::read_to_string(rig.ssh_config()).expect("ssh config written");
    assert!(config.contains("Host offrig\n"), "{config}");
    assert!(
        config.contains("HostName 203.0.113.9") && config.contains("Port 22022"),
        "{config}"
    );
}

#[cfg(unix)]
#[test]
fn launching_creates_the_pod_and_wires_the_tunnel_zed_and_the_guard() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "launching_creates_the_pod_and_wires_the_tunnel_zed_and_the_guard",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    // The lane is empty until the pod is created.
    let rp = rig.runpod(|r, b, n| {
        let pods = if n == 0 {
            "[]".to_string()
        } else {
            format!("[{POD}]")
        };
        runpod_reply(r, b, &pods)
    });
    let (mut w, rx) = worker(test_cfg(), &rp);
    // A cancel left over from an earlier wait must not stop this launch.
    w.cancel.store(true, Ordering::SeqCst);
    w.do_cmd(Cmd::Launch);
    let us = drain(&rx);
    assert!(errors(&us).is_empty(), "{:?}", errors(&us));
    let k = kinds(&us);
    assert_eq!(k.first(), Some(&"Busy"));
    assert_eq!(k.last(), Some(&"Busy"));
    for expected in ["Pods", "Tunnel", "PodModels", "Zed", "Guard"] {
        assert!(k.contains(&expected), "{expected} in {k:?}");
    }
    assert!(matches!(&us[0], Update::Busy(Some(l)) if l == "Launching the pod"));
    assert!(!w.cancel.load(Ordering::SeqCst));
    assert_eq!(rp.count("POST /pods"), 1);
    assert!(has_log(&us, "creating offrig-small"));
    assert!(has_log(&us, "pod p1 reachable"));
    assert!(has_log(&us, "all profile models are on the pod"));
    assert!(has_log(&us, "Zed provider written to"));
    assert!(has_log(&us, "offrig-small is ready"));
    assert!(tunnel_is_up(&mut w));

    let zed = us.iter().rev().find_map(|u| match u {
        Update::Zed(z) => Some(z.clone()),
        _ => None,
    });
    assert_eq!(zed.expect("a Zed update").provider_models, ["podonly:1b"]);
    let guard = us.iter().find_map(|u| match u {
        Update::Guard(g) => Some(g.clone()),
        _ => None,
    });
    let guard = guard.expect("a guard update");
    assert_eq!(guard.len(), 7);
    let failing: Vec<_> = guard
        .iter()
        .filter(|c| !c.ok)
        .map(|c| (c.name, &c.detail))
        .collect();
    assert!(failing.is_empty(), "{failing:?}");
    let settings = std::fs::read_to_string(rig.zed_settings()).expect("Zed settings written");
    assert!(
        settings.contains("podonly:1b") && settings.contains("offrig"),
        "{settings}"
    );
}

#[cfg(unix)]
#[test]
fn launching_again_reuses_the_pod_that_is_up() {
    let Some(rig) = Rig::enter(module_path!(), "launching_again_reuses_the_pod_that_is_up") else {
        return;
    };
    let _ollama = rig.pod_ollama();
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::Launch);
    let us = drain(&rx);
    assert!(errors(&us).is_empty(), "{:?}", errors(&us));
    assert!(
        has_log(&us, "offrig-small is already up (p1)"),
        "{:?}",
        logs(&us)
    );
    assert_eq!(rp.count("POST /pods"), 0, "nothing new is rented");
    assert!(has_log(&us, "offrig-small is ready"));
}

#[test]
fn a_launch_runpod_refuses_ends_with_an_error_and_a_clear_busy_flag() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "a_launch_runpod_refuses_ends_with_an_error_and_a_clear_busy_flag",
    ) else {
        return;
    };
    let rp = rig.runpod(|r, b, _| {
        if r == "POST /pods" {
            (400, r#"{"error":"invalid image"}"#.into())
        } else {
            runpod_reply(r, b, "[]")
        }
    });
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.cancel.store(true, Ordering::SeqCst);
    w.do_cmd(Cmd::Launch);
    let us = drain(&rx);
    assert_eq!(kinds(&us).first(), Some(&"Busy"));
    assert_eq!(kinds(&us).last(), Some(&"Busy"));
    assert!(matches!(us.last(), Some(Update::Busy(None))));
    assert!(errors(&us)[0].contains("400"), "{:?}", errors(&us));
    assert!(
        !w.cancel.load(Ordering::SeqCst),
        "the launch re-armed the cancel flag"
    );
    assert!(w.tunnel.is_none());
}

#[cfg(unix)]
#[test]
fn a_pull_reports_progress_until_the_pod_says_it_is_done() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "a_pull_reports_progress_until_the_pod_says_it_is_done",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    rig.fake(
        "pull",
        "ALIVE\n{\"status\":\"pulling\",\"total\":100,\"completed\":40}\n===\nDEAD\n{\"status\":\"success\"}",
    );
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.open_tunnel().expect("tunnel opens");
    drain(&rx);
    w.do_cmd(Cmd::Pull("podonly:2b".into()));
    let us = drain(&rx);
    assert_eq!(
        kinds(&us),
        ["Busy", "Log", "Pull", "Pull", "Log", "PodModels", "Busy"],
        "{:?}",
        logs(&us)
    );
    assert!(
        matches!(&us[2], Update::Pull { model, state: PullState::Running { completed: 40, total: 100, .. } }
        if model == "podonly:2b")
    );
    assert!(matches!(
        &us[3],
        Update::Pull {
            state: PullState::Done,
            ..
        }
    ));
    assert_eq!(
        logs(&us),
        ["podonly:2b: pull started on the pod", "podonly:2b: pulled"]
    );
    let calls = rig.ssh_calls();
    assert!(
        calls.contains("nohup curl") && calls.contains("podonly:2b"),
        "{calls}"
    );

    // With the tunnel closed there is nothing to list the models through.
    w.do_cmd(Cmd::CloseTunnel);
    drain(&rx);
    rig.fake(
        "pull",
        "DEAD
{\"status\":\"success\"}",
    );
    w.do_cmd(Cmd::Pull("podonly:2b".into()));
    let us = drain(&rx);
    assert_eq!(
        kinds(&us),
        ["Busy", "Log", "Pull", "Log", "Busy"],
        "{:?}",
        errors(&us)
    );
}

#[cfg(unix)]
#[test]
fn a_pull_that_fails_or_vanishes_is_an_error() {
    let Some(rig) = Rig::enter(module_path!(), "a_pull_that_fails_or_vanishes_is_an_error") else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);

    rig.fake("pull", "DEAD\n{\"error\":\"disk full\"}");
    w.do_cmd(Cmd::Pull("podonly:2b".into()));
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Busy", "Log", "Pull", "Error", "Busy"]);
    assert!(
        errors(&us)[0].contains("pull of podonly:2b failed: disk full"),
        "{:?}",
        errors(&us)
    );

    rig.fake("pull", "NOFILE");
    w.do_cmd(Cmd::Pull("podonly:2b".into()));
    let us = drain(&rx);
    assert!(
        errors(&us)[0].contains("pull of podonly:2b vanished"),
        "{:?}",
        errors(&us)
    );
    assert!(
        w.tunnel.is_none(),
        "no tunnel was open, so no model refresh"
    );
}

#[cfg(unix)]
#[test]
fn a_chat_check_runs_through_the_tunnel_and_counts_as_activity() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "a_chat_check_runs_through_the_tunnel_and_counts_as_activity",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);

    // No tunnel yet: the check opens one.
    w.idle = Some(IdleTracker::new(Duration::from_secs(3600)));
    w.do_cmd(Cmd::Check("podonly:1b".into()));
    let us = drain(&rx);
    assert!(errors(&us).is_empty(), "{:?}", errors(&us));
    assert!(matches!(&us[0], Update::Busy(Some(l)) if l == "Testing podonly:1b"));
    assert!(has_log(&us, "tunnel up on"));
    let check = us.iter().find_map(|u| match u {
        Update::Check(c) => Some(c.clone()),
        _ => None,
    });
    let c = check.expect("a check update");
    assert_eq!(c.model, "podonly:1b");
    assert_eq!(c.reply, "hi");
    assert_eq!(c.tool_call.as_deref(), Some("read_file"));
    assert_eq!(c.streamed_chunks, 2);

    // The check touched the tracker: ten idle minutes later it counts from zero.
    let stats = remote::parse_gpu_stats(idle_gpu());
    let t0 = Instant::now();
    let tracker = w.idle.as_mut().expect("tracker");
    assert_eq!(
        tracker.observe(&stats, t0 + Duration::from_secs(600)),
        Idle::Idle(Duration::ZERO)
    );

    // A second check reuses the live tunnel.
    w.do_cmd(Cmd::Check("podonly:1b".into()));
    let us = drain(&rx);
    assert!(!has_log(&us, "tunnel up on"), "{:?}", logs(&us));
    assert!(kinds(&us).contains(&"Check"));
}

#[cfg(unix)]
#[test]
fn the_guard_command_reports_each_check_and_fails_the_zed_one_without_a_provider() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "the_guard_command_reports_each_check_and_fails_the_zed_one_without_a_provider",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::Guard);
    let us = drain(&rx);
    assert_eq!(
        kinds(&us),
        ["Busy", "Log", "Log", "Tunnel", "Guard", "Busy"]
    );
    let Update::Guard(checks) = &us[4] else {
        panic!("a guard update");
    };
    let failing: Vec<&str> = checks.iter().filter(|c| !c.ok).map(|c| c.name).collect();
    assert!(
        failing.contains(&"Zed sends pod models through the tunnel")
            && failing.contains(&"Every model Zed offers is on the pod"),
        "{failing:?}"
    );
    let tunnel = checks
        .iter()
        .find(|c| c.name == "The tunnel ends at the pod")
        .expect("check");
    assert!(tunnel.ok, "{}", tunnel.detail);
}

#[cfg(unix)]
#[test]
fn zed_settings_are_written_with_a_default_and_removed_with_the_old_default_restored() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "zed_settings_are_written_with_a_default_and_removed_with_the_old_default_restored",
    ) else {
        return;
    };
    let _ollama = rig.pod_ollama();
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    let path = rig.zed_settings();
    std::fs::create_dir_all(path.parent().expect("dir")).expect("zed dir");
    std::fs::write(
        &path,
        "{\n  // keep this comment\n  \"agent\": { \"default_model\": { \"provider\": \"zed.dev\", \"model\": \"claude-x\" } }\n}\n",
    )
    .expect("seed settings");
    w.open_tunnel().expect("tunnel opens");
    drain(&rx);

    w.do_cmd(Cmd::ConfigureZed {
        default_model: Some("podonly:1b".into()),
    });
    let us = drain(&rx);
    assert_eq!(
        kinds(&us),
        ["Busy", "Log", "Zed", "Busy"],
        "{:?}",
        errors(&us)
    );
    assert!(matches!(&us[0], Update::Busy(Some(l)) if l == "Writing Zed settings"));
    let Update::Zed(z) = &us[2] else {
        panic!("a Zed update");
    };
    assert_eq!(z.provider_models, ["podonly:1b"]);
    assert_eq!(
        z.default,
        Some(DefaultModel {
            provider: "offrig".into(),
            model: "podonly:1b".into()
        })
    );
    assert!(z.key_env);
    assert_eq!(
        w.previous_default,
        Some(DefaultModel {
            provider: "zed.dev".into(),
            model: "claude-x".into()
        }),
        "the old default is remembered"
    );
    let text = std::fs::read_to_string(&path).expect("settings");
    assert!(
        text.contains("keep this comment"),
        "comments survive: {text}"
    );
    assert!(
        path.with_file_name("settings.json.offrig.bak").is_file(),
        "backed up once"
    );

    // Writing again with a default does not forget the user's own default.
    w.do_cmd(Cmd::ConfigureZed {
        default_model: Some("podonly:1b".into()),
    });
    drain(&rx);
    assert_eq!(
        w.previous_default.as_ref().map(|d| d.model.as_str()),
        Some("claude-x")
    );

    w.do_cmd(Cmd::RemoveZed);
    let us = drain(&rx);
    assert_eq!(
        kinds(&us),
        ["Busy", "Log", "Zed", "Busy"],
        "{:?}",
        errors(&us)
    );
    assert_eq!(logs(&us), ["offrig's provider removed from Zed"]);
    let Update::Zed(z) = &us[2] else {
        panic!("a Zed update");
    };
    assert!(z.provider_models.is_empty());
    assert_eq!(
        z.default,
        Some(DefaultModel {
            provider: "zed.dev".into(),
            model: "claude-x".into()
        }),
        "the user's default is back"
    );
}

#[cfg(unix)]
#[test]
fn zed_status_reads_the_provider_the_default_and_refuses_malformed_settings() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "zed_status_reads_the_provider_the_default_and_refuses_malformed_settings",
    ) else {
        return;
    };
    let cfg = test_cfg();
    let blank = zed_status(&cfg).expect("no settings file is an empty status");
    assert_eq!(
        blank,
        ZedStatus {
            provider_models: vec![],
            default: None,
            key_env: true
        }
    );

    let path = rig.zed_settings();
    std::fs::create_dir_all(path.parent().expect("dir")).expect("zed dir");
    std::fs::write(
        &path,
        r#"{"language_models":{"openai_compatible":{"offrig":{"api_url":"http://127.0.0.1:1/v1","available_models":[{"name":"a:1b"},{"name":"b:2b"}]}}},
            "agent":{"default_model":{"provider":"offrig","model":"a:1b"}}}"#,
    )
    .expect("settings");
    let st = zed_status(&cfg).expect("status");
    assert_eq!(st.provider_models, ["a:1b", "b:2b"]);
    assert_eq!(
        st.default,
        Some(DefaultModel {
            provider: "offrig".into(),
            model: "a:1b".into()
        })
    );

    std::fs::write(&path, "{ not json").expect("settings");
    assert!(zed_status(&cfg).is_err());
}

#[cfg(unix)]
#[test]
fn open_in_zed_starts_zed_on_the_pods_workspace() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "open_in_zed_starts_zed_on_the_pods_workspace",
    ) else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::OpenZedRemote);
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Log"], "{:?}", errors(&us));
    assert_eq!(logs(&us), ["opened ssh://offrig/workspace in Zed"]);
    let args = rig.dir.join("fake").join("zed_args");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !args.is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        std::fs::read_to_string(&args).expect("zed ran"),
        "ssh://offrig/workspace"
    );
    let config = std::fs::read_to_string(rig.ssh_config()).expect("ssh config written");
    assert!(config.contains("Host offrig\n"), "{config}");
}

#[cfg(unix)]
#[test]
fn open_in_zed_says_so_when_zed_is_not_installed() {
    let Some(rig) = Rig::enter_with(
        module_path!(),
        "open_in_zed_says_so_when_zed_is_not_installed",
        false,
    ) else {
        return;
    };
    let rp = rig.runpod(|r, b, _| runpod_reply(r, b, &format!("[{POD}]")));
    let (mut w, rx) = worker(test_cfg(), &rp);
    w.do_cmd(Cmd::OpenZedRemote);
    let us = drain(&rx);
    assert_eq!(kinds(&us), ["Error"]);
    assert!(
        errors(&us)[0].starts_with("io error while launching zed"),
        "{:?}",
        errors(&us)
    );
}
