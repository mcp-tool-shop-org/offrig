//! Staging: put a recipe profile's weights on a RunPod network volume once, so every
//! later launch skips the download (measured 2026-10-03: 252 GB took about 20 of the
//! frontier's 22 minutes to ready, at $8.36/hr).
//!
//! A network volume bills monthly whether or not a pod runs, and it ties the
//! profile's pods to its data center. So staging is a human command (`offrig stage`),
//! never an agent tool, and removing the volume is its named compensator.

use std::time::{Duration, Instant};

use crate::config::{Config, Profile};
use crate::error::{Error, Result};
use crate::remote;
use crate::runpod::{NetworkVolume, PodCreate, RunPod};
use crate::session::{Event, Session};
use crate::spec::HF_DIR;

/// Where staged weights are marked complete, one file per model.
pub const STAGED_DIR: &str = "/workspace/offrig/staged";
/// RunPod's standard network storage, first TB (docs.runpod.io, 2026-10-03).
pub const USD_PER_GB_MONTH: f64 = 0.07;
/// A small image with Python; the staging pod only downloads.
pub const STAGE_IMAGE: &str = "python:3.12-slim-bookworm";
/// The staging pod downloads and nothing else: any cheap card will do. RunPod takes
/// the first with capacity in the volume's data center.
pub const STAGE_GPUS: &[&str] = &[
    "NVIDIA RTX A4000",
    "NVIDIA RTX 2000 Ada Generation",
    "NVIDIA RTX A4500",
    "NVIDIA RTX 4000 Ada Generation",
    "NVIDIA RTX A5000",
    "NVIDIA L4",
    "NVIDIA RTX A6000",
    "NVIDIA A40",
    "NVIDIA RTX PRO 6000 Blackwell Server Edition",
];
pub const STAGE_TIMEOUT: Duration = Duration::from_secs(120 * 60);

/// The marker's file name for a Hugging Face repo id.
pub fn stage_key(model: &str) -> String {
    model.replace('/', "__")
}

pub fn volume_name(profile: &Profile) -> String {
    format!("offrig-{}", profile.name)
}

/// Weights plus 15% and 10 GB of room, at least 50 GB.
pub fn volume_size_gb(profile: &Profile) -> u32 {
    let weights: f64 = profile.models.iter().map(|m| m.size_gb).sum();
    ((weights * 1.15).ceil() as u32 + 10).max(50)
}

pub fn monthly_usd(size_gb: u32) -> f64 {
    (f64::from(size_gb) * USD_PER_GB_MONTH * 100.0).round() / 100.0
}

/// Runs as `bash -c` on the staging pod: sshd first (to watch it), then the download
/// with Hugging Face's own client into the same `HF_HOME` the engine reads, then the
/// marker that lets launches go offline. The marker is written only on success.
pub const STAGE_BOOTSTRAP: &str = r#"set -u
mkdir -p /workspace/offrig/staged /workspace/hf
exec > >(tee -a /workspace/offrig/stage.log) 2>&1
echo "[offrig] stage start $(date -u +%FT%TZ)"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq && apt-get install -y -qq --no-install-recommends openssh-server >/dev/null
mkdir -p /run/sshd /root/.ssh && chmod 700 /root/.ssh
printf '%s\n' "${PUBLIC_KEY:-}" > /root/.ssh/authorized_keys && chmod 600 /root/.ssh/authorized_keys
ssh-keygen -A >/dev/null
/usr/sbin/sshd -o PasswordAuthentication=no -o PermitRootLogin=prohibit-password -o AllowTcpForwarding=no
pip install -q -U "huggingface_hub[hf_xet]"
echo "[offrig] downloading $OFFRIG_MODEL"
if hf download "$OFFRIG_MODEL" --quiet; then
  du -sb /workspace/hf | cut -f1 > "/workspace/offrig/staged/$OFFRIG_STAGE_KEY"
  echo "[offrig] STAGED_OK"
else
  echo "[offrig] STAGE_FAILED"
fi
sleep infinity
"#;

/// The profile's recipe, or why it cannot be staged. Called before anything that
/// creates or costs: a live test on 2026-10-03 created a volume for an Ollama profile
/// because this check only ran when the staging pod was built.
pub fn stageable(profile: &Profile) -> Result<&crate::config::Recipe> {
    profile.recipe.as_ref().ok_or_else(|| {
        Error::Refused(format!(
            "profile {} has no recipe: Ollama profiles pull from Ollama's registry and are not staged",
            profile.name
        ))
    })
}

