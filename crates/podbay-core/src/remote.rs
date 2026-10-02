//! Commands run on the pod over SSH: readiness, GPU load, and model pulls that run
//! on the pod itself, so they survive the tunnel or this app closing.

use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::proc;
use crate::spec::STATE_DIR;

pub fn ssh_exec(alias: &str, script: &str, timeout: Duration) -> Result<String> {
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        alias,
        script,
    ]);
    let out = proc::run_with_timeout(&mut cmd, timeout, &format!("ssh {alias}"))?;
    if out.success() {
        Ok(out.stdout)
    } else {
        let msg = out.stderr.trim();
        Err(Error::Ssh(if msg.is_empty() {
            format!("exit {:?}", out.status)
        } else {
            msg.lines().last().unwrap_or(msg).to_string()
        }))
    }
}

/// Poll until the pod answers over SSH.
pub fn wait_for_ssh(alias: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < deadline {
        match ssh_exec(alias, "echo ok", Duration::from_secs(25)) {
            Ok(s) if s.trim() == "ok" => return Ok(()),
            Ok(s) => last = s,
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    Err(Error::Timeout(format!("ssh to {alias} (last: {last})")))
}

#[derive(Debug, Clone, PartialEq)]
pub struct GpuStat {
    pub index: u32,
    pub name: String,
    pub util_pct: u32,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
}

pub const GPU_QUERY: &str = "nvidia-smi --query-gpu=index,name,utilization.gpu,memory.used,memory.total --format=csv,noheader,nounits";

pub fn parse_gpu_stats(csv: &str) -> Vec<GpuStat> {
    csv.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            if f.len() != 5 {
                return None;
            }
            Some(GpuStat {
                index: f[0].parse().ok()?,
                name: f[1].to_string(),
                util_pct: f[2].parse().ok()?,
                mem_used_mb: f[3].parse().ok()?,
                mem_total_mb: f[4].parse().ok()?,
            })
        })
        .collect()
}

pub fn gpu_stats(alias: &str) -> Result<Vec<GpuStat>> {
    Ok(parse_gpu_stats(&ssh_exec(
        alias,
        GPU_QUERY,
        Duration::from_secs(30),
    )?))
}

