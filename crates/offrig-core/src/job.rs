//! Work on a job pod: files up, a command run on the pod, files back. A command runs
//! detached on the pod (`setsid nohup`), so it survives this app, the side-car or the
//! ssh session closing; its log and exit status stay in `/workspace/offrig/jobs/`.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::proc;
use crate::remote::ssh_exec;
use crate::spec::{JOB_DIR, STATE_DIR};

/// A copy of a few GB (a checkpoint) over a slow link can take a while.
pub const COPY_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const SSH_TIMEOUT: Duration = Duration::from_secs(60);

/// A job name: letters, digits, `-` and `_`, at most 40. It names the log on the pod.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Error::Refused(format!(
            "job name {name:?} must be 1-40 letters, digits, - or _"
        )))
    }
}

/// A pod path: relative ones are under the job directory. No quotes, newlines or
/// shell metacharacters, so it can be passed to scp and the remote shell as is.
pub fn pod_path(path: &str) -> Result<String> {
    let path = path.trim();
    let bad = path.is_empty()
        || path.chars().any(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '\'' | '"' | '`' | '$' | ';' | '&' | '|' | '<' | '>' | '\\' | '*' | '?'
                )
        })
        || path.split('/').any(|seg| seg == "..");
    if bad {
        return Err(Error::Refused(format!(
            "pod path {path:?} must be a plain path with no spaces, quotes, globs or .."
        )));
    }
    Ok(if path.starts_with('/') {
        path.to_string()
    } else {
        format!("{JOB_DIR}/{path}")
    })
}

/// Standard base64 with padding: what `base64 -d` on the pod reads.
pub fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn jobs_dir() -> String {
    format!("{STATE_DIR}/jobs")
}

/// The shell that starts a job: it writes the command to a script (sent base64, so
/// nothing in it is interpreted by the ssh shell), then runs it detached in the job
/// directory, recording the exit status when it ends.
pub fn start_script(name: &str, command: &str) -> Result<String> {
    validate_name(name)?;
    if command.trim().is_empty() {
        return Err(Error::Refused("a job needs a command".into()));
    }
    let b64 = base64(command.as_bytes());
    let d = jobs_dir();
    Ok(format!(
        "set -e; mkdir -p {d} {JOB_DIR}; \
         if [ -f {d}/{name}.pid ] && kill -0 \"$(cat {d}/{name}.pid)\" 2>/dev/null; then echo running; exit 0; fi; \
         echo {b64} | base64 -d > {d}/{name}.sh; rm -f {d}/{name}.exit; \
         cd {JOB_DIR}; \
         setsid nohup bash -c 'bash {d}/{name}.sh; echo $? > {d}/{name}.exit' > {d}/{name}.log 2>&1 < /dev/null & \
         echo $! > {d}/{name}.pid; echo started"
    ))
}

/// Start a job. Starting one whose name is still running is a no-op that says so.
pub fn start(alias: &str, name: &str, command: &str) -> Result<bool> {
    let out = ssh_exec(alias, &start_script(name, command)?, SSH_TIMEOUT)?;
    Ok(out.trim() == "started")
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    NotStarted,
    Running,
    Exited { code: i32 },
}

#[derive(Debug, Clone, Serialize)]
pub struct JobStatus {
    #[serde(flatten)]
    pub state: JobState,
    /// The last lines of the job's log.
    pub log_tail: String,
}

/// What the status script prints: one header line, then the log tail.
pub fn status_script(name: &str, tail_lines: u32) -> Result<String> {
    validate_name(name)?;
    let d = jobs_dir();
    Ok(format!(
        "if [ -f {d}/{name}.exit ]; then echo \"exited $(cat {d}/{name}.exit)\"; \
         elif [ -f {d}/{name}.pid ] && kill -0 \"$(cat {d}/{name}.pid)\" 2>/dev/null; then echo running; \
         elif [ -f {d}/{name}.pid ]; then echo 'exited -1'; \
         else echo not_started; fi; \
         tail -n {tail_lines} {d}/{name}.log 2>/dev/null || true"
    ))
}

pub fn parse_status(out: &str) -> JobStatus {
    let (head, tail) = out.split_once('\n').unwrap_or((out, ""));
    let head = head.trim();
    let state = if head == "running" {
        JobState::Running
    } else if let Some(code) = head.strip_prefix("exited ") {
        // A job killed before it could write its status (the pod restarted, or it was
        // killed) has a pid and no exit file: reported as -1.
        JobState::Exited {
            code: code.trim().parse().unwrap_or(-1),
        }
    } else {
        JobState::NotStarted
    };
    JobStatus {
        state,
        log_tail: tail.to_string(),
    }
}

pub fn status(alias: &str, name: &str, tail_lines: u32) -> Result<JobStatus> {
    let out = ssh_exec(alias, &status_script(name, tail_lines)?, SSH_TIMEOUT)?;
    Ok(parse_status(&out))
}

/// Stop a running job and everything it started.
pub fn stop(alias: &str, name: &str) -> Result<()> {
    validate_name(name)?;
    let d = jobs_dir();
    ssh_exec(
        alias,
        &format!(
            "if [ -f {d}/{name}.pid ]; then kill -TERM -- -\"$(cat {d}/{name}.pid)\" 2>/dev/null || true; fi; echo ok"
        ),
        SSH_TIMEOUT,
    )
    .map(|_| ())
}

