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
    // An ssh session does not inherit the container's environment, so the job pod's
    // variables (Hugging Face on the volume, the job directory) are set here. Without
    // them the first live run downloaded 40 GB onto the 60 GB container disk and filled it.
    let env = crate::spec::job_env()
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(format!(
        "set -e; mkdir -p {d} {JOB_DIR}; \
         if [ -f {d}/{name}.pid ] && kill -0 \"$(cat {d}/{name}.pid)\" 2>/dev/null; then echo running; exit 0; fi; \
         echo {b64} | base64 -d > {d}/{name}.sh; rm -f {d}/{name}.exit; \
         export {env}; \
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
        log_tail: collapse_progress(tail),
    }
}

/// A log tail with progress bars collapsed. Tools such as tqdm redraw one line with
/// carriage returns, so a single "line" of the log can hold hundreds of frames. Each line
/// keeps only what it showed last, and a `\r\n` ending is just a line ending.
pub fn collapse_progress(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let line = line.trim_end_matches('\r');
        out.push_str(line.rsplit('\r').find(|s| !s.is_empty()).unwrap_or(""));
    }
    out
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

/// A synchronous run is for quick checks (`ls`, `nvidia-smi`), not work: it is capped.
pub const RUN_DEFAULT_SECS: u32 = 30;
pub const RUN_MAX_SECS: u32 = 120;
/// Output kept from each of stdout and stderr of a synchronous run.
pub const RUN_OUTPUT_CAP: usize = 64 * 1024;

/// The timeout of a synchronous run: the default when none is asked, else the request,
/// never above the cap and never zero.
pub fn run_timeout(requested: Option<u32>) -> u32 {
    requested.unwrap_or(RUN_DEFAULT_SECS).clamp(1, RUN_MAX_SECS)
}