/// Ollama tags are `[namespace/]name[:tag]`. Anything else is refused before it can
/// reach a shell.
pub fn validate_model_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with(['-', '.', '/'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'));
    if ok {
        Ok(())
    } else {
        Err(Error::Ollama(format!("{name:?} is not a valid model name")))
    }
}

fn pull_file_in(dir: &str, model: &str) -> String {
    let safe: String = model
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{dir}/pulls/{safe}")
}

fn pull_file(model: &str) -> String {
    pull_file_in(STATE_DIR, model)
}

/// Only the `nohup curl` command goes to the background, with all three streams
/// redirected. Backgrounding a whole `a && b && curl &` list would put a subshell
/// in the background that still holds ssh's stdout, and ssh would never return.
fn pull_start_script_in(dir: &str, model: &str) -> Result<String> {
    validate_model_name(model)?;
    let f = pull_file_in(dir, model);
    Ok(format!(
        "mkdir -p {dir}/pulls && rm -f {f}.jsonl || exit 1; \
         nohup curl -sN http://127.0.0.1:11434/api/pull -d '{{\"model\":\"{model}\"}}' > {f}.jsonl 2>&1 < /dev/null & \
         echo $! > {f}.pid; echo started"
    ))
}

pub fn pull_start_script(model: &str) -> Result<String> {
    pull_start_script_in(STATE_DIR, model)
}

/// Start `ollama pull` on the pod in the background. Restarting a pull resumes it.
pub fn pull_start(alias: &str, model: &str) -> Result<()> {
    let out = ssh_exec(alias, &pull_start_script(model)?, Duration::from_secs(30))?;
    if out.trim().ends_with("started") {
        Ok(())
    } else {
        Err(Error::Ollama(format!(
            "pull of {model} did not start: {}",
            out.trim()
        )))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PullState {
    NotStarted,
    Running {
        status: String,
        completed: u64,
        total: u64,
    },
    Done,
    Failed(String),
}

#[derive(Deserialize)]
struct PullLine {
    #[serde(default)]
    status: String,
    #[serde(default)]
    total: u64,
    #[serde(default)]
    completed: u64,
    #[serde(default)]
    error: Option<String>,
}

/// `tail` is the end of the pull's JSON-lines log; `alive` says whether its curl
/// is still running.
pub fn parse_pull_state(tail: &str, alive: bool) -> PullState {
    let mut last_progress: Option<PullLine> = None;
    for line in tail.lines() {
        let Ok(l) = serde_json::from_str::<PullLine>(line.trim()) else {
            continue;
        };
        if let Some(e) = l.error {
            return PullState::Failed(e);
        }
        if l.status == "success" {
            return PullState::Done;
        }
        if l.total > 0 || last_progress.is_none() {
            last_progress = Some(l);
        }
    }
    match (last_progress, alive) {
        (Some(l), true) => PullState::Running {
            status: l.status,
            completed: l.completed,
            total: l.total,
        },
        (None, true) => PullState::Running {
            status: "starting".into(),
            completed: 0,
            total: 0,
        },
        (Some(l), false) => PullState::Failed(format!("pull stopped at: {}", l.status)),
        (None, false) => PullState::NotStarted,
    }
}

pub fn pull_state(alias: &str, model: &str) -> Result<PullState> {
    validate_model_name(model)?;
    let f = pull_file(model);
    let script = format!(
        "if [ ! -f {f}.jsonl ]; then echo NOFILE; exit 0; fi; \
         if kill -0 $(cat {f}.pid 2>/dev/null) 2>/dev/null; then echo ALIVE; else echo DEAD; fi; \
         tail -c 4096 {f}.jsonl"
    );
    let out = ssh_exec(alias, &script, Duration::from_secs(30))?;
    let mut lines = out.splitn(2, '\n');
    let head = lines.next().unwrap_or("").trim();
    let rest = lines.next().unwrap_or("");
    Ok(match head {
        "NOFILE" => PullState::NotStarted,
        "ALIVE" => parse_pull_state(rest, true),
        _ => parse_pull_state(rest, false),
    })
}

pub fn bootstrap_log(alias: &str) -> Result<String> {
    ssh_exec(
        alias,
        &format!("tail -n 40 {STATE_DIR}/bootstrap.log"),
        Duration::from_secs(30),
    )
}

/// The pod's model list read on the pod itself, for the tunnel identity check.
pub fn pod_tags_json(alias: &str) -> Result<String> {
    ssh_exec(
        alias,
        "curl -s http://127.0.0.1:11434/api/tags",
        Duration::from_secs(30),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvidia_smi_rows_and_skips_noise() {
        let csv = "0, NVIDIA H200, 87, 120000, 143771\n1, NVIDIA H200, 3, 98000, 143771\nNo devices were found\n";
        let s = parse_gpu_stats(csv);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].util_pct, 87);
        assert_eq!(s[1].mem_used_mb, 98000);
        assert_eq!(s[1].name, "NVIDIA H200");
    }

    #[test]
    fn model_names_are_validated_before_any_shell() {
        for ok in [
            "qwen3:8b",
            "qwen3-coder:30b-a3b-q8_0",
            "library/gpt-oss:120b",
            "hf.co/org/model:Q4_K_M",
        ] {
            validate_model_name(ok).unwrap_or_else(|e| panic!("{ok} should pass: {e}"));
        }
        for bad in [
            "",
            "a b",
            "x;rm -rf /",
            "$(id)",
            "'q'",
            "-o",
            "a\nb",
            "q`x`",
        ] {
            assert!(
                validate_model_name(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
        assert!(pull_start_script("x;id").is_err());
    }

    #[test]
    fn pull_script_names_a_safe_file() {
        let s = pull_start_script("hf.co/org/m:Q4").expect("valid name");
        assert!(s.contains("/workspace/podbay/pulls/hf.co_org_m_Q4.jsonl"));
        assert!(s.contains(r#"-d '{"model":"hf.co/org/m:Q4"}'"#));
    }

    #[test]
    fn only_curl_is_backgrounded() {
        let s = pull_start_script("qwen3:8b").expect("valid name");
        assert!(s.contains("|| exit 1; nohup curl"), "{s}");
        assert!(s.contains("< /dev/null & echo $! >"), "{s}");
    }

    /// Regression for a live hang: the script must return while the pull keeps going.
    #[cfg(unix)]
    #[test]
    fn pull_start_returns_while_the_pull_runs() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("podbay-pull-{}", std::process::id()));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).expect("temp dir");
        let curl = bin.join("curl");
        std::fs::write(
            &curl,
            "#!/bin/sh\nsleep 5\necho '{\"status\":\"success\"}'\n",
        )
        .expect("fake curl");
        std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let state = dir.join("state");
        let script =
            pull_start_script_in(state.to_str().expect("utf8 path"), "qwen3:8b").expect("script");
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(&script).env("PATH", path);
        let started = std::time::Instant::now();
        let out = crate::proc::run_with_timeout(&mut cmd, Duration::from_secs(3), "pull script")
            .expect("the script must not wait for curl");
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(out.stdout.trim(), "started");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_state_reads_progress_success_and_errors() {
        let running = "{\"status\":\"pulling manifest\"}\n{\"status\":\"pulling abc\",\"digest\":\"sha256:abc\",\"total\":1000,\"completed\":250}\n{\"status\":\"pulling abc\",\"total\":1000,\"comp";
        assert_eq!(
            parse_pull_state(running, true),
            PullState::Running {
                status: "pulling abc".into(),
                completed: 250,
                total: 1000
            }
        );
        assert!(matches!(
            parse_pull_state(running, false),
            PullState::Failed(_)
        ));
        assert_eq!(
            parse_pull_state(
                "{\"status\":\"verifying sha256 digest\"}\n{\"status\":\"success\"}\n",
                false
            ),
            PullState::Done
        );
        assert_eq!(
            parse_pull_state(
                "{\"error\":\"pull model manifest: file does not exist\"}\n",
                false
            ),
            PullState::Failed("pull model manifest: file does not exist".into())
        );
        assert_eq!(parse_pull_state("", false), PullState::NotStarted);
        assert!(matches!(
            parse_pull_state("", true),
            PullState::Running { .. }
        ));
    }
}
