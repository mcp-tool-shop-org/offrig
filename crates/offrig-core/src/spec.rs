//! Turns a profile into a `POST /pods` body. The pod runs the pinned Ollama image, or
//! a recipe's engine image, with a bootstrap that adds sshd, and exposes only 22/tcp:
//! the engine listens on the pod's loopback, so the one way to reach it is the SSH
//! tunnel.

use std::collections::BTreeMap;

use crate::config::{
    Config, Engine, Job, OLLAMA_IMAGE, Profile, REMOTE_OLLAMA_PORT, Recipe, cuda_at_least,
};
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

/// Where a recipe engine keeps Hugging Face downloads (on the volume).
pub const HF_DIR: &str = "/workspace/hf";
/// The engine's own log on the pod, read when it fails.
pub const ENGINE_LOG: &str = "/workspace/offrig/engine.log";

/// Runs as `bash -c` in place of the SGLang image's entrypoint. sshd comes first so a
/// failed engine can still be read over ssh; the container stays up after the engine
/// exits for the same reason (the launch reads the log, then terminates the pod).
pub const BOOTSTRAP_SGLANG: &str = r#"set -u
mkdir -p /workspace/offrig /workspace/hf
exec > >(tee -a /workspace/offrig/bootstrap.log) 2>&1
echo "[offrig] bootstrap start $(date -u +%FT%TZ)"
export DEBIAN_FRONTEND=noninteractive
if ! command -v sshd >/dev/null 2>&1; then
  apt-get update -qq && apt-get install -y -qq --no-install-recommends openssh-server >/dev/null
fi
mkdir -p /run/sshd /root/.ssh && chmod 700 /root/.ssh
printf '%s\n' "${PUBLIC_KEY:-}" > /root/.ssh/authorized_keys && chmod 600 /root/.ssh/authorized_keys
ssh-keygen -A >/dev/null
/usr/sbin/sshd -o PasswordAuthentication=no -o PermitRootLogin=prohibit-password -o AllowTcpForwarding=local
if [ -f "/workspace/offrig/staged/$OFFRIG_STAGE_KEY" ]; then
  export HF_HUB_OFFLINE=1
  echo "[offrig] weights staged on the volume; Hugging Face offline"
fi
echo "[offrig] sshd up, starting sglang: $OFFRIG_MODEL as $OFFRIG_SERVED"
python3 -m sglang.launch_server --model-path "$OFFRIG_MODEL" --served-model-name "$OFFRIG_SERVED" \
  --host 127.0.0.1 --port "$OFFRIG_PORT" $OFFRIG_ARGS > /workspace/offrig/engine.log 2>&1
echo "[offrig] engine exited with status $?" | tee -a /workspace/offrig/engine.log
sleep infinity
"#;

/// Where a job's files and logs live on the pod (on the volume).
pub const JOB_DIR: &str = "/workspace/job";

/// Runs as `bash -c` in place of a job image's entrypoint: sshd and nothing else. The
/// work arrives over ssh (`offrig exec`), so the pod serves nothing and nothing is
/// tunnelled; the container sleeps until it is terminated.
pub const BOOTSTRAP_JOB: &str = r#"set -u
mkdir -p /workspace/offrig /workspace/job /workspace/hf
exec > >(tee -a /workspace/offrig/bootstrap.log) 2>&1
echo "[offrig] bootstrap start $(date -u +%FT%TZ)"
export DEBIAN_FRONTEND=noninteractive
if ! command -v sshd >/dev/null 2>&1; then
  apt-get update -qq && apt-get install -y -qq --no-install-recommends openssh-server >/dev/null
fi
mkdir -p /run/sshd /root/.ssh && chmod 700 /root/.ssh
printf '%s\n' "${PUBLIC_KEY:-}" > /root/.ssh/authorized_keys && chmod 600 /root/.ssh/authorized_keys
ssh-keygen -A >/dev/null
/usr/sbin/sshd -o PasswordAuthentication=no -o PermitRootLogin=prohibit-password -o AllowTcpForwarding=no
if [ -n "${HF_TOKEN:-}" ]; then
  (umask 077; printf '%s' "$HF_TOKEN" > /root/.offrig-hf-token)
  echo "[offrig] Hugging Face token written for ssh sessions"
fi
echo "[offrig] sshd up; job pod waiting for work"
sleep infinity
"#;

