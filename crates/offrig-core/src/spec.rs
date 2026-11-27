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

/// Seconds past a plan's deadline when the on-pod backstop timer fires.
/// The host watchdog polls every 60s, so 120s gives it first turn to terminate
/// the pod and close the plan's books while bounding spend if the host dies.
pub const SELF_STOP_MARGIN_SECS: i64 = 120;
/// Path on the pod where bootstrap records whether the on-pod self-stop backstop armed.
pub const SELF_STOP_STATUS_FILE: &str = "/workspace/offrig/self-stop.status";
/// Line prefix written by bootstrap and read over SSH after pod boot.
pub const SELF_STOP_PREFIX: &str = "offrig self-stop: ";
/// Status recorded when bootstrap's self-stop status cannot be read over SSH.
pub const SELF_STOP_UNKNOWN_PREFIX: &str = "self-stop status unknown";
const SELF_STOP_STOPPED_LINE: &str =
    "offrig self-stop: STOPPED NOT TERMINATED: volume still billing";

const SELF_STOP_START: &str = "# offrig-self-stop-begin\n";
const SELF_STOP_END: &str = "# offrig-self-stop-end\n";

/// Build a warning message when the pod's self-stop status line reports `UNAVAILABLE`,
/// `STOPPED NOT TERMINATED`, or could not be read (`self-stop status unknown`).
pub fn self_stop_warning(status: Option<&str>) -> Option<String> {
    let line = status?.trim();
    if line.contains("UNAVAILABLE") {
        Some(format!(
            "{line}; on-pod backstop is not armed, so keep the host watchdog running until offrig_shutdown"
        ))
    } else if line.contains("STOPPED NOT TERMINATED") {
        Some(format!(
            "{line}; on-pod backstop could only stop the pod, so terminate it to stop volume billing"
        ))
    } else if line.starts_with(SELF_STOP_UNKNOWN_PREFIX) {
        Some(format!(
            "{line}; on-pod backstop could not be verified, so keep the host watchdog running until offrig_shutdown"
        ))
    } else {
        None
    }
}

/// Background timer placed in a planned pod's bootstrap script as a backstop
/// beneath the host watchdog.
///
/// Prefers Runpod's preinstalled `runpodctl` CLI when present, and falls back to
/// calling Runpod's REST API v2 directly via `python3` (`ssl` + `urllib.request`)
/// first (falling through to `curl` at fire time if Python fails) or `curl` second,
/// using the container's pod-injected `RUNPOD_POD_ID` and pod-scoped `RUNPOD_API_KEY`
/// (`https://docs.runpod.io/pods/references/environment-variables` and
/// `https://docs.runpod.io/pods/templates/environment-variables`, section
/// "Runpod-provided variables"; `https://docs.runpod.io/pods/manage-pods`, sections
/// "Stop a Pod" and "Terminate a Pod";
/// `https://docs.runpod.io/api-reference-v2/pods/terminate-a-pod`, section
/// "Terminate a pod"; `https://docs.runpod.io/runpodctl/overview`, section
/// "Install Runpod CLI locally"). Arms only when one of those tools is available
/// and both `RUNPOD_POD_ID` and `RUNPOD_API_KEY` are set; otherwise writes
/// `UNAVAILABLE (<missing>)`. Retries delete/termination before ever attempting a
/// last-resort stop, writes `STOPPED NOT TERMINATED: volume still billing` to the
/// status file before any last-resort stop, never prints or logs `RUNPOD_API_KEY`,
/// and never places a host/studio API key on the pod.
pub fn self_stop_timer(deadline: i64) -> String {
    self_stop_timer_in(
        deadline,
        STATE_DIR,
        SELF_STOP_STATUS_FILE,
        crate::runpod::REST_BASE,
        2,
    )
}

