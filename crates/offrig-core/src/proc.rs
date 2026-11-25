//! Child-process helpers: no console window on Windows, and a wall-clock timeout,
//! which `std::process` does not provide.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Keep a GUI app from flashing a console window for every ssh call.
pub fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// Run `cmd` to completion or kill it at `timeout`.
pub fn run_with_timeout(cmd: &mut Command, timeout: Duration, what: &str) -> Result<Output> {
    let mut child = quiet(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::io(format!("starting {what}"), e))?;
    let out_reader = drain(child.stdout.take());
    let err_reader = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code(),
            Ok(None) if Instant::now() >= deadline => {
                kill(&mut child);
                return Err(Error::Timeout(format!("{what} ({}s)", timeout.as_secs())));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(Error::io(format!("waiting for {what}"), e)),
        }
    };
    Ok(Output {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    })
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut p) = pipe {
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            s = String::from_utf8_lossy(&buf).into_owned();
        }
        s
    })
}

pub fn kill(child: &mut Child) {
    // Already-exited children make kill fail; the wait reaps either way.
    let _ = child.kill();
    let _ = child.wait();
}

/// Seconds without a launch heartbeat before a non-responsive launch job's
/// `pid` may be treated as stale when combined with `!pid_alive(pid)`.
pub const LAUNCH_HEARTBEAT_STALE_SECS: i64 = 180;

/// Best-effort OS check whether process `pid` currently exists and is not dead.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        let proc_dir = format!("/proc/{pid}");
        if !std::path::Path::new(&proc_dir).exists() {
            return false;
        }
        match std::fs::read_to_string(format!("{proc_dir}/stat")) {
            Ok(stat) => {
                if let Some(after) = stat.rfind(") ") {
                    let state = stat[after + 2..].chars().next();
                    !matches!(state, Some('Z' | 'X'))
                } else {
                    true
                }
            }
            Err(_) => false,
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let mut cmd = Command::new("kill");
        cmd.args(["-0", &pid.to_string()]);
        match run_with_timeout(
            &mut cmd,
            Duration::from_secs(5),
            "checking process liveness",
        ) {
            Ok(out) => {
                out.success()
                    || out.stderr.contains("Operation not permitted")
                    || out.stderr.contains("EPERM")
            }
            Err(_) => true,
        }
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("tasklist");
        cmd.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]);
        match run_with_timeout(
            &mut cmd,
            Duration::from_secs(10),
            "checking process liveness",
        ) {
            Ok(out) => out.stdout.lines().any(|line| {
                line.split(',')
                    .nth(1)
                    .is_some_and(|col| col.trim_matches(['"', ' ', '\r']).parse::<u32>() == Ok(pid))
            }),
            Err(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_pid_is_alive_and_exited_child_is_dead() {
        assert!(!pid_alive(0));
        assert!(pid_alive(std::process::id()));

        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/c", "exit", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn short child");
        #[cfg(unix)]
        let mut child = Command::new("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn short child");

        let child_pid = child.id();
        let status = child.wait().expect("wait child");
        assert!(status.success());
        assert!(!pid_alive(child_pid), "reaped pid {child_pid} must be dead");
    }
}
