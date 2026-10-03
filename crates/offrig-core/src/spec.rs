//! Turns a profile into a `POST /pods` body. The pod runs the pinned Ollama image
//! with a bootstrap that adds sshd, and exposes only 22/tcp: Ollama listens on the
//! pod's loopback, so the one way to reach it is the SSH tunnel.

use std::collections::BTreeMap;

use crate::config::{Config, OLLAMA_IMAGE, Profile, REMOTE_OLLAMA_PORT};
use crate::runpod::PodCreate;

/// Where weights live on the pod. On a network volume they outlive the pod.
pub const MODELS_DIR: &str = "/workspace/ollama/models";
/// offrig's working directory on the pod: bootstrap log, pull logs.
pub const STATE_DIR: &str = "/workspace/offrig";

/// Runs as `bash -c` in place of the image's `ollama` entrypoint.
pub const BOOTSTRAP: &str = r#"set -u
mkdir -p /workspace/offrig/pulls
exec > >(tee -a /workspace/offrig/bootstrap.log) 2>&1
echo "[offrig] bootstrap start $(date -u +%FT%TZ)"
export DEBIAN_FRONTEND=noninteractive
if ! command -v sshd >/dev/null 2>&1 || ! command -v curl >/dev/null 2>&1; then
  apt-get update -qq && apt-get install -y -qq --no-install-recommends openssh-server curl ca-certificates >/dev/null
fi
mkdir -p /run/sshd /root/.ssh && chmod 700 /root/.ssh
printf '%s\n' "${PUBLIC_KEY:-}" > /root/.ssh/authorized_keys && chmod 600 /root/.ssh/authorized_keys
ssh-keygen -A >/dev/null
/usr/sbin/sshd -o PasswordAuthentication=no -o PermitRootLogin=prohibit-password -o AllowTcpForwarding=local
mkdir -p "$OLLAMA_MODELS"
echo "[offrig] sshd up, starting ollama on $OLLAMA_HOST"
exec ollama serve
"#;

pub fn pod_name(profile: &Profile) -> String {
    format!("offrig-{}", profile.name)
}

pub fn ollama_env(profile: &Profile) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        "OLLAMA_HOST".into(),
        format!("127.0.0.1:{REMOTE_OLLAMA_PORT}"),
    );
    env.insert("OLLAMA_MODELS".into(), MODELS_DIR.into());
    env.insert("OLLAMA_KEEP_ALIVE".into(), "-1".into());
    env.insert(
        "OLLAMA_CONTEXT_LENGTH".into(),
        profile.context_length.to_string(),
    );
    env.insert("OLLAMA_FLASH_ATTENTION".into(), "1".into());
    env.insert("OLLAMA_KV_CACHE_TYPE".into(), "q8_0".into());
    env.insert(
        "OLLAMA_NUM_PARALLEL".into(),
        profile.parallel.max(1).to_string(),
    );
    env
}

pub fn pod_create(_cfg: &Config, profile: &Profile) -> PodCreate {
    let on_volume = profile.network_volume_id.is_some();
    PodCreate {
        name: pod_name(profile),
        image_name: OLLAMA_IMAGE.into(),
        gpu_type_ids: profile.gpu_type_ids.clone(),
        gpu_type_priority: "custom".into(),
        gpu_count: profile.gpu_count,
        cloud_type: "SECURE".into(),
        support_public_ip: true,
        ports: vec!["22/tcp".into()],
        container_disk_in_gb: profile.container_disk_gb,
        volume_in_gb: (!on_volume).then_some(profile.volume_gb),
        network_volume_id: profile.network_volume_id.clone(),
        volume_mount_path: "/workspace".into(),
        data_center_ids: vec![],
        docker_entrypoint: vec!["bash".into(), "-c".into()],
        docker_start_cmd: vec![BOOTSTRAP.into()],
        env: ollama_env(profile),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medium_pod_exposes_only_ssh_and_keeps_ollama_on_loopback() {
        let cfg = Config::default();
        let p = cfg.profile("medium").expect("medium profile");
        let body = pod_create(&cfg, p);
        assert_eq!(
            body.ports,
            ["22/tcp"],
            "no http port: ollama must not be reachable through RunPod's proxy"
        );
        assert_eq!(body.env["OLLAMA_HOST"], "127.0.0.1:11434");
        assert_eq!(body.image_name, OLLAMA_IMAGE);
        assert!(!body.image_name.ends_with(":latest"));
        assert_eq!(body.volume_in_gb, Some(p.volume_gb));
        assert_eq!(body.network_volume_id, None);
        assert_eq!(body.docker_entrypoint, ["bash", "-c"]);
        assert_eq!(body.name, "offrig-medium");
    }

    #[test]
    fn pod_serves_requests_in_parallel_slots() {
        let cfg = Config::default();
        for name in ["small", "medium", "frontier"] {
            let p = cfg.profile(name).expect("profile");
            assert_eq!(ollama_env(p)["OLLAMA_NUM_PARALLEL"], "4", "{name}");
        }
        // A config saved before the field existed still gets slots.
        let mut v = serde_json::to_value(cfg.profile("small").expect("small")).expect("json");
        v.as_object_mut().expect("object").remove("parallel");
        let old: Profile = serde_json::from_value(v).expect("old profile");
        assert_eq!(old.parallel, 4);
    }

    #[test]
    fn network_volume_replaces_pod_volume() {
        let cfg = Config::default();
        let mut p = cfg.profile("frontier").expect("frontier profile").clone();
        p.network_volume_id = Some("vol123".into());
        let body = pod_create(&cfg, &p);
        assert_eq!(body.network_volume_id.as_deref(), Some("vol123"));
        assert_eq!(body.volume_in_gb, None);
    }

    #[test]
    fn bootstrap_starts_sshd_before_ollama_and_allows_only_local_forwarding() {
        let sshd = BOOTSTRAP.find("/usr/sbin/sshd").expect("sshd line");
        let serve = BOOTSTRAP.find("exec ollama serve").expect("serve line");
        assert!(sshd < serve);
        assert!(BOOTSTRAP.contains("AllowTcpForwarding=local"));
        assert!(BOOTSTRAP.contains("PasswordAuthentication=no"));
    }
}
