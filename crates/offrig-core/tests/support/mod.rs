//! A tiny HTTP server standing in for RunPod or a pod's Ollama, plus a temp dir
//! helper. Shared by the integration tests of offrig-core.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

/// One request the mock received.
#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    /// Path with the query string.
    pub target: String,
    /// Lower-cased header name, value.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Req {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }

    pub fn route(&self) -> String {
        format!("{} {}", self.method, self.path())
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == &name.to_ascii_lowercase())
            .map(|(_, v)| v.as_str())
    }
}

pub struct Mock {
    pub url: String,
    reqs: Arc<Mutex<Vec<Req>>>,
}

impl Mock {
    pub fn requests(&self) -> Vec<Req> {
        self.reqs.lock().expect("requests lock").clone()
    }

    pub fn count(&self, route: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.route() == route)
            .count()
    }

    pub fn last(&self) -> Req {
        self.requests().pop().expect("at least one request")
    }
}

/// Serve `handler(request, nth)` where `nth` counts earlier hits on the same route.
/// The reply is `(status, body)`, sent as JSON with the connection closed.
pub fn serve(handler: impl Fn(&Req, usize) -> (u16, String) + Send + 'static) -> Mock {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let reqs = Arc::new(Mutex::new(Vec::<Req>::new()));
    let log = Arc::clone(&reqs);
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(std::result::Result::ok) {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let target = parts.next().unwrap_or("").to_string();
            let mut len = 0usize;
            let mut headers = Vec::new();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
                    if k == "content-length" {
                        len = v.parse().unwrap_or(0);
                    }
                    headers.push((k, v));
                }
            }
            let mut body = vec![0u8; len];
            let _ = reader.read_exact(&mut body);
            let req = Req {
                method,
                target,
                headers,
                body: String::from_utf8_lossy(&body).to_string(),
            };
            let route = req.route();
            let nth = {
                let mut l = log.lock().expect("requests lock");
                let n = l.iter().filter(|r| r.route() == route).count();
                l.push(req.clone());
                n
            };
            let (status, text) = handler(&req, nth);
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = reader.get_mut().write_all(resp.as_bytes());
        }
    });
    Mock { url, reqs }
}

/// A URL nothing listens on.
pub fn dead_url() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    drop(l);
    format!("http://{addr}")
}

pub fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("offrig-core-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// Re-run the calling test binary for the single test `name` as a child process with
/// `OFFRIG_CHILD=1`, the given variables set and `remove` unset, and assert it passed.
/// A test cannot change its own process environment (no `unsafe`), so a case that
/// needs particular variables checks `OFFRIG_CHILD` and does its asserts in the child.
pub fn reexec(name: &str, vars: &[(&str, &str)], remove: &[&str]) {
    let exe = std::env::current_exe().expect("current exe");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["--exact", name, "--test-threads=1"])
        .env("OFFRIG_CHILD", "1");
    for (k, v) in vars {
        cmd.env(k, v);
    }
    for k in remove {
        cmd.env_remove(k);
    }
    let out = cmd.output().expect("run the child test");
    assert!(
        out.status.success(),
        "child run of {name} failed:
{}
{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("1 passed"),
        "child ran no test:
{stdout}"
    );
}