fn self_stop_timer_in(
    deadline: i64,
    state_dir: &str,
    status_file: &str,
    api_base: &str,
    retry_base_secs: u64,
) -> String {
    let fire_at = deadline.saturating_add(SELF_STOP_MARGIN_SECS);
    // Single-quote-free Python script so it sits inside `'...'` cleanly under `sh`.
    let py_api = r#"import json, os, ssl, sys, time, urllib.parse, urllib.request
mode, base, base_sleep = sys.argv[1], sys.argv[2].rstrip("/"), float(sys.argv[3])
pod = urllib.parse.quote(os.environ["RUNPOD_POD_ID"], safe="")
hdrs = {"Authorization": "Bearer " + os.environ["RUNPOD_API_KEY"], "User-Agent": "offrig-self-stop"}
def call(method, suffix, payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    h = dict(hdrs)
    if data is not None:
        h["Content-Type"] = "application/json"
    req = urllib.request.Request(base + suffix, data=data, headers=h, method=method)
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.status
if mode == "probe":
    try:
        st = call("GET", "/pods/" + pod)
    except Exception as exc:
        st = getattr(exc, "code", type(exc).__name__)
    sys.stderr.write("[offrig] self-stop boot GET /pods/" + pod + ": " + str(st) + "\n")
    sys.exit(0)
if mode == "stop":
    try:
        call("POST", "/pods/" + pod + "/action", {"action": "stop"})
        sys.exit(0)
    except Exception as exc:
        sys.stderr.write("[offrig] self-stop stop failed (" + str(getattr(exc, "code", type(exc).__name__)) + ")\n")
        sys.exit(1)
for i in range(4):
    try:
        call("DELETE", "/pods/" + pod)
        sys.exit(0)
    except Exception as exc:
        code = getattr(exc, "code", type(exc).__name__)
        sys.stderr.write("[offrig] self-stop DELETE " + str(i + 1) + "/4 failed (" + str(code) + ")\n")
        if i < 3:
            w = base_sleep * (2 ** i)
            hdr = getattr(exc, "headers", None)
            ra = hdr.get("Retry-After") if code == 429 and hdr is not None else None
            if ra is not None:
                try:
                    w = max(0.0, min(float(ra.strip()), 60.0))
                except Exception:
                    pass
            if w > 0:
                time.sleep(w)
try:
    call("POST", "/pods/" + pod + "/action", {"action": "terminate"})
    sys.exit(0)
except Exception as exc:
    sys.stderr.write("[offrig] self-stop terminate action failed (" + str(getattr(exc, "code", type(exc).__name__)) + ")\n")
    sys.exit(1)"#;
    format!(
        "{SELF_STOP_START}\
         _offrig_ss_tool=\"\"; \
         if command -v runpodctl >/dev/null 2>&1; then _offrig_ss_tool=\"runpodctl\"; \
         elif command -v python3 >/dev/null 2>&1 && python3 -c \"import ssl, urllib.request\" >/dev/null 2>&1; then _offrig_ss_tool=\"python3\"; \
         elif command -v curl >/dev/null 2>&1; then _offrig_ss_tool=\"curl\"; fi; \
         _offrig_ss_missing=\"\"; \
         if [ -z \"$_offrig_ss_tool\" ]; then _offrig_ss_missing=\"runpodctl/python3/curl\"; fi; \
         if [ -z \"${{RUNPOD_POD_ID:-}}\" ]; then _offrig_ss_missing=\"${{_offrig_ss_missing:+$_offrig_ss_missing, }}RUNPOD_POD_ID\"; fi; \
         if [ -z \"${{RUNPOD_API_KEY:-}}\" ]; then _offrig_ss_missing=\"${{_offrig_ss_missing:+$_offrig_ss_missing, }}RUNPOD_API_KEY\"; fi; \
         mkdir -p \"{state_dir}\"; \
         if [ -z \"$_offrig_ss_missing\" ]; then \
           printf '%s\\n' \"{SELF_STOP_PREFIX}armed fire_at={fire_at} via $_offrig_ss_tool\" | tee \"{status_file}\"; \
           (_offrig_py_api() {{ \
             python3 -c '{py_api}' \"$1\" \"{api_base}\" \"$2\"; \
           }}; \
           _offrig_curl_req() {{ \
             if [ -n \"$3\" ]; then \
               printf 'header = \"Authorization: Bearer %s\"\\n' \"$RUNPOD_API_KEY\" | curl -s -D - -o /dev/null -w '\\n__CODE__:%{{http_code}}' --max-time 30 -X \"$1\" -H 'Content-Type: application/json' --data \"$3\" -K - \"{api_base}/pods/${{RUNPOD_POD_ID}}$2\" 2>/dev/null || true; \
             else \
               printf 'header = \"Authorization: Bearer %s\"\\n' \"$RUNPOD_API_KEY\" | curl -s -D - -o /dev/null -w '\\n__CODE__:%{{http_code}}' --max-time 30 -X \"$1\" -K - \"{api_base}/pods/${{RUNPOD_POD_ID}}$2\" 2>/dev/null || true; \
             fi; \
           }}; \
           _offrig_curl_term() {{ \
             _d={retry_base_secs}; \
             for _i in 1 2 3 4; do \
               _out=$(_offrig_curl_req DELETE \"\" \"\"); _c=\"${{_out##*__CODE__:}}\"; case \"$_c\" in [0-9][0-9][0-9]) ;; *) _c=\"000\" ;; esac; \
               case \"$_c\" in 2*) return 0 ;; esac; \
               echo \"[offrig] self-stop DELETE $_i/4 failed ($_c)\" >> \"{state_dir}/bootstrap.log\" 2>&1; \
               if [ \"$_i\" -lt 4 ]; then \
                 _w=$_d; _d=$((_d * 2)); \
                 if [ \"$_c\" = \"429\" ]; then \
                   case \"$_out\" in *[Rr][Ee][Tt][Rr][Yy]-[Aa][Ff][Tt][Ee][Rr]:*) \
                     _ra=\"${{_out#*[Rr][Ee][Tt][Rr][Yy]-[Aa][Ff][Tt][Ee][Rr]:}}\"; \
                     _ra=\"${{_ra%%[! 0-9]*}}\"; \
                     while [ \"${{_ra# }}\" != \"$_ra\" ]; do _ra=\"${{_ra# }}\"; done; \
                     _ra=\"${{_ra%%[!0-9]*}}\"; \
                     case \"$_ra\" in \"\"|*[!0-9]*) ;; *) [ \"$_ra\" -gt 60 ] && _ra=60; _w=$_ra ;; esac ;; \
                   esac; \
                 fi; \
                 if [ \"$_w\" -gt 0 ]; then sleep \"$_w\"; fi; \
               fi; \
             done; \
             _out=$(_offrig_curl_req POST \"/action\" '{{\"action\":\"terminate\"}}'); _c=\"${{_out##*__CODE__:}}\"; case \"$_c\" in 2*) return 0 ;; esac; \
             echo \"[offrig] self-stop terminate action failed (${{_c:-000}})\" >> \"{state_dir}/bootstrap.log\" 2>&1; \
             return 1; \
           }}; \
           if [ \"$_offrig_ss_tool\" = \"python3\" ]; then \
             _offrig_py_api probe {retry_base_secs} >> \"{state_dir}/bootstrap.log\" 2>&1 || true; \
           elif [ \"$_offrig_ss_tool\" = \"curl\" ]; then \
             _out=$(_offrig_curl_req GET \"\" \"\"); _c=\"${{_out##*__CODE__:}}\"; case \"$_c\" in [0-9][0-9][0-9]) ;; *) _c=\"000\" ;; esac; \
             echo \"[offrig] self-stop boot GET /pods/${{RUNPOD_POD_ID}}: $_c\" >> \"{state_dir}/bootstrap.log\" 2>&1; \
           fi; \
           now=$(date -u +%s); wait=$(({fire_at} - now)); \
           if [ \"$wait\" -gt 0 ]; then sleep \"$wait\"; fi; \
           echo \"[offrig] on-pod deadline {fire_at} reached; terminating ${{RUNPOD_POD_ID}} via $_offrig_ss_tool\" >> \"{state_dir}/bootstrap.log\" 2>&1; \
           _offrig_term=0; \
           case \"$_offrig_ss_tool\" in \
             runpodctl) \
               _d={retry_base_secs}; \
               for _i in 1 2 3 4; do \
                 if runpodctl pod delete \"$RUNPOD_POD_ID\" >> \"{state_dir}/bootstrap.log\" 2>&1 || runpodctl remove pod \"$RUNPOD_POD_ID\" >> \"{state_dir}/bootstrap.log\" 2>&1; then \
                   _offrig_term=1; break; \
                 fi; \
                 if [ \"$_i\" -lt 4 ] && [ \"$_d\" -gt 0 ]; then sleep \"$_d\"; _d=$((_d * 2)); fi; \
               done ;; \
             python3) \
               if _offrig_py_api terminate {retry_base_secs} >> \"{state_dir}/bootstrap.log\" 2>&1; then \
                 _offrig_term=1; \
               elif command -v curl >/dev/null 2>&1; then \
                 echo \"[offrig] self-stop python3 failed; falling through to curl\" >> \"{state_dir}/bootstrap.log\" 2>&1; \
                 if _offrig_curl_term; then _offrig_term=1; fi; \
               fi ;; \
             curl) \
               if _offrig_curl_term; then _offrig_term=1; fi ;; \
           esac; \
           if [ \"$_offrig_term\" -ne 1 ]; then \
             printf '%s\\n' \"{SELF_STOP_STOPPED_LINE}\" >> \"{status_file}\"; \
             echo \"[offrig] {SELF_STOP_STOPPED_LINE}\" >> \"{state_dir}/bootstrap.log\" 2>&1; \
             case \"$_offrig_ss_tool\" in \
               runpodctl) \
                 runpodctl pod stop \"$RUNPOD_POD_ID\" >> \"{state_dir}/bootstrap.log\" 2>&1 \
                 || runpodctl stop pod \"$RUNPOD_POD_ID\" >> \"{state_dir}/bootstrap.log\" 2>&1 || exit 1 ;; \
               python3) \
                 if ! _offrig_py_api stop 0 >> \"{state_dir}/bootstrap.log\" 2>&1; then \
                   if command -v curl >/dev/null 2>&1; then \
                     _out=$(_offrig_curl_req POST \"/action\" '{{\"action\":\"stop\"}}'); _c=\"${{_out##*__CODE__:}}\"; \
                     case \"$_c\" in 2*) ;; *) echo \"[offrig] self-stop stop failed (${{_c:-000}})\" >> \"{state_dir}/bootstrap.log\" 2>&1; exit 1 ;; esac; \
                   else exit 1; fi; \
                 fi ;; \
               curl) \
                 _out=$(_offrig_curl_req POST \"/action\" '{{\"action\":\"stop\"}}'); _c=\"${{_out##*__CODE__:}}\"; \
                 case \"$_c\" in 2*) ;; *) echo \"[offrig] self-stop stop failed (${{_c:-000}})\" >> \"{state_dir}/bootstrap.log\" 2>&1; exit 1 ;; esac ;; \
             esac; \
           fi) </dev/null >/dev/null 2>&1 & \
         else \
           printf '%s\\n' \"{SELF_STOP_PREFIX}UNAVAILABLE ($_offrig_ss_missing)\" | tee \"{status_file}\"; \
         fi\n\
         {SELF_STOP_END}"
    )
}

fn install_self_stop(cmd: &mut String, deadline: i64) {
    if let Some(start) = cmd.find(SELF_STOP_START)
        && let Some(end_rel) = cmd[start..].find(SELF_STOP_END)
    {
        cmd.replace_range(start..start + end_rel + SELF_STOP_END.len(), "");
    }
    let snippet = self_stop_timer(deadline);
    let anchor = "echo \"[offrig] bootstrap start $(date -u +%FT%TZ)\"\n";
    let pos = cmd
        .find(anchor)
        .map(|i| i + anchor.len())
        .or_else(|| cmd.find("set -u\n").map(|i| i + "set -u\n".len()))
        .unwrap_or(0);
    cmd.insert_str(pos, &snippet);
}

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
        gpu_count: profile.gpu_count,
        cloud_type: "SECURE".into(),
        support_public_ip: true,
        ports: vec!["22/tcp".into()],
        container_disk_in_gb: profile.container_disk_gb,
        volume_in_gb: (!on_volume).then_some(profile.volume_gb),
        network_volume_id: profile.network_volume_id.clone(),
        volume_mount_path: "/workspace".into(),
        data_center_ids: profile.data_center_id.iter().cloned().collect(),
        min_cuda_version: if profile.cuda_versions.is_some() {
            None
        } else {
            profile
                .effective_min_cuda()
                .filter(|v| cuda_at_least(v).is_some())
        },
        allowed_cuda_versions: profile.cuda_versions.clone(),
        docker_entrypoint: vec!["bash".into(), "-c".into()],
        docker_start_cmd: vec![start.into()],
        env,
        max_price_hr: None,
        try_first: None,
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
    cuda_versions: Option<&[String]>,
) -> PodCreate {
    let mut body = pod_create(cfg, profile);
    if let Some(gb) = container_disk_gb {
        body.container_disk_in_gb = gb;
    }
    if !gpu_types.is_empty() {
        body.gpu_type_ids = gpu_types.to_vec();
    }
    // An exact list replaces the floor. Sending both is a 400 from Runpod.
    if let Some(list) = cuda_versions {
        body.allowed_cuda_versions = Some(list.to_vec());
        body.min_cuda_version = None;
    } else if let Some(v) = min_cuda.filter(|v| cuda_at_least(v).is_some()) {
        body.min_cuda_version = Some(v.to_string());
        body.allowed_cuda_versions = None;
    }
    body
}