/// Where a job pod's bootstrap writes the Hugging Face token (root-only), when the
/// profile names a secret. An ssh session does not inherit the container's
/// environment, so `exec` and `run` reach the token through `HF_TOKEN_PATH`.
pub const HF_TOKEN_FILE: &str = "/root/.offrig-hf-token";

/// The variables every job command runs with. `HF_TOKEN_PATH` is always set: with no
/// token configured the file does not exist and Hugging Face downloads anonymously.
pub fn job_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("HF_HOME".into(), HF_DIR.into());
    env.insert("HF_TOKEN_PATH".into(), HF_TOKEN_FILE.into());
    env.insert("OFFRIG_JOB_DIR".into(), JOB_DIR.into());
    env
}

/// The job pod's own environment: [`job_env`], plus the token as a RunPod secret
/// reference when the profile names one, which RunPod substitutes at start.
pub fn job_pod_env(j: &Job) -> BTreeMap<String, String> {
    let mut env = job_env();
    if let Some(sec) = &j.hf_token_secret {
        env.insert("HF_TOKEN".into(), format!("{{{{ RUNPOD_SECRET_{sec} }}}}"));
    }
    env
}

/// The pod's name in the config's lane: `offrig-<profile>` on the plain lane,
/// `offrig-<tag>-<profile>` on a project lane (see `lanes`).
pub fn pod_name(cfg: &Config, profile: &Profile) -> String {
    cfg.pod_name(profile)
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

/// The engine's arguments: tensor parallel and context length come from the profile,
/// so they cannot disagree with the GPUs rented; the recipe adds the rest.
pub fn engine_args(profile: &Profile, r: &Recipe) -> Vec<String> {
    let mut a = vec![
        "--tp-size".to_string(),
        profile.gpu_count.to_string(),
        "--context-length".to_string(),
        profile.context_length.to_string(),
    ];
    a.extend(r.args.iter().cloned());
    a
}

pub fn engine_env(profile: &Profile, r: &Recipe) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("OFFRIG_MODEL".into(), r.model.clone());
    env.insert(
        "OFFRIG_SERVED".into(),
        profile
            .models
            .first()
            .map(|m| m.name.clone())
            .unwrap_or_default(),
    );
    env.insert("OFFRIG_PORT".into(), REMOTE_OLLAMA_PORT.to_string());
    env.insert("OFFRIG_ARGS".into(), engine_args(profile, r).join(" "));
    env.insert("HF_HOME".into(), HF_DIR.into());
    env.insert("OFFRIG_STAGE_KEY".into(), crate::stage::stage_key(&r.model));
    if let Some(sec) = &r.hf_token_secret {
        // RunPod substitutes the secret at start; the token is never in this spec.
        env.insert("HF_TOKEN".into(), format!("{{{{ RUNPOD_SECRET_{sec} }}}}"));
    }
    env
}

/// Env vars that make a pod identifiable in RunPod's console (issue #26). They are
/// added to the profile's own env; none of the profile's variables uses these names.
pub const ENV_LANE: &str = "OFFRIG_LANE";
pub const ENV_PLAN: &str = "OFFRIG_PLAN";
pub const ENV_DEADLINE: &str = "OFFRIG_DEADLINE";

pub fn pod_create(cfg: &Config, profile: &Profile) -> PodCreate {
    let on_volume = profile.network_volume_id.is_some();
    let (image, start, mut env) = match (&profile.job, &profile.recipe) {
        (Some(j), _) => (j.image.clone(), BOOTSTRAP_JOB, job_pod_env(j)),
        (None, Some(r)) => match r.engine {
            Engine::Sglang => (r.image.clone(), BOOTSTRAP_SGLANG, engine_env(profile, r)),
        },
        (None, None) => (OLLAMA_IMAGE.to_string(), BOOTSTRAP, ollama_env(profile)),
    };
    // The lane is in the pod's name already; the variable shows it in the console too.
    // The plain lane (CLI, app, Zed) says so. No free-text note goes on a pod.
    env.insert(
        ENV_LANE.into(),
        cfg.lane_tag.clone().unwrap_or_else(|| "plain".into()),
    );
    PodCreate {
        name: pod_name(cfg, profile),
        image_name: image,
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
        data_center_ids: profile.data_center_id.iter().cloned().collect(),
        allowed_cuda_versions: profile
            .effective_min_cuda()
            .as_deref()
            .and_then(cuda_at_least)
            .unwrap_or_default(),
        docker_entrypoint: vec!["bash".into(), "-c".into()],
        docker_start_cmd: vec![start.into()],
        env,
    }
}