pub fn stage_pod(profile: &Profile, volume: &NetworkVolume) -> Result<PodCreate> {
    let recipe = stageable(profile)?;
    let mut env = std::collections::BTreeMap::new();
    env.insert("OFFRIG_MODEL".into(), recipe.model.clone());
    env.insert("OFFRIG_STAGE_KEY".into(), stage_key(&recipe.model));
    env.insert("HF_HOME".into(), HF_DIR.into());
    if let Some(sec) = &recipe.hf_token_secret {
        env.insert("HF_TOKEN".into(), format!("{{{{ RUNPOD_SECRET_{sec} }}}}"));
    }
    Ok(PodCreate {
        name: format!("offrig-stage-{}", profile.name),
        image_name: STAGE_IMAGE.into(),
        gpu_type_ids: STAGE_GPUS.iter().map(|s| (*s).to_string()).collect(),
        gpu_type_priority: "custom".into(),
        gpu_count: 1,
        cloud_type: "SECURE".into(),
        support_public_ip: true,
        ports: vec!["22/tcp".into()],
        container_disk_in_gb: 20,
        volume_in_gb: None,
        network_volume_id: Some(volume.id.clone()),
        volume_mount_path: "/workspace".into(),
        data_center_ids: vec![volume.data_center_id.clone()],
        allowed_cuda_versions: vec![],
        docker_entrypoint: vec!["bash".into(), "-c".into()],
        docker_start_cmd: vec![STAGE_BOOTSTRAP.into()],
        env,
    })
}

/// What the staging pod's log says.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Running,
    Done,
    Failed(String),
}

pub fn parse_progress(log_tail: &str) -> Progress {
    if log_tail.contains("[offrig] STAGED_OK") {
        Progress::Done
    } else if log_tail.contains("[offrig] STAGE_FAILED") {
        Progress::Failed(log_tail.trim().to_string())
    } else {
        Progress::Running
    }
}

/// Deletes the staging pod however `run` ends: it only downloads, and it bills.
struct PodGuard<'a> {
    rp: &'a RunPod,
    id: Option<String>,
}

impl Drop for PodGuard<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let _ = self.rp.delete_pod(&id);
        }
    }
}

/// Find the profile's volume in `data_center`, or create it.
pub fn ensure_volume(
    rp: &RunPod,
    profile: &Profile,
    data_center: &str,
) -> Result<(NetworkVolume, bool)> {
    stageable(profile)?;
    let name = volume_name(profile);
    if let Some(v) = rp
        .list_volumes()?
        .into_iter()
        .find(|v| v.name == name && v.data_center_id == data_center)
    {
        return Ok((v, false));
    }
    Ok((
        rp.create_volume(&name, volume_size_gb(profile), data_center)?,
        true,
    ))
}