/// Mark a plan's pod with its plan id and deadline (UTC, ISO 8601), added to the env it
/// already carries, and wire the on-pod deadline timer into its startup command. A plan
/// whose worst case is not committed has no deadline to show or schedule.
pub fn mark_plan(body: &mut PodCreate, plan: &crate::store::Plan) {
    body.max_price_hr = Some(plan.max_price_hr);
    body.env.insert(ENV_PLAN.into(), plan.id.to_string());
    if let Some(d) = plan.deadline() {
        body.env
            .insert(ENV_DEADLINE.into(), crate::cost::iso_utc(d));
        if let Some(cmd) = body.docker_start_cmd.first_mut() {
            install_self_stop(cmd, d);
        }
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
            jam.min_cuda_version.as_deref(),
            Some("12.8"),
            "a CUDA 12.8 image needs a 12.8 driver or newer"
        );
        let v = serde_json::to_value(&jam).expect("json");
        assert_eq!(v["gpu"]["minCudaVersion"], "12.8", "RunPod's field name");
        // job: the profile's own floor (CUDA 13) is newer than the image's and wins.
        let job = pod_create(&cfg, cfg.profile("job").expect("job profile"));
        assert_eq!(job.min_cuda_version.as_deref(), Some("13.0"), "issue #9");
        let medium = pod_create(&cfg, cfg.profile("medium").expect("medium"));
        assert!(medium.min_cuda_version.is_none(), "unchanged for Ollama");
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
        assert_eq!(medium.min_cuda_version.as_deref(), Some("12.9"));
        let jam = pod_create(&cfg, cfg.profile("jam").expect("jam"));
        assert_eq!(
            jam.min_cuda_version.as_deref(),
            Some("12.8"),
            "the image's 12.8 is newer than 12.4, so it stays the floor"
        );
    }

    #[test]
    fn a_cuda_runtime_sends_its_host_floor_as_min_cuda_version() {
        let mut cfg = Config::default();
        let jam = cfg
            .profiles
            .iter_mut()
            .find(|p| p.name == "jam")
            .expect("jam");
        jam.cuda_runtime = Some("13.4".into());
        cfg.validate().expect("13.4 floors at a listed host");
        let body = pod_create(&cfg, cfg.profile("jam").expect("jam"));
        assert_eq!(
            body.min_cuda_version.as_deref(),
            Some("13.0"),
            "a 13.4 build floors at 13.0, newer than the image's 12.8"
        );
        assert!(body.allowed_cuda_versions.is_none());
        let v = serde_json::to_value(&body).expect("json");
        assert_eq!(v["gpu"]["minCudaVersion"], "13.0");
        assert!(
            v["gpu"].get("allowedCudaVersions").is_none(),
            "a runtime floor uses the same field as every other floor: {v}"
        );
    }

    #[test]
    fn an_accepted_pin_and_runtime_sends_the_pin_and_not_a_floor() {
        let mut cfg = Config::default();
        let medium = cfg
            .profiles
            .iter_mut()
            .find(|p| p.name == "medium")
            .expect("medium");
        medium.cuda_runtime = Some("13.4".into());
        medium.cuda_versions = Some(vec!["13.0".into()]);
        cfg.validate()
            .expect("13.0 meets the 13.4 runtime's host floor");
        let body = pod_create(&cfg, cfg.profile("medium").expect("medium"));
        assert!(body.min_cuda_version.is_none(), "a pin sends no floor");
        let v = serde_json::to_value(&body).expect("json");
        assert_eq!(v["gpu"]["allowedCudaVersions"], serde_json::json!(["13.0"]));
        assert!(v["gpu"].get("minCudaVersion").is_none(), "{v}");
    }

    #[test]
    fn a_plan_rents_from_its_own_gpu_list_and_cuda_floor() {
        let cfg = Config::default();
        let job = cfg.profile("job").expect("job profile");
        let only = vec!["NVIDIA RTX PRO 6000 Blackwell Server Edition".to_string()];
        let body = pod_create_for_plan(&cfg, job, &only, Some("13.0"), None, None);
        assert_eq!(body.gpu_type_ids, only, "not the profile's six cards");
        assert_eq!(body.min_cuda_version.as_deref(), Some("13.0"));
        let v = serde_json::to_value(&body).expect("json");
        assert_eq!(v["gpu"]["id"], only[0]);
        assert_eq!(v["gpu"]["minCudaVersion"], "13.0");
        assert!(
            v["gpu"].get("allowedCudaVersions").is_none(),
            "a floor does not also send the exact list: {v}"
        );
        // A plan that stored nothing falls back to the profile's.
        let old = pod_create_for_plan(&cfg, job, &[], None, None, None);
        assert_eq!(old, pod_create(&cfg, job));
    }

    #[test]
    fn an_exact_cuda_list_is_sent_as_allowed_cuda_versions() {
        let mut cfg = Config::default();
        let medium = cfg
            .profiles
            .iter_mut()
            .find(|p| p.name == "medium")
            .expect("medium");
        medium.cuda_versions = Some(vec!["13.0".into(), "12.8".into()]);
        let body = pod_create(&cfg, cfg.profile("medium").expect("medium"));
        assert!(body.min_cuda_version.is_none(), "a pin sends no floor");
        let v = serde_json::to_value(&body).expect("json");
        assert_eq!(
            v["gpu"]["allowedCudaVersions"],
            serde_json::json!(["13.0", "12.8"]),
            "the list is sent in the order written"
        );
        assert!(v["gpu"].get("minCudaVersion").is_none(), "{v}");

        let job = cfg.profile("job").expect("job");
        let pin = vec!["13.0".to_string()];
        let planned = pod_create_for_plan(&cfg, job, &[], Some("13.0"), None, Some(&pin));
        let sent = serde_json::to_value(&planned).expect("json");
        assert_eq!(
            sent["gpu"]["allowedCudaVersions"],
            serde_json::json!(["13.0"])
        );
        assert!(
            sent["gpu"].get("minCudaVersion").is_none(),
            "a stored pin replaces the floor: {sent}"
        );

        let floor = pod_create_for_plan(&cfg, job, &[], Some("13.0"), None, None);
        let sent = serde_json::to_value(&floor).expect("json");
        assert_eq!(sent["gpu"]["minCudaVersion"], "13.0");
        assert!(
            sent["gpu"].get("allowedCudaVersions").is_none(),
            "a floor does not send the whole list: {sent}"
        );
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
            let mut body = pod_create_for_plan(&cfg, p, &[], None, None, None);
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
        assert_eq!(v["disk"], 60, "the profile's container disk");
        // A plan's size replaces it in the create body, and nothing else changes.
        let big = pod_create_for_plan(&cfg, job, &[], None, Some(400), None);
        let v = serde_json::to_value(&big).expect("json");
        assert_eq!(v["disk"], 400);
        let mut same = big.clone();
        same.container_disk_in_gb = job.container_disk_gb;
        assert_eq!(same, pod_create(&cfg, job));
        // The volume the work dir lives on is separate and untouched.
        assert_eq!(v["mounts"]["persistent"]["size"], 200);
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

    #[test]
    fn a_stored_cuda_list_replaces_a_floor_on_the_create_body() {
        let cfg = Config::default();
        let mut mini = cfg.profile("frontier-mini").expect("frontier-mini").clone();
        mini.cuda_versions = Some(vec!["13.0".into()]);
        let v = serde_json::to_value(pod_create(&cfg, &mini)).expect("json");
        assert_eq!(v["gpu"]["allowedCudaVersions"], serde_json::json!(["13.0"]));
        assert!(
            v["gpu"].get("minCudaVersion").is_none(),
            "a pin does not also send a floor: {v}"
        );
        let pinned = pod_create_for_plan(
            &cfg,
            &mini,
            &[],
            Some("12.8"),
            None,
            Some(&["12.8".into(), "13.0".into()]),
        );
        let v = serde_json::to_value(&pinned).expect("json");
        assert_eq!(
            v["gpu"]["allowedCudaVersions"],
            serde_json::json!(["12.8", "13.0"]),
            "the plan's list, unchanged, even when a floor was also passed"
        );
        assert!(v["gpu"].get("minCudaVersion").is_none(), "{v}");
    }

    #[test]
    fn every_pod_starting_profile_carries_the_self_stop_timer_at_deadline_plus_margin() {
        let cfg = Config::default();
        assert!(!cfg.profiles.is_empty());
        let committed = 1_709_208_000;
        let p_plan = plan(Some(committed));
        let deadline = p_plan.deadline().expect("committed plan has deadline");
        let expected_fire = deadline + SELF_STOP_MARGIN_SECS;
        let mut saw_model = false;
        let mut saw_job = false;

        for profile in &cfg.profiles {
            if profile.is_job() {
                saw_job = true;
            } else {
                saw_model = true;
            }
            let mut body = pod_create_for_plan(&cfg, profile, &[], None, None, None);
            mark_plan(&mut body, &p_plan);
            // Calling mark_plan twice does not duplicate the timer.
            mark_plan(&mut body, &p_plan);

            let cmd = &body.docker_start_cmd[0];
            assert_eq!(
                cmd.matches(&format!("fire_at={expected_fire}")).count(),
                1,
                "{}: missing or duplicated self-stop timer: {cmd}",
                profile.name
            );
            assert!(
                cmd.contains("runpodctl pod delete \"$RUNPOD_POD_ID\""),
                "{}: {cmd}",
                profile.name
            );
            assert!(
                cmd.contains("runpodctl pod stop \"$RUNPOD_POD_ID\""),
                "{}: {cmd}",
                profile.name
            );
            assert!(
                cmd.contains("offrig self-stop: armed fire_at="),
                "{}: {cmd}",
                profile.name
            );
            assert!(
                cmd.contains("offrig self-stop: UNAVAILABLE ("),
                "{}: {cmd}",
                profile.name
            );
            assert!(
                cmd.contains(SELF_STOP_STATUS_FILE),
                "{}: {cmd}",
                profile.name
            );
            let timer_pos = cmd.find("fire_at=").expect("timer");
            let main_pos = cmd
                .find("exec ollama serve")
                .or_else(|| cmd.find("sglang.launch_server"))
                .or_else(|| cmd.find("sleep infinity"))
                .expect("profile main command");
            assert!(
                timer_pos < main_pos,
                "{}: self-stop timer must start before main command",
                profile.name
            );

            // Wire body also carries the timer and never exposes any studio API key.
            let wire = body.wire(&profile.gpu_type_ids[0]);
            let wire_cmd = wire["cmd"][0].as_str().expect("wire cmd");
            assert!(wire_cmd.contains(&expected_fire.to_string()));
            assert!(!body.env.contains_key("RUNPOD_API_KEY"));
            assert!(wire["env"].get("RUNPOD_API_KEY").is_none());
        }
        assert!(saw_model && saw_job, "both model and job profiles tested");
    }

    #[test]
    fn self_stop_warning_triggers_only_on_unavailable() {
        assert_eq!(self_stop_warning(None), None);
        assert_eq!(
            self_stop_warning(Some(
                "offrig self-stop: armed fire_at=1900000120 via python3"
            )),
            None
        );
        let warn = self_stop_warning(Some("offrig self-stop: UNAVAILABLE (runpodctl)"))
            .expect("must warn");
        assert!(warn.contains("UNAVAILABLE (runpodctl)"), "{warn}");
        let stopped = self_stop_warning(Some(
            "offrig self-stop: STOPPED NOT TERMINATED: volume still billing",
        ))
        .expect("stopped-not-terminated must warn");
        assert!(
            stopped.contains("STOPPED NOT TERMINATED: volume still billing"),
            "{stopped}"
        );
        let unk = self_stop_warning(Some(
            "self-stop status unknown: bootstrap wrote no self-stop status line",
        ))
        .expect("unknown must warn");
        assert!(unk.contains(SELF_STOP_UNKNOWN_PREFIX), "{unk}");
    }

    const TEST_KEY: &str = "rpa_synthetic_unit_test_secret_99";

    #[derive(Clone, Debug)]
    struct RecordedReq {
        method: String,
        path: String,
        auth: Option<String>,
        body: String,
        at: std::time::Instant,
    }

    struct LocalMock {
        url: String,
        reqs: std::sync::Arc<std::sync::Mutex<Vec<RecordedReq>>>,
    }

    impl LocalMock {
        fn start(handler: impl Fn(&RecordedReq) -> (u16, String) + Send + Sync + 'static) -> Self {
            Self::start_with_headers(move |req| {
                let (status, body) = handler(req);
                (status, Vec::new(), body)
            })
        }

        fn start_with_headers(
            handler: impl Fn(&RecordedReq) -> (u16, Vec<(String, String)>, String)
            + Send
            + Sync
            + 'static,
        ) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let url = format!("http://{}", listener.local_addr().expect("addr"));
            let reqs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = std::sync::Arc::clone(&reqs);
            let handler = std::sync::Arc::new(handler);
            std::thread::spawn(move || {
                use std::fmt::Write as _;
                use std::io::{BufRead, BufReader, Read, Write};
                for stream in listener.incoming().map_while(std::result::Result::ok) {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        continue;
                    }
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("").to_string();
                    let mut len = 0usize;
                    let mut auth = None;
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':') {
                            if k.trim().eq_ignore_ascii_case("content-length") {
                                len = v.trim().parse().unwrap_or(0);
                            } else if k.trim().eq_ignore_ascii_case("authorization") {
                                auth = Some(v.trim().to_string());
                            }
                        }
                    }
                    let mut raw = vec![0u8; len];
                    let _ = reader.read_exact(&mut raw);
                    let rec = RecordedReq {
                        method,
                        path,
                        auth,
                        body: String::from_utf8_lossy(&raw).into_owned(),
                        at: std::time::Instant::now(),
                    };
                    log.lock().expect("lock").push(rec.clone());
                    let (status, extra_headers, resp_body) = handler(&rec);
                    let mut extra = String::new();
                    for (hk, hv) in extra_headers {
                        let _ = write!(extra, "{hk}: {hv}\r\n");
                    }
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{resp_body}",
                        resp_body.len()
                    );
                    let _ = reader.get_mut().write_all(reply.as_bytes());
                }
            });
            Self { url, reqs }
        }

        fn requests(&self) -> Vec<RecordedReq> {
            self.reqs.lock().expect("lock").clone()
        }
    }

    fn find_sh() -> std::path::PathBuf {
        #[cfg(windows)]
        for candidate in [
            r"C:\Program Files\Git\bin\sh.exe",
            r"C:\Program Files\Git\usr\bin\sh.exe",
        ] {
            let p = std::path::Path::new(candidate);
            if p.exists() {
                return p.to_path_buf();
            }
        }
        #[cfg(not(windows))]
        {
            let p = std::path::Path::new("/bin/sh");
            if p.exists() {
                return p.to_path_buf();
            }
        }
        std::path::PathBuf::from("sh")
    }

    fn posix_dir(sh: &std::path::Path, dir: &std::path::Path) -> String {
        #[cfg(windows)]
        {
            let mut cmd = std::process::Command::new(sh);
            cmd.arg("-c").arg("cd \"$1\" && pwd").arg("sh").arg(dir);
            let out = crate::proc::run_with_timeout(
                &mut cmd,
                std::time::Duration::from_secs(5),
                "posix_dir",
            )
            .expect("posix_dir");
            assert!(out.success(), "posix_dir failed: {}", out.stderr);
            out.stdout.trim().to_string()
        }
        #[cfg(not(windows))]
        {
            let _ = sh;
            dir.to_str().expect("utf8 path").to_string()
        }
    }

    fn discover_tool(sh: &std::path::Path, tool: &str) -> String {
        let mut cmd = std::process::Command::new(sh);
        cmd.arg("-c").arg("command -v \"$1\"").arg("sh").arg(tool);
        let out = crate::proc::run_with_timeout(
            &mut cmd,
            std::time::Duration::from_secs(5),
            "discover_tool",
        )
        .expect("discover_tool");
        assert!(
            out.success(),
            "tool {tool} missing on host PATH: {}",
            out.stderr
        );
        out.stdout.trim().to_string()
    }

    fn write_shim(bin_dir: &std::path::Path, name: &str, body: &str) {
        let p = bin_dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("write shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
    }

    fn populate_coreutils(sh: &std::path::Path, bin_dir: &std::path::Path) {
        for tool in ["mkdir", "tee", "date", "sleep"] {
            let real = discover_tool(sh, tool);
            write_shim(bin_dir, tool, &format!("exec '{real}' \"$@\""));
        }
    }

    struct SnippetRun {
        status: String,
        bootstrap_log: String,
        stdout: String,
    }

    fn run_snippet(
        sh: &std::path::Path,
        case_dir: &std::path::Path,
        bin_dir: &std::path::Path,
        api_base: &str,
        pod_id: Option<&str>,
        api_key: Option<&str>,
    ) -> SnippetRun {
        let state_dir = case_dir.join("state");
        std::fs::create_dir_all(&state_dir).expect("state_dir");
        let state_posix = posix_dir(sh, &state_dir);
        let status_posix = format!("{state_posix}/self-stop.status");
        let bin_posix = posix_dir(sh, bin_dir);

        let snippet = self_stop_timer_in(0, &state_posix, &status_posix, api_base, 0);
        assert!(
            !snippet.contains(TEST_KEY),
            "snippet must never embed secret"
        );
        let mut syntax = std::process::Command::new(sh);
        syntax.arg("-n").arg("-c").arg(&snippet);
        let syntax_out =
            crate::proc::run_with_timeout(&mut syntax, std::time::Duration::from_secs(5), "sh -n")
                .expect("sh -n");
        assert!(
            syntax_out.success(),
            "sh -n failed: {}\nsnippet:\n{snippet}",
            syntax_out.stderr
        );

        let script = format!("PATH='{bin_posix}'; export PATH\n{snippet}\nwait\n");
        let mut cmd = std::process::Command::new(sh);
        cmd.arg("-c").arg(&script);
        match pod_id {
            Some(v) => {
                cmd.env("RUNPOD_POD_ID", v);
            }
            None => {
                cmd.env_remove("RUNPOD_POD_ID");
            }
        }
        match api_key {
            Some(v) => {
                cmd.env("RUNPOD_API_KEY", v);
            }
            None => {
                cmd.env_remove("RUNPOD_API_KEY");
            }
        }
        let out = crate::proc::run_with_timeout(
            &mut cmd,
            std::time::Duration::from_secs(15),
            "run self_stop snippet",
        )
        .expect("run snippet");
        assert!(
            out.success(),
            "snippet exited {:?}: stdout={:?} stderr={:?}",
            out.status,
            out.stdout,
            out.stderr
        );

        let status = std::fs::read_to_string(state_dir.join("self-stop.status"))
            .expect("self-stop.status written");
        let bootstrap_log =
            std::fs::read_to_string(state_dir.join("bootstrap.log")).unwrap_or_default();

        for (label, text) in [
            ("stdout", &out.stdout),
            ("stderr", &out.stderr),
            ("self-stop.status", &status),
            ("bootstrap.log", &bootstrap_log),
        ] {
            assert!(
                !text.contains(TEST_KEY),
                "secret leaked into {label}: {text}"
            );
        }

        SnippetRun {
            status,
            bootstrap_log,
            stdout: out.stdout,
        }
    }

    fn temp_case_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("offrig-self-stop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp_case_dir");
        dir
    }

    #[test]
    fn self_stop_snippet_passes_sh_syntax_check_and_embeds_no_secret() {
        let sh = find_sh();
        let snippet = self_stop_timer(1_900_000_000);
        assert!(snippet.contains(crate::runpod::REST_BASE));
        assert!(snippet.contains(STATE_DIR));
        assert!(snippet.contains(SELF_STOP_STATUS_FILE));
        assert!(!snippet.contains(TEST_KEY));

        let mut cmd = std::process::Command::new(sh);
        cmd.arg("-n").arg("-c").arg(&snippet);
        let out = crate::proc::run_with_timeout(
            &mut cmd,
            std::time::Duration::from_secs(5),
            "sh -n prod snippet",
        )
        .expect("sh -n");
        assert!(out.success(), "sh -n failed: {}", out.stderr);
        assert!(out.stderr.is_empty(), "unexpected stderr: {}", out.stderr);
    }

    #[test]
    fn self_stop_snippet_prefers_runpodctl_when_present() {
        let sh = find_sh();
        let dir = temp_case_dir("runpodctl");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        populate_coreutils(&sh, &bin);
        let calls_file = dir.join("runpodctl.calls");
        let calls_posix = posix_dir(&sh, &dir) + "/runpodctl.calls";
        write_shim(
            &bin,
            "runpodctl",
            &format!("printf '%s\\n' \"$*\" >> '{calls_posix}'\nexit 0"),
        );
        // Also provide python3 and curl so preference order is proven.
        let py = discover_tool(&sh, "python3");
        write_shim(&bin, "python3", &format!("exec '{py}' \"$@\""));
        let cu = discover_tool(&sh, "curl");
        write_shim(&bin, "curl", &format!("exec '{cu}' \"$@\""));

        let mock = LocalMock::start(|_| (204, String::new()));
        let res = run_snippet(&sh, &dir, &bin, &mock.url, Some("pod_abc"), Some(TEST_KEY));
        assert_eq!(
            res.status,
            "offrig self-stop: armed fire_at=120 via runpodctl\n"
        );
        assert_eq!(
            std::fs::read_to_string(&calls_file).expect("calls"),
            "pod delete pod_abc\n"
        );
        assert!(mock.requests().is_empty(), "runpodctl must win over HTTP");
        assert!(res.bootstrap_log.contains("via runpodctl"));
        assert!(!res.stdout.is_empty());

        // Fallback chain inside runpodctl (4x delete/remove retries -> loud warning -> stop)
        let dir2 = temp_case_dir("runpodctl-chain");
        let bin2 = dir2.join("bin");
        std::fs::create_dir_all(&bin2).expect("bin2");
        populate_coreutils(&sh, &bin2);
        let calls2 = dir2.join("runpodctl.calls");
        let calls2_posix = posix_dir(&sh, &dir2) + "/runpodctl.calls";
        write_shim(
            &bin2,
            "runpodctl",
            &format!(
                "printf '%s\\n' \"$*\" >> '{calls2_posix}'\ncase \"$1 $2\" in 'stop pod') exit 0;; *) exit 1;; esac"
            ),
        );
        let res2 = run_snippet(
            &sh,
            &dir2,
            &bin2,
            &mock.url,
            Some("pod_abc"),
            Some(TEST_KEY),
        );
        assert_eq!(
            res2.status,
            format!(
                "offrig self-stop: armed fire_at=120 via runpodctl\n{SELF_STOP_STOPPED_LINE}\n"
            )
        );
        let parsed2 = crate::remote::parse_self_stop(&res2.status);
        assert_eq!(parsed2.as_deref(), Some(SELF_STOP_STOPPED_LINE));
        assert!(self_stop_warning(parsed2.as_deref()).is_some());
        let expected_delete_rounds = "pod delete pod_abc\nremove pod pod_abc\n".repeat(4);
        assert_eq!(
            std::fs::read_to_string(&calls2).expect("calls2"),
            format!("{expected_delete_rounds}pod stop pod_abc\nstop pod pod_abc\n")
        );
        assert!(mock.requests().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn self_stop_snippet_uses_python_urllib_fallback_before_curl() {
        let sh = find_sh();
        let dir = temp_case_dir("python-ok");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        populate_coreutils(&sh, &bin);
        let py = discover_tool(&sh, "python3");
        write_shim(&bin, "python3", &format!("exec '{py}' \"$@\""));
        let curl_marker = dir.join("curl.called");
        let curl_marker_posix = posix_dir(&sh, &dir) + "/curl.called";
        write_shim(
            &bin,
            "curl",
            &format!("printf 'curl_called\\n' > '{curl_marker_posix}'\nexit 0"),
        );

        let mock = LocalMock::start(|req| {
            if req.method == "GET" {
                (200, "{}".into())
            } else {
                (204, String::new())
            }
        });
        let res = run_snippet(&sh, &dir, &bin, &mock.url, Some("pod_abc"), Some(TEST_KEY));
        assert_eq!(
            res.status,
            "offrig self-stop: armed fire_at=120 via python3\n"
        );
        assert!(!curl_marker.exists(), "python3 must run before curl");
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 2, "expected boot GET + DELETE, got {reqs:?}");
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(reqs[0].path, "/pods/pod_abc");
        assert_eq!(reqs[1].method, "DELETE");
        assert_eq!(reqs[1].path, "/pods/pod_abc");
        assert_eq!(
            reqs[1].auth.as_deref(),
            Some(&format!("Bearer {TEST_KEY}")[..])
        );
        assert!(
            res.bootstrap_log
                .contains("[offrig] self-stop boot GET /pods/pod_abc: 200"),
            "{}",
            res.bootstrap_log
        );
        assert!(!res.status.contains("boot GET"));
        assert!(res.bootstrap_log.contains("via python3"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn self_stop_snippet_uses_curl_fallback_when_runpodctl_and_python_urllib_absent() {
        let sh = find_sh();
        let cu = discover_tool(&sh, "curl");

        // curl only: boot GET 403 (with echoed secret) logged without gating, DELETE 204
        let dir = temp_case_dir("curl-only");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        populate_coreutils(&sh, &bin);
        write_shim(&bin, "curl", &format!("exec '{cu}' \"$@\""));
        let mock = LocalMock::start(|req| {
            if req.method == "GET" {
                (403, format!("forbidden {TEST_KEY}"))
            } else {
                (204, String::new())
            }
        });
        let res = run_snippet(&sh, &dir, &bin, &mock.url, Some("pod_abc"), Some(TEST_KEY));
        assert_eq!(res.status, "offrig self-stop: armed fire_at=120 via curl\n");
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 2, "expected boot GET + DELETE, got {reqs:?}");
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(reqs[0].path, "/pods/pod_abc");
        assert_eq!(reqs[1].method, "DELETE");
        assert_eq!(reqs[1].path, "/pods/pod_abc");
        assert_eq!(
            reqs[1].auth.as_deref(),
            Some(&format!("Bearer {TEST_KEY}")[..])
        );
        assert!(
            res.bootstrap_log
                .contains("[offrig] self-stop boot GET /pods/pod_abc: 403"),
            "{}",
            res.bootstrap_log
        );
        assert!(!res.status.contains("boot GET"));
        assert!(res.bootstrap_log.contains("via curl"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn self_stop_snippet_preflights_python_ssl_and_falls_through_to_curl_at_fire_time() {
        let sh = find_sh();
        let py = discover_tool(&sh, "python3");
        let cu = discover_tool(&sh, "curl");

        // Case A: broken ssl import fails preflight even though urllib.request alone imports -> arms via curl.
        let dir = temp_case_dir("broken-ssl");
        let bin = dir.join("bin");
        let badpy = dir.join("badpy");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::create_dir_all(&badpy).expect("badpy");
        std::fs::write(badpy.join("ssl.py"), "raise ImportError('broken ssl')\n").expect("ssl.py");
        let badpy_posix = posix_dir(&sh, &badpy);
        populate_coreutils(&sh, &bin);
        write_shim(
            &bin,
            "python3",
            &format!("PYTHONPATH='{badpy_posix}' exec '{py}' \"$@\""),
        );
        write_shim(&bin, "curl", &format!("exec '{cu}' \"$@\""));
        let mock = LocalMock::start(|req| {
            if req.method == "GET" {
                (200, "{}".into())
            } else {
                (204, String::new())
            }
        });
        let res = run_snippet(&sh, &dir, &bin, &mock.url, Some("pod_abc"), Some(TEST_KEY));
        assert_eq!(res.status, "offrig self-stop: armed fire_at=120 via curl\n");
        let reqs = mock.requests();
        assert_eq!(
            reqs.len(),
            2,
            "expected boot GET + DELETE via curl: {reqs:?}"
        );
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(reqs[1].method, "DELETE");

        // Case B: python3 passes preflight (arms via python3) but crashes at fire time -> falls through to curl.
        let dir2 = temp_case_dir("py-fire-fallthrough");
        let bin2 = dir2.join("bin");
        std::fs::create_dir_all(&bin2).expect("bin2");
        populate_coreutils(&sh, &bin2);
        write_shim(
            &bin2,
            "python3",
            &format!(
                "case \"$*\" in *\"import ssl, urllib.request\"*) exec '{py}' \"$@\" ;; *) exit 1 ;; esac"
            ),
        );
        write_shim(&bin2, "curl", &format!("exec '{cu}' \"$@\""));
        let mock2 = LocalMock::start(|req| {
            if req.method == "GET" {
                (200, "{}".into())
            } else {
                (204, String::new())
            }
        });
        let res2 = run_snippet(
            &sh,
            &dir2,
            &bin2,
            &mock2.url,
            Some("pod_abc"),
            Some(TEST_KEY),
        );
        assert_eq!(
            res2.status,
            "offrig self-stop: armed fire_at=120 via python3\n"
        );
        assert!(
            res2.bootstrap_log
                .contains("python3 failed; falling through to curl"),
            "{}",
            res2.bootstrap_log
        );
        let reqs2 = mock2.requests();
        assert_eq!(
            reqs2.len(),
            1,
            "expected curl DELETE 204 after py crash: {reqs2:?}"
        );
        assert_eq!(reqs2[0].method, "DELETE");
        assert_eq!(reqs2[0].path, "/pods/pod_abc");
        assert_eq!(
            reqs2[0].auth.as_deref(),
            Some(&format!("Bearer {TEST_KEY}")[..])
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn self_stop_snippet_retries_delete_honours_429_tries_terminate_and_warns_on_stop() {
        let sh = find_sh();
        let py = discover_tool(&sh, "python3");
        let cu = discover_tool(&sh, "curl");

        // Case A: 429 with Retry-After: 1 then 204, across both python3 and curl.
        for (tool, with_py) in [("python3", true), ("curl", false)] {
            let dir = temp_case_dir(&format!("retry-429-{tool}"));
            let bin = dir.join("bin");
            std::fs::create_dir_all(&bin).expect("bin");
            populate_coreutils(&sh, &bin);
            if with_py {
                write_shim(&bin, "python3", &format!("exec '{py}' \"$@\""));
            } else {
                write_shim(&bin, "curl", &format!("exec '{cu}' \"$@\""));
            }
            let del_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let del_hits_c = std::sync::Arc::clone(&del_hits);
            let mock = LocalMock::start_with_headers(move |req| {
                if req.method == "GET" {
                    return (200, Vec::new(), "{}".into());
                }
                let n = del_hits_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 {
                    (
                        429,
                        vec![("Retry-After".into(), "1".into())],
                        format!("rate limited {TEST_KEY}"),
                    )
                } else {
                    (204, Vec::new(), String::new())
                }
            });
            let res = run_snippet(&sh, &dir, &bin, &mock.url, Some("pod_abc"), Some(TEST_KEY));
            assert_eq!(
                res.status,
                format!("offrig self-stop: armed fire_at=120 via {tool}\n")
            );
            let reqs = mock.requests();
            let deletes: Vec<&RecordedReq> = reqs.iter().filter(|r| r.method == "DELETE").collect();
            assert_eq!(deletes.len(), 2, "{tool}: {reqs:?}");
            assert!(
                deletes[1].at.duration_since(deletes[0].at)
                    >= std::time::Duration::from_millis(800),
                "{tool}: must honour Retry-After: 1"
            );
            assert!(
                reqs.iter().all(|r| r.method != "POST"),
                "{tool}: 204 delete must not call action"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        // Case B: 5xx x4 on DELETE -> POST /action {"action":"terminate"} 200, across python3 and curl.
        for (tool, with_py) in [("python3", true), ("curl", false)] {
            let dir = temp_case_dir(&format!("term-action-{tool}"));
            let bin = dir.join("bin");
            std::fs::create_dir_all(&bin).expect("bin");
            populate_coreutils(&sh, &bin);
            if with_py {
                write_shim(&bin, "python3", &format!("exec '{py}' \"$@\""));
            } else {
                write_shim(&bin, "curl", &format!("exec '{cu}' \"$@\""));
            }
            let mock = LocalMock::start(|req| match req.method.as_str() {
                "GET" => (200, "{}".into()),
                "DELETE" => (502, format!("err {TEST_KEY}")),
                "POST" => (200, "{}".into()),
                _ => (400, String::new()),
            });
            let res = run_snippet(&sh, &dir, &bin, &mock.url, Some("pod_abc"), Some(TEST_KEY));
            assert_eq!(
                res.status,
                format!("offrig self-stop: armed fire_at=120 via {tool}\n")
            );
            let reqs = mock.requests();
            let deletes = reqs.iter().filter(|r| r.method == "DELETE").count();
            let posts: Vec<&RecordedReq> = reqs.iter().filter(|r| r.method == "POST").collect();
            assert_eq!(deletes, 4, "{tool}: expected 4 DELETE retries: {reqs:?}");
            assert_eq!(
                posts.len(),
                1,
                "{tool}: expected 1 terminate POST: {reqs:?}"
            );
            assert_eq!(posts[0].path, "/pods/pod_abc/action");
            let body_json: serde_json::Value =
                serde_json::from_str(&posts[0].body).expect("terminate body json");
            assert_eq!(body_json, serde_json::json!({"action": "terminate"}));
            let _ = std::fs::remove_dir_all(&dir);
        }

        // Case C: all terminate paths fail (python3 -> curl -> last-resort stop) -> loud status line.
        let dir3 = temp_case_dir("all-fail-stop");
        let bin3 = dir3.join("bin");
        std::fs::create_dir_all(&bin3).expect("bin3");
        populate_coreutils(&sh, &bin3);
        write_shim(&bin3, "python3", &format!("exec '{py}' \"$@\""));
        write_shim(&bin3, "curl", &format!("exec '{cu}' \"$@\""));
        let mock3 = LocalMock::start(|req| {
            if req.method == "GET" {
                (200, "{}".into())
            } else {
                (500, format!("echoed {TEST_KEY}"))
            }
        });
        let res3 = run_snippet(
            &sh,
            &dir3,
            &bin3,
            &mock3.url,
            Some("pod_abc"),
            Some(TEST_KEY),
        );
        assert_eq!(
            res3.status,
            format!("offrig self-stop: armed fire_at=120 via python3\n{SELF_STOP_STOPPED_LINE}\n")
        );
        let parsed3 = crate::remote::parse_self_stop(&res3.status);
        assert_eq!(parsed3.as_deref(), Some(SELF_STOP_STOPPED_LINE));
        assert!(self_stop_warning(parsed3.as_deref()).is_some());
        let reqs3 = mock3.requests();
        let dels3 = reqs3.iter().filter(|r| r.method == "DELETE").count();
        let posts3: Vec<serde_json::Value> = reqs3
            .iter()
            .filter(|r| r.method == "POST")
            .map(|r| serde_json::from_str(&r.body).expect("action json"))
            .collect();
        assert_eq!(dels3, 8, "4 python3 + 4 curl DELETE retries: {reqs3:?}");
        assert_eq!(
            posts3,
            vec![
                serde_json::json!({"action": "terminate"}),
                serde_json::json!({"action": "terminate"}),
                serde_json::json!({"action": "stop"}),
                serde_json::json!({"action": "stop"}),
            ],
            "expected python terminate -> curl terminate -> last-resort stop (python -> curl): {reqs3:?}"
        );
        let _ = std::fs::remove_dir_all(&dir3);
    }

    #[test]
    fn self_stop_snippet_reports_unavailable_when_no_tool_or_vars_missing() {
        let sh = find_sh();
        let cu = discover_tool(&sh, "curl");
        let mock = LocalMock::start(|_| (204, String::new()));

        let cases = [
            (
                "no-tool",
                false,
                Some("pod_abc"),
                Some(TEST_KEY),
                "offrig self-stop: UNAVAILABLE (runpodctl/python3/curl)\n",
            ),
            (
                "missing-key",
                true,
                Some("pod_abc"),
                None,
                "offrig self-stop: UNAVAILABLE (RUNPOD_API_KEY)\n",
            ),
            (
                "empty-key",
                true,
                Some("pod_abc"),
                Some(""),
                "offrig self-stop: UNAVAILABLE (RUNPOD_API_KEY)\n",
            ),
            (
                "missing-pod-id",
                true,
                None,
                Some(TEST_KEY),
                "offrig self-stop: UNAVAILABLE (RUNPOD_POD_ID)\n",
            ),
            (
                "missing-both-vars",
                true,
                None,
                None,
                "offrig self-stop: UNAVAILABLE (RUNPOD_POD_ID, RUNPOD_API_KEY)\n",
            ),
            (
                "missing-all",
                false,
                None,
                None,
                "offrig self-stop: UNAVAILABLE (runpodctl/python3/curl, RUNPOD_POD_ID, RUNPOD_API_KEY)\n",
            ),
        ];

        for (name, with_curl, pod_id, api_key, expected) in cases {
            let dir = temp_case_dir(name);
            let bin = dir.join("bin");
            std::fs::create_dir_all(&bin).expect("bin");
            populate_coreutils(&sh, &bin);
            if with_curl {
                write_shim(&bin, "curl", &format!("exec '{cu}' \"$@\""));
            }
            let res = run_snippet(&sh, &dir, &bin, &mock.url, pod_id, api_key);
            assert_eq!(res.status, expected, "{name}");
            assert!(
                self_stop_warning(Some(&res.status)).is_some(),
                "{name}: UNAVAILABLE line must trigger self_stop_warning"
            );
            assert!(
                mock.requests().is_empty(),
                "{name}: unavailable branch must not fire HTTP"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
