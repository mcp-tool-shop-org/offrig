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

/// Whether something at `host:port` answers like sshd: a TCP connect, then the
/// `SSH-` banner sshd sends before anything is asked. No key and no login, so it is
/// cheap enough to run on every progress poll. Any failure is simply "not yet".
pub fn ssh_answers(host: &str, port: u16, timeout: Duration) -> bool {
    use std::io::Read;
    use std::net::{TcpStream, ToSocketAddrs};
    let Ok(mut addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(timeout));
    let mut banner = [0u8; 4];
    s.read_exact(&mut banner).is_ok() && &banner == b"SSH-"
}

/// The host driver's CUDA version from `nvidia-smi`'s header, which prints it as
/// `CUDA Version: 12.8` and, on newer drivers, `CUDA UMD Version: 13.4`. `None` when
/// no such field is in the text.
pub fn parse_cuda_version(smi: &str) -> Option<String> {
    for line in smi.lines() {
        for label in ["CUDA UMD Version", "CUDA Version"] {
            let Some(at) = line.find(label) else { continue };
            let rest = line[at + label.len()..].trim_start();
            let Some(rest) = rest.strip_prefix(':') else {
                continue;
            };
            let v: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let v = v.trim_end_matches('.');
            if v.split('.').count() >= 2 && v.split('.').all(|p| !p.is_empty()) {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// One `nvidia-smi` on the pod: the header's CUDA version and how many cards the
/// table lists. The launch asks once; both facts come from that same text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmiReading {
    pub cuda_version: Option<String>,
    pub gpu_count: Option<u32>,
}

/// How many GPU cards `nvidia-smi`'s table lists. A card row starts with the card
/// index and then a name (`|   0  NVIDIA ...`). Process rows (`|    0   N/A`) and the
/// header are not cards. `None` when the text has no card row. Against a plan above
/// 0 the caller reports that as zero cards, the worst mismatch, not as "not measured".
pub fn parse_gpu_count(smi: &str) -> Option<u32> {
    let n = smi.lines().filter(|line| gpu_card_row(line)).count();
    (n > 0).then_some(u32::try_from(n).unwrap_or(u32::MAX))
}

/// A table row whose first cell is a card index and whose next cell is a name.
fn gpu_card_row(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix('|') else {
        return false;
    };
    let Some((idx, after)) = rest.trim_start().split_once(char::is_whitespace) else {
        return false;
    };
    if idx.parse::<u32>().is_err() {
        return false;
    }
    let token = after.split_whitespace().next().unwrap_or("");
    !token.is_empty()
        && token != "N/A"
        && token
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
}

/// Run `nvidia-smi` once on the pod and read the CUDA version and the card count.
/// `Err` when it could not be run (no ssh, no driver).
pub fn read_nvidia_smi(alias: &str) -> Result<SmiReading> {
    let text = ssh_exec(alias, "nvidia-smi", Duration::from_secs(30))?;
    Ok(SmiReading {
        cuda_version: parse_cuda_version(&text),
        gpu_count: parse_gpu_count(&text),
    })
}

/// Run `nvidia-smi` once on the pod and read its CUDA version. `Ok(None)` when it ran
/// but printed no version; `Err` when it could not be run (no ssh, no driver).
pub fn host_cuda_version(alias: &str) -> Result<Option<String>> {
    Ok(read_nvidia_smi(alias)?.cuda_version)
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
/// The pod engine's OpenAI model list, read on the pod itself (both engines serve it).
pub fn pod_models_json(alias: &str) -> Result<String> {
    ssh_exec(
        alias,
        "curl -s http://127.0.0.1:11434/v1/models",
        Duration::from_secs(30),
    )
}

/// A recipe engine while it starts: bytes downloaded, whether it still runs, and the
/// last lines of its log.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineState {
    pub hf_bytes: u64,
    pub running: bool,
    pub log_tail: String,
}

pub const ENGINE_STATE_SCRIPT: &str = "du -sb /workspace/hf 2>/dev/null | cut -f1; \
if pgrep -f sglang.launch_server >/dev/null; then echo running; else echo stopped; fi; \
tail -n 40 /workspace/offrig/engine.log 2>/dev/null";

pub fn parse_engine_state(out: &str) -> EngineState {
    let mut lines = out.lines();
    let hf_bytes = lines
        .next()
        .and_then(|l| l.trim().parse().ok())
        .unwrap_or(0);
    let running = lines.next().is_some_and(|l| l.trim() == "running");
    EngineState {
        hf_bytes,
        running,
        log_tail: lines.collect::<Vec<_>>().join("\n"),
    }
}

pub fn engine_state(alias: &str) -> Result<EngineState> {
    Ok(parse_engine_state(&ssh_exec(
        alias,
        ENGINE_STATE_SCRIPT,
        Duration::from_secs(30),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_state_reads_bytes_liveness_and_log() {
        let s = parse_engine_state("31457280000\nrunning\nLoading safetensors 40%\nstill loading");
        assert_eq!(s.hf_bytes, 31_457_280_000);
        assert!(s.running);
        assert_eq!(s.log_tail, "Loading safetensors 40%\nstill loading");
        let dead = parse_engine_state("\nstopped\n[offrig] engine exited with status 1");
        assert_eq!(dead.hf_bytes, 0);
        assert!(!dead.running && dead.log_tail.contains("status 1"));
    }

    #[test]
    fn the_engine_log_tail_is_forty_lines() {
        assert!(
            ENGINE_STATE_SCRIPT.contains("tail -n 40 /workspace/offrig/engine.log"),
            "{ENGINE_STATE_SCRIPT}"
        );
    }

    #[test]
    fn the_cuda_version_is_read_from_both_nvidia_smi_header_forms() {
        let old = "| NVIDIA-SMI 570.124.06             Driver Version: 570.124.06     CUDA Version: 12.8     |";
        assert_eq!(parse_cuda_version(old).as_deref(), Some("12.8"));
        let newer = "| NVIDIA-SMI 595.45.04              Driver Version: 595.45.04      CUDA UMD Version: 13.4    |";
        assert_eq!(parse_cuda_version(newer).as_deref(), Some("13.4"));
        // A whole table: the header line is found among the rest.
        let table = format!(
            "Tue Oct  7 12:00:01 2026\n+---+\n{old}\n|---+\n| GPU  Name  Persistence-M |\n"
        );
        assert_eq!(parse_cuda_version(&table).as_deref(), Some("12.8"));
        // A space before the colon and a two-digit minor still read.
        assert_eq!(
            parse_cuda_version("CUDA UMD Version : 12.10").as_deref(),
            Some("12.10")
        );
        for none in [
            "",
            "NVIDIA-SMI has failed because it couldn't communicate with the NVIDIA driver.",
            "Driver Version: 570.124.06",
            "CUDA Version: N/A",
            "CUDA Version: 12",
        ] {
            assert_eq!(parse_cuda_version(none), None, "{none:?}");
        }
    }

    #[test]
    fn nvidia_smi_counts_gpu_cards_and_ignores_process_rows() {
        let one = "\
| NVIDIA-SMI 570.124.06   Driver Version: 570.124.06   CUDA Version: 12.8     |
| GPU  Name                 Persistence-M |
|   0  NVIDIA RTX PRO 6000 Blackwell Server Edition  Off |
| 30%   32C    P8             15W /  300W |
|    0   N/A  N/A            1234      C   python |
";
        assert_eq!(parse_gpu_count(one), Some(1));
        assert_eq!(parse_cuda_version(one).as_deref(), Some("12.8"));
        let two = "\
| NVIDIA-SMI 570.124.06   Driver Version: 570.124.06   CUDA Version: 12.8     |
|   0  NVIDIA RTX PRO 6000 Blackwell Server Edition  Off |
|   1  NVIDIA RTX PRO 6000 Blackwell Server Edition  Off |
|    0   N/A  N/A            1234      C   python |
|    1   N/A  N/A            1235      C   python |
";
        assert_eq!(parse_gpu_count(two), Some(2));
        // A header, a fan line and a process row are not cards.
        assert_eq!(
            parse_gpu_count("| NVIDIA-SMI 570.1  Driver Version: 570.1  CUDA Version: 12.8 |\n"),
            None
        );
        assert_eq!(parse_gpu_count("|    0   N/A  N/A   1  C   python |"), None);
        assert_eq!(parse_gpu_count(""), None);
    }

    #[test]
    fn ssh_answers_only_to_an_ssh_banner() {
        use std::io::Write;
        let banner = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = banner.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for mut s in banner.incoming().map_while(std::result::Result::ok) {
                let _ = s.write_all(b"SSH-2.0-OpenSSH_9.6\r\n");
            }
        });
        let wait = Duration::from_secs(3);
        assert!(ssh_answers("127.0.0.1", port, wait));
        // Something listening that is not sshd, and nothing listening at all.
        let http = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let http_port = http.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for mut s in http.incoming().map_while(std::result::Result::ok) {
                let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
            }
        });
        assert!(!ssh_answers("127.0.0.1", http_port, wait));
        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let closed_port = closed.local_addr().expect("addr").port();
        drop(closed);
        assert!(!ssh_answers("127.0.0.1", closed_port, wait));
    }

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
        assert!(s.contains("/workspace/offrig/pulls/hf.co_org_m_Q4.jsonl"));
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
        let dir = std::env::temp_dir().join(format!("offrig-pull-{}", std::process::id()));
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
