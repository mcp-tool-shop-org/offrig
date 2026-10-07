//! A stand-in for `ssh`, `scp` and `ssh-keygen`, for tests only. A test copies this
//! binary into a temp directory under those names and puts that directory first on
//! the PATH of the program under test, so nothing ever reaches a real host.
//!
//! Behaviour comes from `FAKE_SSH_DIR`:
//! - every call is appended to `calls.log` as `<kind>|<detail>` (`ssh`, `scp`, `keygen`,
//!   `tunnel`);
//! - `rules.json` is a list of `{"contains", "stdout", "stderr", "code", "limit"}`; the
//!   first rule whose `contains` is in the remote script (or the scp arguments) answers,
//!   and a rule with a `limit` answers that many times and is then skipped;
//! - a tunnel (`ssh -N -L 127.0.0.1:P:127.0.0.1:R alias`) listens on P and relays each
//!   connection to `FAKE_SSH_UPSTREAM` (`host:port`), unless `tunnel_fail` exists in the
//!   directory, in which case it prints an error and exits 255;
//! - with `FAKE_SSH_TUNNEL_LIFETIME_MS` set, a tunnel dies with exit 255 after that long
//!   (a dropped connection);
//! - `echo ok` answers `ok` when no rule matches, so a readiness probe passes.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    PathBuf::from(std::env::var_os("FAKE_SSH_DIR").expect("FAKE_SSH_DIR is set by the test"))
}

fn log(dir: &Path, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("calls.log"))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// The first rule matching `text`, honouring `limit`.
fn answer(dir: &Path, text: &str) -> Option<(String, String, i32)> {
    let raw = std::fs::read_to_string(dir.join("rules.json")).ok()?;
    let rules: Vec<serde_json::Value> = serde_json::from_str(&raw).ok()?;
    for (i, r) in rules.iter().enumerate() {
        if !text.contains(r["contains"].as_str().unwrap_or("\u{0}")) {
            continue;
        }
        if let Some(limit) = r["limit"].as_u64() {
            let counter = dir.join(format!("rule-{i}.count"));
            let used: u64 = std::fs::read_to_string(&counter)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            if used >= limit {
                continue;
            }
            let _ = std::fs::write(&counter, (used + 1).to_string());
        }
        return Some((
            r["stdout"].as_str().unwrap_or("").to_string(),
            r["stderr"].as_str().unwrap_or("").to_string(),
            r["code"].as_i64().unwrap_or(0) as i32,
        ));
    }
    None
}

fn relay(mut a: TcpStream, upstream: &str) {
    let Ok(mut b) = TcpStream::connect(upstream) else {
        return;
    };
    let (Ok(mut a2), Ok(mut b2)) = (a.try_clone(), b.try_clone()) else {
        return;
    };
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a2, &mut b2);
        let _ = b2.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut b, &mut a);
    let _ = a.shutdown(std::net::Shutdown::Write);
    let _ = up.join();
}

fn tunnel(dir: &Path, spec: &str) -> i32 {
    log(dir, &format!("tunnel|{spec}"));
    if dir.join("tunnel_fail").exists() {
        eprintln!("fake ssh: connect failed");
        return 255;
    }
    let port = spec.split(':').nth(1).unwrap_or("0");
    let Ok(listener) = TcpListener::bind(format!("127.0.0.1:{port}")) else {
        eprintln!("fake ssh: bind failed");
        return 255;
    };
    if let Some(ms) = std::env::var("FAKE_SSH_TUNNEL_LIFETIME_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            eprintln!("fake ssh: connection dropped");
            std::process::exit(255);
        });
    }
    let upstream = std::env::var("FAKE_SSH_UPSTREAM").unwrap_or_default();
    for stream in listener.incoming().map_while(Result::ok) {
        if upstream.is_empty() {
            continue;
        }
        let up = upstream.clone();
        std::thread::spawn(move || relay(stream, &up));
    }
    0
}

fn main() {
    let dir = dir();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let me = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let code = if me.starts_with("ssh-keygen") {
        log(&dir, &format!("keygen|{}", args.join(" ")));
        0
    } else if me.starts_with("scp") {
        let joined = args.join(" ");
        log(&dir, &format!("scp|{joined}"));
        let (out, err, code) = answer(&dir, &joined).unwrap_or_default();
        // A download names the pod path first and the local file last.
        let download = args.len() >= 2 && args[args.len() - 2].contains(':');
        if code == 0
            && download
            && let Some(local) = args.last()
        {
            let _ = std::fs::write(local, "fake download");
        }
        print!("{out}");
        eprint!("{err}");
        code
    } else if let Some(at) = args.iter().position(|a| a == "-L") {
        tunnel(&dir, args.get(at + 1).map_or("", String::as_str))
    } else {
        let script = args.last().cloned().unwrap_or_default();
        log(&dir, &format!("ssh|{script}"));
        match answer(&dir, &script) {
            Some((out, err, code)) => {
                print!("{out}");
                eprint!("{err}");
                code
            }
            None if script.trim() == "echo ok" => {
                println!("ok");
                0
            }
            None => 0,
        }
    };
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}
