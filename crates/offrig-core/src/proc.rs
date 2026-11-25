//! Child-process helpers: no console window on Windows, and a wall-clock timeout,
//! which `std::process` does not provide.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
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
/// `pid` may be treated as stale when combined with `!pid_alive_at(pid, start)`.
pub const LAUNCH_HEARTBEAT_STALE_SECS: i64 = 180;

/// Result of inspecting an OS process `pid` and its recorded start time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessProbe {
    Dead,
    Alive(Option<String>),
    Unknown,
}

/// Parse one RFC-4180 CSV line (`tasklist /FO CSV /NH` output): handles quoted fields,
/// commas inside quotes, `""` escapes, and trailing `\r`.
pub fn parse_csv_row(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.trim_end_matches(['\r', '\n']).chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cur.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            fields.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    fields.push(cur);
    fields
}

/// Probe whether `pid` is live and, when available, return its OS-specific start-time
/// token so PID reuse after a crash is never mistaken for the original launcher.
pub fn process_start_time(pid: u32) -> ProcessProbe {
    if pid == 0 {
        return ProcessProbe::Dead;
    }
    #[cfg(target_os = "linux")]
    {
        let proc_dir = format!("/proc/{pid}");
        if !std::path::Path::new(&proc_dir).exists() {
            return ProcessProbe::Dead;
        }
        match std::fs::read_to_string(format!("{proc_dir}/stat")) {
            Ok(stat) => {
                if let Some(after) = stat.rfind(") ") {
                    let mut fields = stat[after + 2..].split_whitespace();
                    let state = fields.next().and_then(|s| s.chars().next());
                    if matches!(state, Some('Z' | 'X')) {
                        return ProcessProbe::Dead;
                    }
                    // After `) `, field 3 (state) is index 0; field 22 (starttime) is index 19.
                    let start = fields.nth(18).map(str::to_string);
                    ProcessProbe::Alive(start)
                } else {
                    ProcessProbe::Alive(None)
                }
            }
            Err(_) => ProcessProbe::Dead,
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let mut cmd = Command::new("ps");
        cmd.args(["-o", "lstart=", "-p", &pid.to_string()]);
        if let Ok(out) =
            run_with_timeout(&mut cmd, Duration::from_secs(5), "checking process start")
            && out.success()
        {
            let start = out.stdout.trim().to_string();
            if !start.is_empty() {
                return ProcessProbe::Alive(Some(start));
            }
        }
        let mut k = Command::new("kill");
        k.args(["-0", &pid.to_string()]);
        match run_with_timeout(&mut k, Duration::from_secs(5), "checking process liveness") {
            Ok(out) => {
                if out.success()
                    || out.stderr.contains("Operation not permitted")
                    || out.stderr.contains("EPERM")
                {
                    ProcessProbe::Alive(None)
                } else {
                    ProcessProbe::Dead
                }
            }
            Err(_) => ProcessProbe::Unknown,
        }
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("tasklist");
        cmd.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]);
        let listed = match run_with_timeout(
            &mut cmd,
            Duration::from_secs(10),
            "checking process liveness",
        ) {
            Ok(out) => out.stdout.lines().any(|line| {
                let cols = parse_csv_row(line);
                cols.get(1)
                    .and_then(|col| col.trim().parse::<u32>().ok())
                    .is_some_and(|p| p == pid)
            }),
            Err(_) => return ProcessProbe::Unknown,
        };
        if !listed {
            return ProcessProbe::Dead;
        }
        let mut ps = Command::new("powershell");
        ps.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "$p = Get-Process -Id {pid} -ErrorAction SilentlyContinue; \
                 if ($p) {{ $p.StartTime.ToFileTimeUtc() }} else {{ 'DEAD' }}"
            ),
        ]);
        match run_with_timeout(
            &mut ps,
            Duration::from_secs(10),
            "reading process start time",
        ) {
            Ok(out) => {
                let s = out.stdout.trim();
                if s == "DEAD" {
                    ProcessProbe::Dead
                } else if s.parse::<i64>().is_ok() {
                    ProcessProbe::Alive(Some(s.to_string()))
                } else {
                    ProcessProbe::Alive(None)
                }
            }
            Err(_) => ProcessProbe::Alive(None),
        }
    }
}

/// Cached OS start-time token for the current process.
pub fn own_start_time() -> Option<String> {
    static OWN_START: OnceLock<Option<String>> = OnceLock::new();
    OWN_START
        .get_or_init(|| match process_start_time(std::process::id()) {
            ProcessProbe::Alive(s) => s,
            _ => None,
        })
        .clone()
}

/// Check whether `pid` is alive AND matches `recorded_start` (when both are known).
/// A live PID whose OS start time differs from `recorded_start` indicates PID reuse
/// and is treated as dead.
pub fn pid_alive_at(pid: u32, recorded_start: Option<&str>) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return match (recorded_start, own_start_time().as_deref()) {
            (Some(rec), Some(cur)) => rec == cur,
            _ => true,
        };
    }
    match process_start_time(pid) {
        ProcessProbe::Dead => false,
        ProcessProbe::Unknown => true,
        ProcessProbe::Alive(actual) => match (recorded_start, actual.as_deref()) {
            (Some(rec), Some(act)) => rec == act,
            _ => true,
        },
    }
}

/// Best-effort OS check whether process `pid` currently exists and is not dead.
pub fn pid_alive(pid: u32) -> bool {
    pid_alive_at(pid, None)
}

/// True only when `job` recorded a launching `pid` + `heartbeat_at` AND that process
/// is no longer alive (including PID reuse with a different start time) AND its
/// heartbeat is older than [`LAUNCH_HEARTBEAT_STALE_SECS`].
pub fn launch_job_proven_dead(job: &crate::store::Job, now: i64) -> bool {
    let Some(pid) = job.progress["pid"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
    else {
        return false;
    };
    let Some(hb) = job.progress["heartbeat_at"].as_i64() else {
        return false;
    };
    let started_at = job.progress["pid_started_at"].as_str();
    !pid_alive_at(pid, started_at) && now.saturating_sub(hb) > LAUNCH_HEARTBEAT_STALE_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_csv_row_handles_commas_and_escaped_quotes_in_tasklist_output() {
        let row = parse_csv_row(r#""foo,bar"".exe","1234","Console","1","12,345 K""#);
        assert_eq!(
            row,
            vec!["foo,bar\".exe", "1234", "Console", "1", "12,345 K"]
        );
        let info = parse_csv_row("INFO: No tasks are running which match the specified criteria.");
        assert_eq!(info.len(), 1);
    }

    #[test]
    fn current_pid_is_alive_and_exited_child_is_dead() {
        assert!(!pid_alive(0));
        assert!(pid_alive(std::process::id()));
        let own = own_start_time();
        assert!(own.is_some(), "expected OS process start time for self");
        assert!(pid_alive_at(std::process::id(), own.as_deref()));
        assert!(
            !pid_alive_at(std::process::id(), Some("0")),
            "live pid with different recorded start time must be treated as dead"
        );

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