/// The shell for a synchronous run. The command arrives base64, as for a background
/// job, is written to a temporary script and run in the job directory under `timeout`
/// (stdin closed, so a command that reads it ends instead of hanging). The exit status
/// is the command's own, 124 when `timeout` ended it.
pub fn run_script(command: &str, timeout_secs: u32) -> Result<String> {
    if command.trim().is_empty() {
        return Err(Error::Refused("a run needs a command".into()));
    }
    let b64 = base64(command.as_bytes());
    let env = crate::spec::job_env()
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    let t = run_timeout(Some(timeout_secs));
    Ok(format!(
        "f=$(mktemp /tmp/offrig-run.XXXXXX) || exit 125; \
         echo {b64} | base64 -d > \"$f\"; \
         export {env}; mkdir -p {JOB_DIR}; cd {JOB_DIR}; \
         timeout -k 5 {t} bash \"$f\" < /dev/null; rc=$?; rm -f \"$f\"; exit $rc"
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunResult {
    pub exit_code: Option<i32>,
    /// `timeout` ended the command at the limit (exit status 124, or 137 if it had to
    /// be killed).
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    /// Output was cut to the last `RUN_OUTPUT_CAP` bytes of each stream.
    pub truncated: bool,
    pub timeout_secs: u32,
}

/// The last `cap` bytes of `s`, on a character boundary, with progress bars collapsed.
fn keep_tail(s: &str, cap: usize) -> (String, bool) {
    let s = collapse_progress(s);
    if s.len() <= cap {
        return (s, false);
    }
    let mut start = s.len() - cap;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    (s[start..].to_string(), true)
}

pub fn finish_run(status: Option<i32>, stdout: &str, stderr: &str, timeout_secs: u32) -> RunResult {
    let (stdout, t1) = keep_tail(stdout, RUN_OUTPUT_CAP);
    let (stderr, t2) = keep_tail(stderr, RUN_OUTPUT_CAP);
    RunResult {
        exit_code: status,
        timed_out: matches!(status, Some(124 | 137)),
        stdout,
        stderr,
        truncated: t1 || t2,
        timeout_secs,
    }
}

/// Run one short command on the pod and wait for it: stdout, stderr and the exit code.
/// ssh itself exits 255 when it cannot connect, which is indistinguishable by status
/// from a command that exits 255, so the stderr is returned for the caller to read.
pub fn run(alias: &str, command: &str, timeout_secs: Option<u32>) -> Result<RunResult> {
    let t = run_timeout(timeout_secs);
    let script = run_script(command, t)?;
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        alias,
        &script,
    ]);
    // The remote `timeout` ends the command; this outer limit only covers a hung ssh.
    let out = proc::run_with_timeout(
        &mut cmd,
        Duration::from_secs(u64::from(t) + 30),
        &format!("ssh {alias}"),
    )?;
    Ok(finish_run(out.status, &out.stdout, &out.stderr, t))
}

/// The pod path of a job's whole log.
pub fn log_path(name: &str) -> Result<String> {
    validate_name(name)?;
    Ok(format!("{}/{name}.log", jobs_dir()))
}

/// Copy a job's whole log, progress bars and all, to a local file. Returns its size.
pub fn fetch_log(alias: &str, name: &str, local: &Path) -> Result<u64> {
    get(alias, &log_path(name)?, local)?;
    std::fs::metadata(local)
        .map(|m| m.len())
        .map_err(|e| Error::io(format!("reading {}", local.display()), e))
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

/// Create `dir` and its parents. A path that exists but is not a folder is refused.
fn ensure_local_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if dir.exists() {
        return Err(Error::Refused(format!(
            "{} exists and is not a directory",
            dir.display()
        )));
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::io(format!("creating {}", dir.display()), e))
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
    if upload {
        if !dir.is_dir() {
            return Err(Error::Refused(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
    } else {
        // A fetch creates the local parent folders, as a put does on the pod side.
        ensure_local_dir(&dir)?;
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
        let export = s
            .find("export HF_HOME=/workspace/hf")
            .expect("job env exported");
        assert!(
            export < s.find("setsid nohup").expect("start"),
            "set before the job runs"
        );
        assert!(s.contains("OFFRIG_JOB_DIR=/workspace/job"));
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
    fn progress_bars_collapse_to_what_each_line_showed_last() {
        // A tqdm-style line: many frames joined by carriage returns, one newline.
        let bar = "  0%|          | 0/100\r 40%|####      | 40/100\r100%|##########| 100/100\n";
        assert_eq!(collapse_progress(bar), "100%|##########| 100/100\n");
        // Plain lines are untouched, including blank ones and a missing final newline.
        assert_eq!(collapse_progress("a\n\nb"), "a\n\nb");
        assert_eq!(
            collapse_progress("epoch 1\nepoch 2\n"),
            "epoch 1\nepoch 2\n"
        );
        // A Windows line ending is a line ending, not an empty frame.
        assert_eq!(collapse_progress("one\r\ntwo\r\n"), "one\ntwo\n");
        // A bar still being drawn keeps its last full frame; a trailing bare \r is ignored.
        assert_eq!(collapse_progress("x\n 10%\r 20%\r"), "x\n 20%");
        // The tail returned by status is collapsed too.
        let s = parse_status("running\n 1%\r 2%\r 3%\nloss 0.5\n");
        assert_eq!(s.log_tail, " 3%\nloss 0.5\n");
    }

    #[test]
    fn a_synchronous_run_is_capped_and_never_puts_the_command_in_the_ssh_line() {
        assert_eq!(run_timeout(None), 30);
        assert_eq!(run_timeout(Some(10)), 10);
        assert_eq!(run_timeout(Some(0)), 1, "zero is not a timeout");
        assert_eq!(run_timeout(Some(100_000)), 120, "capped");
        let cmd = "nvidia-smi --query-gpu=name --format=csv; echo $HOME && ls 'a b'";
        let s = run_script(cmd, 500).expect("script");
        assert!(
            !s.contains("nvidia-smi"),
            "the command travels as base64 only"
        );
        assert!(s.contains(&base64(cmd.as_bytes())));
        assert!(s.contains("timeout -k 5 120 bash"), "{s}");
        assert!(s.contains("< /dev/null"), "stdin is closed");
        assert!(s.contains("cd /workspace/job"));
        assert!(s.contains("export HF_HOME=/workspace/hf"));
        assert!(
            s.ends_with("exit $rc"),
            "the command's own status is returned"
        );
        assert!(run_script("  ", 10).is_err());
    }

    #[test]
    fn a_run_reports_stdout_stderr_exit_code_and_a_timeout() {
        let r = finish_run(Some(0), "a\nb\n", "warn\n", 30);
        assert_eq!(
            (r.exit_code, r.timed_out, r.truncated),
            (Some(0), false, false)
        );
        assert_eq!((r.stdout.as_str(), r.stderr.as_str()), ("a\nb\n", "warn\n"));
        let failed = finish_run(Some(2), "", "ls: cannot access 'x'\n", 30);
        assert_eq!(failed.exit_code, Some(2));
        assert!(!failed.timed_out);
        assert!(
            finish_run(Some(124), "", "", 5).timed_out,
            "timeout(1) says 124"
        );
        assert!(finish_run(Some(137), "", "", 5).timed_out);
        // Progress bars in the output are collapsed; long output keeps its end.
        let bar = finish_run(Some(0), "1%\r50%\r100%\n", "", 30);
        assert_eq!(bar.stdout, "100%\n");
        let long = "x".repeat(RUN_OUTPUT_CAP + 10) + "END";
        let r = finish_run(Some(0), &long, "", 30);
        assert!(r.truncated && r.stdout.len() == RUN_OUTPUT_CAP && r.stdout.ends_with("END"));
        // The cut never splits a multi-byte character.
        let wide = "\u{20ac}".repeat(RUN_OUTPUT_CAP);
        let r = finish_run(Some(0), &wide, "", 30);
        assert!(r.truncated && r.stdout.len() <= RUN_OUTPUT_CAP);
        assert!(r.stdout.chars().all(|c| c == '\u{20ac}'));
    }

    #[test]
    fn a_whole_log_is_fetched_from_the_jobs_log_file() {
        assert_eq!(
            log_path("train-a").expect("path"),
            "/workspace/offrig/jobs/train-a.log"
        );
        assert!(log_path("bad name").is_err());
        // The path passes the same check as any other pod path.
        assert!(pod_path(&log_path("train-a").expect("path")).is_ok());
    }

    #[test]
    fn a_fetch_creates_the_missing_local_folders() {
        let base = std::env::temp_dir().join(format!("offrig-job-parents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let deep = base.join("out").join("run-1").join("logs");
        assert!(!deep.exists());
        ensure_local_dir(&deep).expect("created");
        assert!(deep.is_dir());
        ensure_local_dir(&deep).expect("already there is fine");
        // A file where a folder is needed is refused, not replaced.
        let file = base.join("a-file");
        std::fs::write(&file, b"x").expect("write");
        let err = ensure_local_dir(&file).expect_err("a file is not a folder");
        assert!(matches!(err, Error::Refused(_)), "{err}");
        assert!(file.is_file(), "left alone");
        let _ = std::fs::remove_dir_all(&base);
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
        // An absolute path on this platform splits into the directory scp runs in and a
        // bare file name, so nothing before the name (a drive letter) reaches scp.
        let base = std::env::temp_dir().join("offrig-job-test");
        let (dir, name) = split_local(&base.join("geometry.json")).expect("split");
        assert_eq!(name, "geometry.json");
        assert_eq!(dir, base);
        #[cfg(windows)]
        {
            let (dir, name) = split_local(Path::new("E:/AI/x/geometry.json")).expect("split");
            assert_eq!(name, "geometry.json");
            assert_eq!(dir, Path::new("E:/AI/x"));
        }
    }

    #[test]
    fn a_copy_is_refused_before_ssh_when_the_local_side_cannot_work() {
        let base = std::env::temp_dir().join(format!("offrig-job-refuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("dir");
        // Upload of a file that is not there.
        let err = put("offrig", &base.join("missing.json"), "x.json").expect_err("missing");
        assert!(
            matches!(&err, Error::Refused(m) if m.contains("does not exist")),
            "{err}"
        );
        // A bad pod path is refused first, in either direction.
        let file = base.join("f.txt");
        std::fs::write(&file, b"x").expect("write");
        assert!(matches!(
            put("offrig", &file, "a b").expect_err("space"),
            Error::Refused(_)
        ));
        assert!(matches!(
            get("offrig", "../etc", &file).expect_err("dotdot"),
            Error::Refused(_)
        ));
        // A download whose local parent is a file cannot make its folder.
        let under_file = file.join("sub").join("out.bin");
        let err = get("offrig", "/workspace/job/out.bin", &under_file).expect_err("parent");
        assert!(matches!(err, Error::Refused(_) | Error::Io { .. }), "{err}");
        // Fetching a log checks the job name before touching anything.
        assert!(fetch_log("offrig", "bad name", &base.join("l.log")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_local_path_without_a_file_name_is_refused() {
        let err = split_local(Path::new("..")).expect_err("no file name");
        assert!(matches!(err, Error::Refused(_)), "{err}");
        // A relative path is made absolute against the current directory.
        let (dir, name) = split_local(Path::new("some-output.bin")).expect("relative");
        assert_eq!(name, "some-output.bin");
        assert!(dir.is_absolute());
    }

    #[test]
    fn run_and_job_scripts_refuse_what_cannot_run() {
        assert!(matches!(
            run_script("  ", 10).expect_err("empty"),
            Error::Refused(_)
        ));
        assert!(matches!(
            start_script("job", "").expect_err("empty"),
            Error::Refused(_)
        ));
        assert!(start_script("bad name", "ls").is_err());
        assert!(status_script("bad name", 5).is_err());
        assert!(stop("offrig", "bad name").is_err());
        assert!(start("offrig", "bad name", "ls").is_err());
        assert!(status("offrig", "bad name", 5).is_err());
        assert!(run("offrig", "", None).is_err());
    }
}
