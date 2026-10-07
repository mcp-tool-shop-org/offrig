//! Stand-ins for `ssh` and `zed`, for the worker's tests. The tests copy this
//! binary into a temp directory as `ssh` (and `zed`) and put that directory first
//! on `PATH`, so no test ever reaches a real pod, a real ssh or a real Zed.
//!
//! Which tool it plays comes from its own file name. What it answers comes from
//! files in the directory named by `FAKE_SSH_DIR`, so a test can change the
//! answers between calls:
//!
//! - `fail`: when present, every ssh call fails with that text on stderr.
//! - `tunnel_fail`: when present, the tunnel (`-L`) exits at once with that text.
//! - `upstream`: `host:port` the tunnel forwards to (the test's mock Ollama).
//! - `gpu`: what the GPU query prints.
//! - `models`: what the pod's `/v1/models` query prints.
//! - `pull`: answers to successive pull-state polls, separated by a `===` line;
//!   the last one repeats. `pull_n` counts the polls.
//! - `calls`: every remote script ssh was asked to run, one per line.
//! - `zed_args`: the arguments `zed` was started with.

use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;

fn dir() -> PathBuf {
    PathBuf::from(std::env::var_os("FAKE_SSH_DIR").unwrap_or_default())
}

fn read(name: &str) -> Option<String> {
    std::fs::read_to_string(dir().join(name)).ok()
}

fn main() -> ExitCode {
    // The name it was started under (a link or a copy named `ssh` or `zed`).
    let me = std::env::args()
        .next()
        .and_then(|a| {
            std::path::Path::new(&a)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if me == "zed" {
        let _ = std::fs::write(dir().join("zed_args"), args.join("\n"));
        return ExitCode::SUCCESS;
    }
    if let Some(at) = args.iter().position(|a| a == "-L") {
        return tunnel(args.get(at + 1).map_or("", String::as_str));
    }
    remote(args.last().map_or("", String::as_str))
}

fn fail(text: &str) -> ExitCode {
    eprintln!("{}", text.trim());
    ExitCode::from(255)
}

/// `ssh -L 127.0.0.1:<port>:127.0.0.1:11434 alias`: listen on the port and carry
/// every connection to the upstream the test named.
fn tunnel(spec: &str) -> ExitCode {
    if let Some(why) = read("tunnel_fail") {
        let code = fail(&why);
        // Let the parent read the message before it sees the process gone.
        std::thread::sleep(std::time::Duration::from_millis(300));
        return code;
    }
    let port = spec.split(':').nth(1).unwrap_or("0");
    let Some(upstream) = read("upstream") else {
        return fail("fake ssh: no upstream");
    };
    let Ok(listener) = TcpListener::bind(format!("127.0.0.1:{port}")) else {
        return fail("fake ssh: cannot listen");
    };
    for client in listener.incoming().map_while(Result::ok) {
        let upstream = upstream.trim().to_string();
        std::thread::spawn(move || {
            let Ok(server) = TcpStream::connect(&upstream) else {
                return;
            };
            let (Ok(mut c_in), Ok(mut s_in)) = (client.try_clone(), server.try_clone()) else {
                return;
            };
            let (mut c_out, mut s_out) = (client, server);
            let up = std::thread::spawn(move || {
                let _ = std::io::copy(&mut c_in, &mut s_out);
                let _ = s_out.shutdown(Shutdown::Write);
            });
            let _ = std::io::copy(&mut s_in, &mut c_out);
            let _ = c_out.shutdown(Shutdown::Write);
            let _ = up.join();
        });
    }
    ExitCode::SUCCESS
}

fn remote(script: &str) -> ExitCode {
    if let Some(why) = read("fail") {
        return fail(&why);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir().join("calls"))
    {
        let _ = writeln!(f, "{}", script.replace('\n', " "));
    }
    let out = if script == "echo ok" {
        "ok\n".to_string()
    } else if script.starts_with("nvidia-smi --query-gpu") {
        read("gpu").unwrap_or_default()
    } else if script.contains("/v1/models") {
        read("models").unwrap_or_default()
    } else if script.contains("nohup curl") {
        "started\n".to_string()
    } else if script.contains("NOFILE") {
        pull_answer()
    } else {
        return fail("fake ssh: unexpected script");
    };
    let _ = std::io::stdout().write_all(out.as_bytes());
    ExitCode::SUCCESS
}

fn pull_answer() -> String {
    let all = read("pull").unwrap_or_else(|| "NOFILE\n".into());
    let answers: Vec<&str> = all.split("\n===\n").collect();
    let n: usize = read("pull_n")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let _ = std::fs::write(dir().join("pull_n"), (n + 1).to_string());
    format!("{}\n", answers[n.min(answers.len() - 1)].trim_end())
}