/// The scp arguments for one copy. scp runs in the local file's parent directory and
/// names it by file name, so a Windows drive letter (`E:`) is never read as a host.
pub fn scp_args(alias: &str, local_name: &str, remote: &str, upload: bool) -> Vec<String> {
    let mut a = vec![
        "-r".to_string(),
        "-q".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=15".to_string(),
    ];
    let pod = format!("{alias}:{remote}");
    if upload {
        a.push(local_name.to_string());
        a.push(pod);
    } else {
        a.push(pod);
        a.push(local_name.to_string());
    }
    a
}

fn split_local(local: &Path) -> Result<(std::path::PathBuf, String)> {
    let abs = if local.is_absolute() {
        local.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| Error::io("reading the current directory", e))?
            .join(local)
    };
    let name = abs
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Refused(format!("{} has no file name", abs.display())))?
        .to_string();
    let parent = abs
        .parent()
        .ok_or_else(|| Error::Refused(format!("{} has no parent directory", abs.display())))?
        .to_path_buf();
    Ok((parent, name))
}

fn scp(alias: &str, local: &Path, remote: &str, upload: bool) -> Result<String> {
    let remote = pod_path(remote)?;
    let (dir, name) = split_local(local)?;
    if upload && !dir.join(&name).exists() {
        return Err(Error::Refused(format!(
            "{} does not exist",
            local.display()
        )));
    }
    if !dir.is_dir() {
        return Err(Error::Refused(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    if upload {
        // scp does not create the remote parent.
        let parent = remote.rsplit_once('/').map_or("/", |(p, _)| p);
        if !parent.is_empty() {
            ssh_exec(alias, &format!("mkdir -p {parent}"), SSH_TIMEOUT)?;
        }
    }
    let mut cmd = Command::new("scp");
    cmd.current_dir(&dir)
        .args(scp_args(alias, &name, &remote, upload));
    let what = if upload { "scp up" } else { "scp down" };
    let out = proc::run_with_timeout(&mut cmd, COPY_TIMEOUT, what)?;
    if out.success() {
        Ok(remote)
    } else {
        let msg = out.stderr.trim();
        Err(Error::Ssh(format!(
            "{what}: {}",
            if msg.is_empty() {
                format!("exit {:?}", out.status)
            } else {
                msg.to_string()
            }
        )))
    }
}

/// Copy a local file or directory to the pod. Returns the pod path written.
pub fn put(alias: &str, local: &Path, remote: &str) -> Result<String> {
    scp(alias, local, remote, true)
}

/// Copy a pod file or directory here.
pub fn get(alias: &str, remote: &str, local: &Path) -> Result<String> {
    scp(alias, local, remote, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn names_and_paths_are_plain() {
        assert!(validate_name("train-a_1").is_ok());
        for bad in ["", "a b", "a;b", "../x", &"x".repeat(41)] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            pod_path("out/geo.json").expect("ok"),
            "/workspace/job/out/geo.json"
        );
        assert_eq!(pod_path("/workspace/hf").expect("ok"), "/workspace/hf");
        for bad in ["", "a b", "x;rm", "$(id)", "a/../b", "q'x", "*.json"] {
            assert!(pod_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_command_reaches_the_pod_only_as_base64() {
        let cmd = "python train.py --x 'quoted' && echo $HOME; rm -rf nothing";
        let s = start_script("run-a", cmd).expect("script");
        assert!(
            !s.contains("train.py"),
            "the command is never in the ssh line"
        );
        let b64 = base64(cmd.as_bytes());
        assert!(s.contains(&b64));
        assert!(s.contains("setsid nohup"), "detached, so it outlives ssh");
        assert!(s.contains("echo $? > /workspace/offrig/jobs/run-a.exit"));
        assert!(s.find("kill -0").expect("guard") < s.find("base64 -d").expect("write"));
        assert!(start_script("run-a", "  ").is_err());
        assert!(start_script("bad name", "true").is_err());
    }

    #[test]
    fn status_reads_running_exited_and_lost_jobs() {
        let s = parse_status("running\nepoch 1\nepoch 2\n");
        assert_eq!(s.state, JobState::Running);
        assert_eq!(s.log_tail, "epoch 1\nepoch 2\n");
        assert_eq!(
            parse_status("exited 0\n").state,
            JobState::Exited { code: 0 }
        );
        assert_eq!(
            parse_status("exited 2\nTraceback").state,
            JobState::Exited { code: 2 }
        );
        assert_eq!(
            parse_status("exited -1\n").state,
            JobState::Exited { code: -1 }
        );
        assert_eq!(parse_status("not_started\n").state, JobState::NotStarted);
        let script = status_script("run-a", 20).expect("script");
        assert!(script.contains("tail -n 20 /workspace/offrig/jobs/run-a.log"));
    }

    #[test]
    fn scp_names_the_local_file_relative_so_a_drive_letter_is_not_a_host() {
        let up = scp_args(
            "offrig",
            "prompts.json",
            "/workspace/job/prompts.json",
            true,
        );
        assert_eq!(
            &up[up.len() - 2..],
            ["prompts.json", "offrig:/workspace/job/prompts.json"]
        );
        assert!(up.contains(&"BatchMode=yes".to_string()));
        let down = scp_args("offrig", "out", "/workspace/job/out", false);
        assert_eq!(
            &down[down.len() - 2..],
            ["offrig:/workspace/job/out", "out"]
        );
        let (dir, name) = split_local(Path::new("E:/AI/x/geometry.json")).expect("split");
        assert_eq!(name, "geometry.json");
        assert_eq!(dir, Path::new("E:/AI/x"));
    }
}
