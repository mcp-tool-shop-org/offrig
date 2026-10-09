//! The CLI's contract: exit codes (0 ok, 1 user error, 2 runtime error), the output
//! levels, and that the RunPod key never reaches the terminal. Runs the real binary
//! against a mock RunPod on localhost; no network.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Output};

const KEY: &str = "rpa_TESTSECRET_0123456789";

/// A server that answers every request with `(status, body)`.
fn mock(status: u16, body: String) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            let mut reader = BufReader::new(stream);
            let mut len = 0usize;
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                if line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
                line.clear();
            }
            let mut sink = vec![0u8; len];
            let _ = reader.read_exact(&mut sink);
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = reader.get_mut().write_all(resp.as_bytes());
        }
    });
    url
}

fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("offrig-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// Run `offrig <args>` with its config in a temp dir, the key (if any) and the mock.
fn offrig(name: &str, key: Option<&str>, base: Option<&str>, args: &[&str]) -> Output {
    let dir = temp(name);
    let mut c = Command::new(env!("CARGO_BIN_EXE_offrig"));
    c.args(args)
        .current_dir(&dir)
        .env("OFFRIG_CONFIG_DIR", dir.join("cfg"))
        .env_remove("RUNPOD_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("RUST_BACKTRACE");
    if let Some(k) = key {
        c.env("RUNPOD_API_KEY", k);
    }
    if let Some(b) = base {
        c.env("OFFRIG_TEST_RUNPOD_BASE", b);
    }
    c.output().expect("run offrig")
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

#[test]
fn help_and_version_exit_zero() {
    for a in [["--help"], ["--version"]] {
        let o = offrig("help", None, None, &a);
        assert_eq!(o.status.code(), Some(0), "{a:?}");
    }
    let (out, _) = text(&offrig("help2", None, None, &["--help"]));
    for flag in ["--quiet", "--verbose", "--debug"] {
        assert!(out.contains(flag), "{flag} in --help");
    }
}

#[test]
fn bad_arguments_are_a_user_error() {
    for a in [
        vec!["--no-such-flag"],
        vec!["no-such-command"],
        vec!["gpus", "--count", "many"],
        vec!["-q", "-v", "profiles"],
        vec!["-v", "--debug", "profiles"],
    ] {
        let o = offrig("badargs", None, None, &a);
        assert_eq!(o.status.code(), Some(1), "{a:?}: {}", text(&o).1);
    }
}

#[test]
fn a_missing_key_is_a_user_error_with_a_plain_message() {
    let o = offrig("nokey", None, None, &["status"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("RUNPOD_API_KEY is not set"), "{err}");
    assert!(!err.contains("stack backtrace"), "{err}");
    assert!(!err.contains("Caused by"), "plain mode is one line: {err}");
}

#[test]
fn unknown_profiles_and_refusals_are_user_errors() {
    let o = offrig("noprofile", Some(KEY), None, &["tunnel", "no-such-profile"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o).1);
    let o = offrig("noyes", Some(KEY), None, &["stage", "no-such-profile"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o).1);
}

#[test]
fn a_runpod_failure_is_a_runtime_error() {
    let base = mock(503, "{\"error\":\"unavailable\"}".into());
    let o = offrig("runtime", Some(KEY), Some(&base), &["status"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(2), "{err}");
    assert!(err.contains("503"), "{err}");
}

#[test]
fn a_command_that_works_exits_zero_and_quiet_keeps_its_result() {
    let o = offrig("profiles", None, None, &["profiles"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    let normal = text(&o).0;
    assert!(!normal.is_empty());
    let q = offrig("profiles-q", None, None, &["-q", "profiles"]);
    assert_eq!(q.status.code(), Some(0));
    assert_eq!(
        text(&q).0,
        normal,
        "-q keeps the data a command exists to print"
    );
}

#[test]
fn quiet_drops_confirmations_but_not_errors() {
    let o = offrig("budget", None, None, &["budget", "12.5"]);
    assert!(text(&o).0.contains("budget cap set"), "{}", text(&o).0);
    let q = offrig("budget-q", None, None, &["-q", "budget", "12.5"]);
    assert_eq!(q.status.code(), Some(0));
    assert!(!text(&q).0.contains("budget cap set"), "{}", text(&q).0);
    // An error still prints under -q.
    let e = offrig("nokey-q", None, None, &["-q", "status"]);
    assert_eq!(e.status.code(), Some(1));
    assert!(text(&e).1.contains("RUNPOD_API_KEY is not set"));
}

#[test]
fn verbose_names_the_api_calls_and_debug_shows_the_chain() {
    let base = mock(
        200,
        "{\"data\":{\"myself\":{\"clientBalance\":9.0,\"currentSpendPerHr\":0.0}}}".into(),
    );
    let plain = offrig("v0", Some(KEY), Some(&base), &["status"]);
    assert!(!text(&plain).1.contains("runpod:"), "{}", text(&plain).1);
    let v = offrig("v1", Some(KEY), Some(&base), &["-v", "status"]);
    let (_, err) = text(&v);
    assert!(err.contains("[verbose]"), "{err}");
    assert!(err.contains("runpod:") && err.contains(" ms"), "{err}");

    let bad = mock(503, "{\"error\":\"unavailable\"}".into());
    let d = offrig("v2", Some(KEY), Some(&bad), &["--debug", "status"]);
    let (_, err) = text(&d);
    assert_eq!(d.status.code(), Some(2), "{err}");
    assert!(err.contains("[debug]"), "response body at debug: {err}");
    assert!(
        err.contains("runpod_api") && err.contains("exit 2"),
        "{err}"
    );
}

#[test]
fn the_api_key_never_reaches_the_terminal() {
    // A server that echoes the key back in its error body and as a Bearer value.
    let body = format!("{{\"error\":\"bad token {KEY}\",\"echo\":\"Bearer {KEY}\"}}");
    let base = mock(401, body);
    for args in [
        vec!["status"],
        vec!["-v", "status"],
        vec!["--debug", "status"],
        vec!["-q", "status"],
    ] {
        let o = offrig("redact", Some(KEY), Some(&base), &args);
        let (out, err) = text(&o);
        assert!(!out.contains(KEY) && !err.contains(KEY), "{args:?}: {err}");
        assert!(!err.contains("TESTSECRET"), "{args:?}: {err}");
        assert_eq!(o.status.code(), Some(2), "{args:?}: {err}");
    }
    let o = offrig("redact2", Some(KEY), Some(&base), &["--debug", "status"]);
    assert!(text(&o).1.contains("[redacted]"), "{}", text(&o).1);
}