/// Download the profile's weights onto `volume` with a short-lived pod in its data
/// center. The pod is deleted on success, failure, timeout or panic.
pub fn fill(
    cfg: &Config,
    rp: RunPod,
    profile: &Profile,
    volume: &NetworkVolume,
    on: &mut dyn FnMut(Event),
) -> Result<()> {
    let body = stage_pod(profile, volume)?;
    // A separate ssh alias, so a live session's `offrig` alias is never rewritten.
    let mut stage_cfg = cfg.clone();
    stage_cfg.ssh_alias = format!("{}-stage", cfg.ssh_alias);
    let session = Session::with_client(stage_cfg.clone(), rp);
    let pod = session.rp.create_pod(&body)?;
    let mut guard = PodGuard {
        rp: &session.rp,
        id: Some(pod.id.clone()),
    };
    on(Event::Step(format!(
        "staging pod {} created at ${:.2}/hr in {}",
        pod.id, pod.cost_per_hr, volume.data_center_id
    )));
    session.wait_ready(&pod.id, on)?;
    let want = profile.models.iter().map(|m| m.size_gb).sum::<f64>();
    let started = Instant::now();
    let mut last = (Instant::now(), 0u64);
    loop {
        let out = remote::ssh_exec(
            &stage_cfg.ssh_alias,
            "du -sb /workspace/hf 2>/dev/null | cut -f1; tail -n 5 /workspace/offrig/stage.log",
            Duration::from_secs(30),
        )?;
        let mut lines = out.lines();
        let bytes: u64 = lines
            .next()
            .and_then(|l| l.trim().parse().ok())
            .unwrap_or(0);
        let tail = lines.collect::<Vec<_>>().join("\n");
        match parse_progress(&tail) {
            Progress::Done => {
                on(Event::Step(format!(
                    "staged {:.1} GB in {} min",
                    bytes as f64 / 1e9,
                    started.elapsed().as_secs() / 60
                )));
                break;
            }
            Progress::Failed(log) => return Err(Error::Engine(format!("staging failed: {log}"))),
            Progress::Running => {
                let secs = last.0.elapsed().as_secs_f64().max(1.0);
                let rate = bytes.saturating_sub(last.1) as f64 / secs / 1e6;
                on(Event::Step(format!(
                    "downloading: {:.1} of ~{want:.0} GB ({rate:.0} MB/s)",
                    bytes as f64 / 1e9
                )));
                last = (Instant::now(), bytes);
            }
        }
        if started.elapsed() >= STAGE_TIMEOUT {
            return Err(Error::Timeout(format!(
                "staging {} within {} minutes",
                profile.name,
                STAGE_TIMEOUT.as_secs() / 60
            )));
        }
        std::thread::sleep(Duration::from_secs(20));
    }
    if let Some(id) = guard.id.take() {
        session.rp.delete_pod(&id)?;
        on(Event::Step(format!("staging pod {id} terminated")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frontier() -> Profile {
        Config::default()
            .profile("frontier")
            .expect("frontier")
            .clone()
    }

    #[test]
    fn the_volume_fits_the_weights_with_room_and_its_price_is_shown() {
        let p = frontier();
        assert_eq!(volume_size_gb(&p), 300, "252 GB x 1.15 + 10");
        assert!((monthly_usd(300) - 21.0).abs() < 1e-9);
        let mini = Config::default()
            .profile("frontier-mini-awq")
            .expect("mini")
            .clone();
        assert_eq!(volume_size_gb(&mini), 50, "never below 50 GB");
    }

    #[test]
    fn the_staging_pod_downloads_into_the_engines_cache_in_the_volumes_data_center() {
        let p = frontier();
        let vol = NetworkVolume {
            id: "vol1".into(),
            name: volume_name(&p),
            size: 300,
            data_center_id: "EUR-IS-1".into(),
        };
        let body = stage_pod(&p, &vol).expect("recipe profile");
        assert_eq!(body.network_volume_id.as_deref(), Some("vol1"));
        assert_eq!(body.data_center_ids, ["EUR-IS-1"]);
        assert_eq!(body.volume_mount_path, "/workspace");
        assert_eq!(
            body.env["HF_HOME"], HF_DIR,
            "the same cache the engine reads"
        );
        assert_eq!(
            body.env["OFFRIG_STAGE_KEY"],
            "QuantTrio__Qwen3-Coder-480B-A35B-Instruct-AWQ"
        );
        assert_eq!(body.ports, ["22/tcp"]);
        assert!(!body.image_name.ends_with(":latest"));
        let dl = STAGE_BOOTSTRAP.find("hf download").expect("download");
        let mark = STAGE_BOOTSTRAP
            .find("/workspace/offrig/staged/")
            .expect("marker");
        assert!(dl < mark, "the marker comes after the download");
        assert!(
            STAGE_BOOTSTRAP.contains("if hf download"),
            "only on success"
        );
        assert!(
            STAGE_BOOTSTRAP.contains("AllowTcpForwarding=no"),
            "nothing to tunnel to"
        );
        let ollama = Config::default().profile("small").expect("small").clone();
        assert!(
            stage_pod(&ollama, &vol).is_err(),
            "Ollama profiles are not staged"
        );
    }

    /// A one-route-table HTTP server standing in for RunPod; returns the hit log.
    fn mock(
        routes: fn(&str) -> (u16, &'static str),
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", l.local_addr().expect("addr"));
        let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in l.incoming().map_while(std::result::Result::ok) {
                let mut r = BufReader::new(stream);
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let mut it = line.split_whitespace();
                let route = format!(
                    "{} {}",
                    it.next().unwrap_or(""),
                    it.next().unwrap_or("").split('?').next().unwrap_or("")
                );
                let mut len = 0;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                let _ = r.read_exact(&mut body);
                log.lock().expect("log").push(route.clone());
                let (code, text) = routes(&route);
                let _ = write!(
                    r.get_mut(),
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        (url, hits)
    }

    #[test]
    fn a_failed_stage_terminates_its_pod() {
        let (url, hits) = mock(|route| match route {
            "POST /pods" => (
                200,
                r#"{"id":"s1","name":"offrig-stage-frontier","desiredStatus":"RUNNING","costPerHr":0.17}"#,
            ),
            "GET /pods/s1" => (500, r#"{"error":"boom"}"#),
            "DELETE /pods/s1" => (204, ""),
            _ => (404, "{}"),
        });
        let rp = RunPod::new("test-key", &url, &format!("{url}/graphql"));
        let vol = NetworkVolume {
            id: "vol1".into(),
            name: "offrig-frontier".into(),
            size: 300,
            data_center_id: "EUR-IS-1".into(),
        };
        let err = fill(&Config::default(), rp, &frontier(), &vol, &mut |_| {})
            .expect_err("the status check fails");
        assert!(!err.to_string().is_empty());
        let h = hits.lock().expect("hits");
        assert_eq!(h.iter().filter(|x| *x == "POST /pods").count(), 1);
        assert_eq!(
            h.iter().filter(|x| *x == "DELETE /pods/s1").count(),
            1,
            "the pod was terminated: {h:?}"
        );
    }

    #[test]
    fn an_unstageable_profile_never_reaches_runpod() {
        let (url, hits) = mock(|_| (200, "[]"));
        let rp = RunPod::new("test-key", &url, &format!("{url}/graphql"));
        let small = Config::default().profile("small").expect("small").clone();
        assert!(ensure_volume(&rp, &small, "EUR-IS-1").is_err());
        assert!(
            hits.lock().expect("hits").is_empty(),
            "no volume listed or created"
        );
    }

    #[test]
    fn progress_reads_the_log() {
        assert_eq!(parse_progress("downloading x"), Progress::Running);
        assert_eq!(parse_progress("...\n[offrig] STAGED_OK"), Progress::Done);
        assert!(matches!(
            parse_progress("401 Unauthorized\n[offrig] STAGE_FAILED"),
            Progress::Failed(_)
        ));
    }
}