/// The pod create for a plan: the profile's pod, but renting only from what the plan
/// recorded. The plan's GPU list is the one `offrig_plan` priced (filtered by price,
/// memory and fallback), and its CUDA floor is the one it was made with; a plan that
/// recorded neither (made before they existed) falls back to the profile's. A plan's
/// `container_disk_gb` replaces the profile's container disk size.
pub fn pod_create_for_plan(
    cfg: &Config,
    profile: &Profile,
    gpu_types: &[String],
    min_cuda: Option<&str>,
    container_disk_gb: Option<u32>,
) -> PodCreate {
    let mut body = pod_create(cfg, profile);
    if let Some(gb) = container_disk_gb {
        body.container_disk_in_gb = gb;
    }
    if !gpu_types.is_empty() {
        body.gpu_type_ids = gpu_types.to_vec();
    }
    if let Some(v) = min_cuda.and_then(cuda_at_least) {
        body.allowed_cuda_versions = v;
    }
    body
}

/// Mark a plan's pod with its plan id and deadline (UTC, ISO 8601), added to the env it
/// already carries. A plan whose worst case is not committed has no deadline to show.
pub fn mark_plan(body: &mut PodCreate, plan: &crate::store::Plan) {
    body.env.insert(ENV_PLAN.into(), plan.id.to_string());
    if let Some(d) = plan.deadline() {
        body.env
            .insert(ENV_DEADLINE.into(), crate::cost::iso_utc(d));
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
    fn a_lane_pod_is_named_for_its_lane_and_is_as_closed_as_the_plain_one() {
        let plain = Config::default();
        let lane = crate::lanes::Lane {
            tag: Some("aspire-si".into()),
            ssh_alias: "offrig-aspire-si".into(),
            tunnel_port: crate::lanes::LANE_PORT_BASE,
        };
        let cfg = plain.in_lane(&lane).expect("lane config");
        for name in ["small", "medium", "frontier", "jam"] {
            let a = pod_create(&plain, plain.profile(name).expect("profile"));
            let b = pod_create(&cfg, cfg.profile(name).expect("profile"));
            assert_eq!(a.name, format!("offrig-{name}"));
            assert_eq!(b.name, format!("offrig-aspire-si-{name}"));
            // Same pod apart from its name: only ssh exposed, the engine on loopback.
            assert_eq!(b.ports, ["22/tcp"], "{name}");
            // The same env apart from the lane marker.
            let mut a_env = a.env.clone();
            a_env.insert(ENV_LANE.into(), "aspire-si".into());
            assert_eq!(b.env, a_env, "{name}");
            assert_eq!(a.env[ENV_LANE], "plain", "{name}");
            assert_eq!(b.docker_start_cmd, a.docker_start_cmd, "{name}");
        }
    }

    #[test]
    fn pod_serves_requests_in_parallel_slots() {
        let cfg = Config::default();
        for name in ["small", "medium"] {
            let p = cfg.profile(name).expect("profile");
            assert!(p.recipe.is_none(), "{name} stays on Ollama");
            assert_eq!(ollama_env(p)["OLLAMA_NUM_PARALLEL"], "4", "{name}");
        }
        // A config saved before the field existed still gets slots.
        let mut v = serde_json::to_value(cfg.profile("small").expect("small")).expect("json");
        v.as_object_mut().expect("object").remove("parallel");
        let old: Profile = serde_json::from_value(v).expect("old profile");
        assert_eq!(old.parallel, 4);
    }

    #[test]
    fn a_recipe_pod_serves_its_engine_on_loopback_with_the_token_as_a_secret() {
        let cfg = Config::default();
        let mut p = cfg.profile("frontier").expect("frontier").clone();
        let r = p.recipe.as_mut().expect("the frontier tier runs a recipe");
        r.hf_token_secret = Some("hf_token".into());
        let body = pod_create(&cfg, &p);
        assert_eq!(body.ports, ["22/tcp"], "only ssh is exposed");
        assert_eq!(body.image_name, p.recipe.as_ref().expect("recipe").image);
        assert!(!body.image_name.ends_with(":latest"));
        assert_eq!(body.env["OFFRIG_PORT"], "11434");
        assert!(
            BOOTSTRAP_SGLANG.contains("--host 127.0.0.1"),
            "never 0.0.0.0"
        );
        let args = &body.env["OFFRIG_ARGS"];
        assert!(args.starts_with("--tp-size 4 --context-length "), "{args}");
        assert_eq!(body.env["HF_TOKEN"], "{{ RUNPOD_SECRET_hf_token }}");
        assert!(
            !body.env.values().any(|v| v.starts_with("hf_")),
            "no raw token"
        );
        let sshd = BOOTSTRAP_SGLANG.find("/usr/sbin/sshd").expect("sshd");
        let engine = BOOTSTRAP_SGLANG
            .find("sglang.launch_server")
            .expect("engine");
        assert!(sshd < engine, "sshd first, so a failed engine can be read");
        assert!(BOOTSTRAP_SGLANG.contains("AllowTcpForwarding=local"));
    }

    #[test]
    fn rehearsal_profiles_share_the_frontier_engine() {
        let cfg = Config::default();
        let frontier = cfg
            .profile("frontier")
            .expect("frontier")
            .recipe
            .clone()
            .expect("recipe");
        for name in ["frontier-mini", "frontier-mini-awq"] {
            let p = cfg.profile(name).expect("rehearsal profile");
            let r = p.recipe.as_ref().expect("recipe");
            assert_eq!(
                (r.engine, &r.image, &r.args),
                (frontier.engine, &frontier.image, &frontier.args),
                "{name}"
            );
            assert_eq!(p.gpu_count, 1);
            assert!(
                p.gpu_type_ids[0].starts_with("NVIDIA RTX PRO 6000"),
                "{name} runs on the frontier's GPU type"
            );
        }
        assert!(frontier.model.contains("AWQ"));
        assert!(
            cfg.profile("frontier-mini-awq")
                .expect("awq")
                .recipe
                .as_ref()
                .expect("r")
                .model
                .contains("AWQ"),
            "the AWQ rehearsal exercises the frontier's 4-bit MoE kernels"
        );
        assert!(
            !frontier.args.iter().any(|a| a.starts_with("fp8")),
            "no fp8 KV on sm_120 by default"
        );
    }

    #[test]
    fn a_staged_profile_launches_in_its_volumes_data_center_and_goes_offline() {
        let cfg = Config::default();
        let mut p = cfg.profile("frontier").expect("frontier").clone();
        p.network_volume_id = Some("vol1".into());
        p.data_center_id = Some("EUR-IS-1".into());
        let body = pod_create(&cfg, &p);
        assert_eq!(body.data_center_ids, ["EUR-IS-1"]);
        assert_eq!(body.network_volume_id.as_deref(), Some("vol1"));
        assert_eq!(body.volume_in_gb, None);
        assert_eq!(
            body.env["OFFRIG_STAGE_KEY"],
            "QuantTrio__Qwen3-Coder-480B-A35B-Instruct-AWQ"
        );
        let marker = BOOTSTRAP_SGLANG
            .find("staged/$OFFRIG_STAGE_KEY")
            .expect("marker check");
        let engine = BOOTSTRAP_SGLANG
            .find("sglang.launch_server")
            .expect("engine");
        assert!(marker < engine && BOOTSTRAP_SGLANG.contains("HF_HUB_OFFLINE=1"));
        assert!(
            pod_create(&cfg, cfg.profile("frontier").expect("f"))
                .data_center_ids
                .is_empty(),
            "unstaged profiles take any data center"
        );
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
    fn a_job_pod_runs_only_sshd_on_its_pinned_image_and_forwards_nothing() {
        let cfg = Config::default();
        let p = cfg.profile("job").expect("job profile");
        assert!(p.models.is_empty(), "a job pod serves nothing");
        let body = pod_create(&cfg, p);
        assert_eq!(body.ports, ["22/tcp"], "only ssh is exposed");
        assert_eq!(body.image_name, crate::config::JOB_IMAGE);
        assert!(!body.image_name.ends_with(":latest"));
        assert_eq!(body.docker_start_cmd, [BOOTSTRAP_JOB]);
        assert_eq!(body.env["HF_HOME"], HF_DIR);
        assert!(
            !body.env.keys().any(|k| k.starts_with("OLLAMA")),
            "no model server"
        );
        assert!(
            BOOTSTRAP_JOB.contains("AllowTcpForwarding=no"),
            "no tunnel of any kind"
        );
        assert!(!BOOTSTRAP_JOB.contains("ollama") && !BOOTSTRAP_JOB.contains("sglang"));
        assert!(BOOTSTRAP_JOB.contains("PasswordAuthentication=no"));
    }

    #[test]
    fn a_job_pod_gets_the_token_as_a_secret_and_its_commands_reach_it_by_file() {
        let cfg = Config::default();
        let plain = pod_create(&cfg, cfg.profile("job").expect("job profile"));
        assert!(!plain.env.contains_key("HF_TOKEN"), "no secret, no token");
        let mut p = cfg.profile("job").expect("job profile").clone();
        p.job.as_mut().expect("job").hf_token_secret = Some("hf_read".into());
        let body = pod_create(&cfg, &p);
        assert_eq!(body.env["HF_TOKEN"], "{{ RUNPOD_SECRET_hf_read }}");
        // ssh sessions do not inherit the container's environment: commands get the
        // file's path, never the token or the secret reference.
        let cmd = job_env();
        assert_eq!(cmd["HF_TOKEN_PATH"], HF_TOKEN_FILE);
        assert!(!cmd.contains_key("HF_TOKEN"));
        assert!(BOOTSTRAP_JOB.contains(&format!(
            "umask 077; printf '%s' \"$HF_TOKEN\" > {HF_TOKEN_FILE}"
        )));
        assert!(
            !BOOTSTRAP_JOB.contains("echo \"$HF_TOKEN"),
            "the token is never logged"
        );
        let mut bad = p.clone();
        bad.job.as_mut().expect("job").hf_token_secret = Some("hf read".into());
        assert!(
            bad.job
                .as_ref()
                .expect("job")
                .validate("t", 0, false)
                .is_err()
        );
    }

    #[test]
    fn job_pods_land_only_on_hosts_that_run_their_cuda_build() {
        let cfg = Config::default();
        // jam: the image's own floor, CUDA 12.8 or newer.
        let jam = pod_create(&cfg, cfg.profile("jam").expect("jam profile"));
        assert_eq!(
            jam.allowed_cuda_versions,
            ["13.0", "12.9", "12.8"],
            "a CUDA 12.8 image needs a 12.8 driver or newer"
        );
        let v = serde_json::to_value(&jam).expect("json");
        assert_eq!(v["allowedCudaVersions"][2], "12.8", "RunPod's field name");
        // job: the profile's own floor (CUDA 13) is newer than the image's and wins.
        let job = pod_create(&cfg, cfg.profile("job").expect("job profile"));
        assert_eq!(job.allowed_cuda_versions, ["13.0"], "issue #9");
        let medium = pod_create(&cfg, cfg.profile("medium").expect("medium"));
        assert!(
            medium.allowed_cuda_versions.is_empty(),
            "unchanged for Ollama"
        );
    }

    #[test]
    fn a_profile_min_cuda_applies_to_any_profile_and_the_newer_floor_wins() {
        let mut cfg = Config::default();
        for p in &mut cfg.profiles {
            if p.name == "medium" {
                p.min_cuda = Some("12.9".into());
            }
            if p.name == "jam" {
                p.min_cuda = Some("12.4".into()); // older than the image's 12.8
            }
        }
        let medium = pod_create(&cfg, cfg.profile("medium").expect("medium"));
        assert_eq!(medium.allowed_cuda_versions, ["13.0", "12.9"]);
        let jam = pod_create(&cfg, cfg.profile("jam").expect("jam"));
        assert_eq!(
            jam.allowed_cuda_versions,
            ["13.0", "12.9", "12.8"],
            "the image's 12.8 is newer than 12.4, so it stays the floor"
        );
    }

    #[test]
    fn a_cuda_runtime_rents_only_hosts_of_its_major() {
        let mut cfg = Config::default();
        for p in &mut cfg.profiles {
            if p.name == "jam" {
                p.cuda_runtime = Some("13.4".into()); // newer than any RunPod host
            }
        }
        let jam = pod_create(&cfg, cfg.profile("jam").expect("jam"));
        assert_eq!(
            jam.allowed_cuda_versions,
            ["13.0"],
            "a 13.4 build runs on a CUDA 13 driver, never on the image's 12.8 floor"
        );
        let v = serde_json::to_value(&jam).expect("json");
        assert_eq!(v["allowedCudaVersions"], serde_json::json!(["13.0"]));
    }

    #[test]
    fn a_plan_rents_from_its_own_gpu_list_and_cuda_floor() {
        let cfg = Config::default();
        let job = cfg.profile("job").expect("job profile");
        let only = vec!["NVIDIA RTX PRO 6000 Blackwell Server Edition".to_string()];
        let body = pod_create_for_plan(&cfg, job, &only, Some("13.0"), None);
        assert_eq!(body.gpu_type_ids, only, "not the profile's six cards");
        assert_eq!(body.allowed_cuda_versions, ["13.0"]);
        let v = serde_json::to_value(&body).expect("json");
        assert_eq!(v["gpuTypeIds"].as_array().map(Vec::len), Some(1));
        // A plan that stored nothing falls back to the profile's.
        let old = pod_create_for_plan(&cfg, job, &[], None, None);
        assert_eq!(old, pod_create(&cfg, job));
    }

    fn plan(committed_at: Option<i64>) -> crate::store::Plan {
        crate::store::Plan {
            id: 17,
            profile: "job".into(),
            gpu_count: 1,
            gpu_types: vec![],
            max_hours: 2.5,
            max_price_hr: 2.0,
            worst_case: 5.0,
            state: "committed".into(),
            pod_id: None,
            created_at: 0,
            note: Some("free text that must not reach the pod".into()),
            committed_at,
            started_at: None,
        }
    }

    #[test]
    fn a_plan_marks_its_pod_with_lane_plan_and_deadline_and_keeps_the_rest_of_the_env() {
        let cfg = Config::default();
        for name in ["small", "frontier", "job"] {
            let p = cfg.profile(name).expect("profile");
            let plain = pod_create(&cfg, p);
            let mut body = pod_create_for_plan(&cfg, p, &[], None, None);
            mark_plan(&mut body, &plan(Some(1_709_208_000)));
            assert_eq!(body.env[ENV_LANE], "plain", "{name}");
            assert_eq!(body.env[ENV_PLAN], "17", "{name}");
            // committed + 2.5 h
            assert_eq!(body.env[ENV_DEADLINE], "2024-02-29T14:30:00Z", "{name}");
            for (k, v) in &plain.env {
                assert_eq!(body.env.get(k), Some(v), "{name}: {k} survives");
            }
            assert_eq!(body.env.len(), plain.env.len() + 2, "{name}: two added");
            let json = serde_json::to_string(&body.env).expect("json");
            assert!(!json.contains("free text"), "no note on the pod: {json}");
        }
    }

    #[test]
    fn an_uncommitted_plan_has_no_deadline_to_put_on_the_pod() {
        let cfg = Config::default();
        let p = cfg.profile("job").expect("job");
        let mut body = pod_create(&cfg, p);
        mark_plan(&mut body, &plan(None));
        assert_eq!(body.env[ENV_PLAN], "17");
        assert!(!body.env.contains_key(ENV_DEADLINE));
    }

    #[test]
    fn the_container_disk_is_the_profiles_unless_the_plan_overrides_it() {
        let cfg = Config::default();
        let job = cfg.profile("job").expect("job profile");
        assert_eq!(job.container_disk_gb, 60);
        let v = serde_json::to_value(pod_create(&cfg, job)).expect("json");
        assert_eq!(
            v["containerDiskInGb"], 60,
            "the profile's, as RunPod names it"
        );
        // A plan's size replaces it in the create body, and nothing else changes.
        let big = pod_create_for_plan(&cfg, job, &[], None, Some(400));
        let v = serde_json::to_value(&big).expect("json");
        assert_eq!(v["containerDiskInGb"], 400);
        let mut same = big.clone();
        same.container_disk_in_gb = job.container_disk_gb;
        assert_eq!(same, pod_create(&cfg, job));
        // The volume the work dir lives on is separate and untouched.
        assert_eq!(v["volumeInGb"], 200);
    }

    #[test]
    fn the_jam_pod_is_a_job_pod_on_a_cheap_card() {
        let cfg = Config::default();
        let p = cfg.profile("jam").expect("jam profile");
        let body = pod_create(&cfg, p);
        assert_eq!(body.name, "offrig-jam");
        assert_eq!(body.docker_start_cmd, [BOOTSTRAP_JOB], "sshd only");
        assert_eq!(body.ports, ["22/tcp"]);
        assert_eq!(body.gpu_count, 1);
        assert_eq!(body.gpu_type_ids[0], "NVIDIA A40");
        assert!(
            !body
                .gpu_type_ids
                .iter()
                .any(|g| g.contains("PRO 6000") || g.contains("H100")),
            "a singing render never rents a training card"
        );
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
