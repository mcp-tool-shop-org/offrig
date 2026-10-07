//! offrig's own settings: `<config dir>/offrig/config.toml`. It holds no secrets; the
//! RunPod key stays in the `RUNPOD_API_KEY` environment variable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The pinned pod image. Moving it is a reviewed change, never a floating `latest`.
pub const OLLAMA_IMAGE: &str = "ollama/ollama:0.35.0";
/// The pinned SGLang image for the recipe tiers. CUDA 13 builds are the line that
/// covers Blackwell (sm_120); v0.5.20 over v0.5.21, which was a day old on
/// 2026-10-03. Research and sources: docs/sidecar-design.md, phase 3b.
pub const SGLANG_IMAGE: &str = "lmsysorg/sglang:v0.5.20-cu130";
/// The pinned PyTorch image for job profiles: CUDA 12.8, the first line with Blackwell
/// (sm_120) kernels. Checked on Docker Hub 2026-10-06 (10 GB, published 2025-03-20).
pub const JOB_IMAGE: &str = "runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04";
/// The host CUDA version [`JOB_IMAGE`] needs: it is a CUDA 12.8 build.
pub const JOB_IMAGE_CUDA: &str = "12.8";
/// Ollama's port inside the pod. It listens on 127.0.0.1 only and is never exposed.
pub const REMOTE_OLLAMA_PORT: u16 = 11434;
/// The local Ollama's port on this machine. The tunnel must never use it.
pub const LOCAL_OLLAMA_PORT: u16 = 11434;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Host alias offrig manages in ~/.ssh/config. Zed and the tunnel both use it.
    pub ssh_alias: String,
    /// Private key RunPod knows (its public half is in RunPod account settings).
    pub identity_file: String,
    /// Local end of the tunnel to the pod's Ollama.
    pub tunnel_port: u16,
    /// Zed provider id under `language_models.openai_compatible`. Zed reads its key
    /// from `<ID>_API_KEY`, so it must not be "runpod" (that would hand Zed the real
    /// RunPod key).
    pub zed_provider: String,
    /// Stop the pod after this many minutes with every GPU idle. `None` disables it.
    pub auto_stop_idle_minutes: Option<u32>,
    /// Role OS checkout whose dossiers and cards define handoff roles. `None` uses
    /// only offrig's built-in game roles.
    pub role_os_dir: Option<String>,
    pub active_profile: String,
    pub profiles: Vec<Profile>,
    /// The lane this config runs in: `None` is the plain lane (the CLI, the app, Zed),
    /// `Some(tag)` is a project lane (the side-car). Never read from or written to the
    /// file; [`Config::in_lane`] sets it together with the lane's alias and port.
    #[serde(skip)]
    pub lane_tag: Option<String>,
}

/// `ROLE_OS_DIR`, else the studio's checkout if present.
pub fn default_role_os_dir() -> Option<String> {
    if let Ok(d) = std::env::var("ROLE_OS_DIR")
        && !d.trim().is_empty()
    {
        return Some(d);
    }
    let studio = Path::new("E:/AI/role-os");
    studio
        .join("dossier")
        .is_dir()
        .then(|| studio.display().to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub name: String,
    pub tier: Tier,
    /// GPU types in priority order. RunPod takes the first with free capacity.
    pub gpu_type_ids: Vec<String>,
    pub gpu_count: u32,
    /// Keep weights on a network volume so a new pod skips the download. Set by
    /// `offrig stage`, which also sets `data_center_id`.
    #[serde(default)]
    pub network_volume_id: Option<String>,
    /// The data center pods must launch in: a network volume lives in one.
    #[serde(default)]
    pub data_center_id: Option<String>,
    /// Pod-local volume size when no network volume is attached.
    pub volume_gb: u32,
    pub container_disk_gb: u32,
    /// Ollama's default context window (`OLLAMA_CONTEXT_LENGTH`), also told to Zed.
    pub context_length: u32,
    pub models: Vec<ModelEntry>,
    /// When the GPUs are not free, check every minute for up to this long before
    /// giving up. Nothing is rented while waiting. 0 fails at once.
    #[serde(default)]
    pub wait_for_gpu_minutes: u32,
    /// Requests the pod model serves at once (`OLLAMA_NUM_PARALLEL`). Each slot holds
    /// its own context window in VRAM. The 2026-10-03 rehearsal measured 40 tok/s with
    /// one slot and 102 tok/s with four on the same GPU, under 8 parallel requests.
    #[serde(default = "default_parallel")]
    pub parallel: u32,
    /// Serve with another engine than the pinned Ollama. `None` is Ollama, with
    /// `models` pulled from its registry. With a recipe, the engine downloads
    /// `recipe.model` from Hugging Face at start and serves it as `models[0].name`.
    #[serde(default)]
    pub recipe: Option<Recipe>,
    /// A job pod: no model server, only sshd on a pinned image, for work that runs
    /// on the GPU itself (a training run). Files go up with `put`, commands run with
    /// `exec`, results come back with `get`. A job profile lists no models.
    #[serde(default)]
    pub job: Option<Job>,
    /// The oldest host CUDA (driver) version this profile's work runs on, from RunPod's
    /// list ([`CUDA_VERSIONS`]). A plan passes every version at this or newer to the pod
    /// create as `allowedCudaVersions`, so the pod is never placed on an older driver
    /// (issue #9). For a job profile the image's own floor ([`Job::min_cuda`]) also
    /// applies, and the newer of the two wins ([`Profile::effective_min_cuda`]).
    #[serde(default)]
    pub min_cuda: Option<String>,
    /// The least total VRAM (all of the profile's GPUs together, in GB) a plan accepts.
    /// Offers below it are never chosen, so the fallback cards cannot silently shrink
    /// the memory the work needs. Compared with the offers' own memory figure.
    #[serde(default)]
    pub min_vram_gb: Option<u32>,
}

/// The image a job pod runs. Nothing is served and nothing is tunnelled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    /// A pinned image tag, never `latest`. It needs bash; sshd is installed if missing.
    pub image: String,
    /// The oldest host CUDA (driver) version the image runs on, from RunPod's list
    /// ([`CUDA_VERSIONS`]). The pod is placed only on a host at this version or newer:
    /// a CUDA 12.8 build on an older driver starts, then finds no GPU.
    #[serde(default)]
    pub min_cuda: Option<String>,
}

/// The host CUDA versions RunPod's `allowedCudaVersions` accepts, newest first
/// (`PodCreateInput` in https://rest.runpod.io/v1/openapi.json, read 2026-10-07).
pub const CUDA_VERSIONS: [&str; 12] = [
    "13.0", "12.9", "12.8", "12.7", "12.6", "12.5", "12.4", "12.3", "12.2", "12.1", "12.0", "11.8",
];

/// Every host CUDA version at `min` or newer, newest first. `None` for a version
/// RunPod does not list.
pub fn cuda_at_least(min: &str) -> Option<Vec<String>> {
    let at = CUDA_VERSIONS.iter().position(|v| *v == min)?;
    Some(CUDA_VERSIONS[..=at].iter().map(|v| v.to_string()).collect())
}

impl Job {
    pub fn validate(&self, profile: &str, models: usize, recipe: bool) -> Result<()> {
        let bad = |why: String| Err(Error::Config(format!("profile {profile}: {why}")));
        if !pinned(&self.image) {
            return bad(format!(
                "job image {:?} needs a pinned tag, not latest",
                self.image
            ));
        }
        if recipe {
            return bad("a profile is a job or a recipe, not both".into());
        }
        if models != 0 {
            return bad("a job profile serves no models; leave models empty".into());
        }
        if let Some(min) = &self.min_cuda
            && cuda_at_least(min).is_none()
        {
            return bad(format!(
                "min_cuda {min:?} is not one of RunPod's CUDA versions ({})",
                CUDA_VERSIONS.join(", ")
            ));
        }
        Ok(())
    }
}

/// `repo:tag` with a real tag: not empty, not `latest`, not a registry port.
fn pinned(image: &str) -> bool {
    matches!(image.rsplit_once(':'), Some((_, tag)) if !tag.is_empty() && tag != "latest" && !tag.contains('/'))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Sglang,
}

/// How a non-Ollama engine is started on the pod. Engine knowledge lives here, in
/// data, not in code (the engine-room rule in docs/sidecar-design.md).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Recipe {
    pub engine: Engine,
    /// A pinned image tag, never `latest`.
    pub image: String,
    /// Hugging Face repo id of the weights.
    pub model: String,
    /// Extra server arguments, one token each (no spaces inside a token).
    #[serde(default)]
    pub args: Vec<String>,
    /// Name of a RunPod secret holding a Hugging Face token, for gated repos. It is
    /// referenced as `{{ RUNPOD_SECRET_<name> }}`, so the token never enters the pod
    /// spec or this file.
    #[serde(default)]
    pub hf_token_secret: Option<String>,
}

impl Profile {
    pub fn engine(&self) -> Option<Engine> {
        self.recipe.as_ref().map(|r| r.engine)
    }

    /// A job pod runs commands, not a model server.
    pub fn is_job(&self) -> bool {
        self.job.is_some()
    }

    /// The host CUDA floor the pod must be placed above: the newer of the profile's own
    /// `min_cuda` and the job image's. `None` when neither is set (any host).
    pub fn effective_min_cuda(&self) -> Option<String> {
        let own = self.min_cuda.as_deref();
        let image = self.job.as_ref().and_then(|j| j.min_cuda.as_deref());
        match (own, image) {
            (Some(a), Some(b)) => Some(newer_cuda(a, b).to_string()),
            (Some(v), None) | (None, Some(v)) => Some(v.to_string()),
            (None, None) => None,
        }
    }
}

/// Parse `"12.8"` into `(12, 8)`.
pub fn parse_cuda(v: &str) -> Option<(u32, u32)> {
    let (a, b) = v.trim().split_once('.')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// The newer of two CUDA version strings. An unparseable one loses.
pub fn newer_cuda<'a>(a: &'a str, b: &'a str) -> &'a str {
    match (parse_cuda(a), parse_cuda(b)) {
        (Some(x), Some(y)) if y > x => b,
        (None, Some(_)) => b,
        _ => a,
    }
}

/// Whether a host reporting CUDA `have` meets the floor `need`. `None` when either
/// does not parse, so a missing report is never mistaken for a pass or a fail.
pub fn cuda_meets(have: &str, need: &str) -> Option<bool> {
    Some(parse_cuda(have)? >= parse_cuda(need)?)
}

fn default_parallel() -> u32 {
    4
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Small,
    Medium,
    Frontier,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelEntry {
    /// Ollama model tag, e.g. `qwen3-coder:30b-a3b-q8_0`.
    pub name: String,
    /// Weights on disk, for planning only.
    pub size_gb: f64,
    #[serde(default = "yes")]
    pub tools: bool,
    #[serde(default)]
    pub images: bool,
}

fn yes() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ssh_alias: "offrig".into(),
            identity_file: default_identity_file(),
            tunnel_port: 11435,
            zed_provider: "offrig".into(),
            auto_stop_idle_minutes: Some(30),
            role_os_dir: default_role_os_dir(),
            active_profile: "medium".into(),
            profiles: default_profiles(),
            lane_tag: None,
        }
    }
}

fn model(name: &str, size_gb: f64, images: bool) -> ModelEntry {
    ModelEntry {
        name: name.into(),
        size_gb,
        tools: true,
        images,
    }
}

impl Recipe {
    pub fn validate(&self, profile: &str, models: usize) -> Result<()> {
        let bad = |why: String| Err(Error::Config(format!("profile {profile}: {why}")));
        if !pinned(&self.image) {
            return bad(format!(
                "recipe image {:?} needs a pinned tag, not latest",
                self.image
            ));
        }
        if self.model.trim().is_empty() || self.model.contains(char::is_whitespace) {
            return bad(format!(
                "recipe model {:?} must be a Hugging Face repo id",
                self.model
            ));
        }
        if let Some(a) = self
            .args
            .iter()
            .find(|a| a.is_empty() || a.contains(char::is_whitespace))
        {
            return bad(format!("recipe arg {a:?} must be one token with no spaces"));
        }
        if let Some(sec) = &self.hf_token_secret
            && (sec.is_empty()
                || !sec
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        {
            return bad(format!(
                "hf_token_secret {sec:?} must be a RunPod secret name"
            ));
        }
        if let Some(a) = self.args.iter().find(|a| {
            [
                "--tp",
                "--tp-size",
                "--tensor-parallel-size",
                "--context-length",
                "--port",
                "--host",
            ]
            .contains(&a.split('=').next().unwrap_or(a))
        }) {
            return bad(format!(
                "recipe arg {a} is set by offrig (GPU count, context length, loopback port); remove it"
            ));
        }
        if models != 1 {
            return bad("a recipe serves exactly one model; list it once in models".into());
        }
        Ok(())
    }
}

pub fn default_profiles() -> Vec<Profile> {
    vec![
        Profile {
            name: "small".into(),
            tier: Tier::Small,
            gpu_type_ids: vec![
                "NVIDIA RTX 2000 Ada Generation".into(),
                "NVIDIA RTX A4000".into(),
                "NVIDIA RTX A4500".into(),
                "NVIDIA RTX 4000 Ada Generation".into(),
            ],
            gpu_count: 1,
            network_volume_id: None,
            data_center_id: None,
            volume_gb: 30,
            container_disk_gb: 30,
            context_length: 32_768,
            // Not qwen3:8b: that one is in this rig's local Ollama, and a pod model
            // must never share a name with a local one.
            models: vec![model("qwen3:4b", 2.5, false)],
            wait_for_gpu_minutes: 0,
            parallel: 4,
            recipe: None,
            job: None,
            min_cuda: None,
            min_vram_gb: None,
        },
        Profile {
            name: "medium".into(),
            tier: Tier::Medium,
            // 1x RTX PRO 6000 (96 GB) first; the 80 GB cards only when none is free.
            gpu_type_ids: vec![
                "NVIDIA RTX PRO 6000 Blackwell Server Edition".into(),
                "NVIDIA RTX PRO 6000 Blackwell Workstation Edition".into(),
                "NVIDIA A100-SXM4-80GB".into(),
                "NVIDIA A100 80GB PCIe".into(),
                "NVIDIA H100 NVL".into(),
                "NVIDIA H100 80GB HBM3".into(),
            ],
            gpu_count: 1,
            network_volume_id: None,
            data_center_id: None,
            volume_gb: 150,
            container_disk_gb: 30,
            context_length: 65_536,
            models: vec![
                model("qwen3-coder:30b-a3b-q8_0", 32.0, false),
                model("gpt-oss:120b", 65.0, false),
            ],
            wait_for_gpu_minutes: 0,
            parallel: 4,
            recipe: None,
            job: None,
            min_cuda: None,
            min_vram_gb: None,
        },
        Profile {
            name: "frontier".into(),
            tier: Tier::Frontier,
            // 4x RTX PRO 6000 = 384 GB: the 290 GB model plus about 90 GB for context.
            // Chosen 2026-10-02 from live offers ($8.36/hr secure); 2x B200 was not
            // rentable, and 4x A100 (320 GB) leaves too little room for context.
            gpu_type_ids: vec![
                "NVIDIA RTX PRO 6000 Blackwell Server Edition".into(),
                "NVIDIA RTX PRO 6000 Blackwell Workstation Edition".into(),
            ],
            gpu_count: 4,
            network_volume_id: None,
            data_center_id: None,
            volume_gb: 400,
            container_disk_gb: 40,
            context_length: 65_536,
            // AWQ 4-bit, 252 GB on disk (measured from the HF API 2026-10-03; the card's
            // "236" is GiB), leaves about 130 GB across the 4 GPUs for context.
            // Full-precision KV: fp8 KV corrupted output on sm_120 in reports.
            models: vec![model("qwen3-coder-480b", 252.0, false)],
            // 4x comes and goes within minutes; wait for it rather than settle.
            wait_for_gpu_minutes: 120,
            // Measured 2026-10-03 on this tier: 88 tok/s for one agent, 1,521 at 64,
            // 2,289 at 128, 4,059 at 512 (384-token replies). SGLang holds 398,526
            // tokens of KV, so with real handoff contexts of 2-8k tokens about 64 fit
            // before the cache thrashes.
            parallel: 64,
            recipe: Some(Recipe {
                engine: Engine::Sglang,
                image: SGLANG_IMAGE.into(),
                model: "QuantTrio/Qwen3-Coder-480B-A35B-Instruct-AWQ".into(),
                args: vec![
                    "--tool-call-parser".into(),
                    "qwen3_coder".into(),
                    "--mem-fraction-static".into(),
                    "0.88".into(),
                ],
                hf_token_secret: None,
            }),
            job: None,
            min_cuda: None,
            min_vram_gb: None,
        },
        rehearsal(
            "frontier-mini",
            "qwen3-coder-30b",
            31.0,
            "Qwen/Qwen3-Coder-30B-A3B-Instruct-FP8",
        ),
        rehearsal(
            "frontier-mini-awq",
            "qwen3-coder-30b-awq",
            17.0,
            "QuantTrio/Qwen3-Coder-30B-A3B-Instruct-AWQ",
        ),
        Profile {
            name: "job".into(),
            tier: Tier::Medium,
            // The medium tier's cards: 96 GB first, the 80 GB cards when none is free.
            // Enough for a 30B model in bf16 next to a small student being trained.
            gpu_type_ids: vec![
                "NVIDIA RTX PRO 6000 Blackwell Server Edition".into(),
                "NVIDIA RTX PRO 6000 Blackwell Workstation Edition".into(),
                "NVIDIA A100-SXM4-80GB".into(),
                "NVIDIA A100 80GB PCIe".into(),
                "NVIDIA H100 NVL".into(),
                "NVIDIA H100 80GB HBM3".into(),
            ],
            gpu_count: 1,
            network_volume_id: None,
            data_center_id: None,
            volume_gb: 200,
            container_disk_gb: 60,
            context_length: 0,
            models: vec![],
            wait_for_gpu_minutes: 0,
            parallel: 1,
            recipe: None,
            job: Some(Job {
                image: JOB_IMAGE.into(),
                min_cuda: Some(JOB_IMAGE_CUDA.into()),
            }),
            // The jobs this profile runs install a current vLLM, whose PyTorch is a
            // CUDA 13 build: an A100 host on a CUDA 12.8 driver cost $0.40 and failed
            // after setup (issue #9). RunPod lists 13.0 as its newest, so this is the
            // only allowed version; a plan that cannot find such a host waits or fails
            // rather than renting one that cannot run the work.
            min_cuda: Some("13.0".into()),
            min_vram_gb: None,
        },
        Profile {
            name: "jam".into(),
            tier: Tier::Small,
            // ai-jam-sessions' singing renders: SoulX-Singer (2.8 GB of weights, fp16)
            // fits any 24 GB card, so the cheap ones come first. 48 GB A40 had high
            // stock at $0.49/hr on 2026-10-07; the plan prices the dearest listed card.
            gpu_type_ids: vec![
                "NVIDIA A40".into(),
                "NVIDIA RTX A6000".into(),
                "NVIDIA RTX A5000".into(),
                "NVIDIA GeForce RTX 3090".into(),
                "NVIDIA L4".into(),
                "NVIDIA GeForce RTX 4090".into(),
            ],
            gpu_count: 1,
            network_volume_id: None,
            data_center_id: None,
            // A Python 3.10 environment with torch (about 7 GB), uv's cache, the
            // weights and the takes.
            volume_gb: 40,
            container_disk_gb: 40,
            context_length: 0,
            models: vec![],
            wait_for_gpu_minutes: 0,
            parallel: 1,
            recipe: None,
            job: Some(Job {
                image: JOB_IMAGE.into(),
                min_cuda: Some(JOB_IMAGE_CUDA.into()),
            }),
            min_cuda: None,
            min_vram_gb: None,
        },
    ]
}

/// The frontier engine path on one RTX PRO 6000, with a 30B model: the same image,
/// engine and flags, for cents instead of dollars.
fn rehearsal(name: &str, served: &str, size_gb: f64, repo: &str) -> Profile {
    Profile {
        name: name.into(),
        tier: Tier::Medium,
        gpu_type_ids: vec![
            "NVIDIA RTX PRO 6000 Blackwell Server Edition".into(),
            "NVIDIA RTX PRO 6000 Blackwell Workstation Edition".into(),
        ],
        gpu_count: 1,
        network_volume_id: None,
        data_center_id: None,
        volume_gb: 80,
        container_disk_gb: 40,
        context_length: 65_536,
        models: vec![model(served, size_gb, false)],
        wait_for_gpu_minutes: 0,
        // A 30B AWQ on one card measured 10,147 tok/s at 256 agents and still rising
        // (2026-10-03); 32 keeps real handoff contexts inside its KV cache.
        parallel: 32,
        recipe: Some(Recipe {
            engine: Engine::Sglang,
            image: SGLANG_IMAGE.into(),
            model: repo.into(),
            args: vec![
                "--tool-call-parser".into(),
                "qwen3_coder".into(),
                "--mem-fraction-static".into(),
                "0.88".into(),
            ],
            hf_token_secret: None,
        }),
        job: None,
        min_cuda: None,
        min_vram_gb: None,
    }
}

/// Prefer the key this rig registered with RunPod, then the OpenSSH default.
pub fn default_identity_file() -> String {
    let home = dirs::home_dir().unwrap_or_default();
    for name in ["runpod_rustline", "id_ed25519", "id_rsa"] {
        if home.join(".ssh").join(name).is_file() {
            return format!("~/.ssh/{name}");
        }
    }
    "~/.ssh/id_ed25519".into()
}

/// offrig's config directory: `OFFRIG_CONFIG_DIR` when set (tests and sandboxes point
/// it at a temp dir so nothing touches the real one), else `<config dir>/offrig`.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("OFFRIG_CONFIG_DIR")
        && !d.is_empty()
    {
        return Ok(PathBuf::from(d));
    }
    let dir = dirs::config_dir()
        .ok_or_else(|| Error::Config("no config directory on this system".into()))?;
    Ok(dir.join("offrig"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path()?)
    }

    /// A missing file is the default config; a malformed one is an error, never
    /// silently replaced.
    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let cfg: Config = toml::from_str(&text)
                    .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
                cfg.validate()?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::io(format!("reading {}", path.display()), e)),
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&config_path()?)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let text = toml::to_string_pretty(self).map_err(|e| Error::Config(e.to_string()))?;
        crate::fsutil::write_atomic(path, text.as_bytes())
    }

    pub fn validate(&self) -> Result<()> {
        if self.tunnel_port == LOCAL_OLLAMA_PORT {
            return Err(Error::Config(format!(
                "tunnel_port {} is the local Ollama port; pick another so a dead tunnel can never fall through to the local GPU",
                self.tunnel_port
            )));
        }
        if self.zed_provider.eq_ignore_ascii_case("runpod") {
            return Err(Error::Config(
                "zed_provider \"runpod\" would make Zed read RUNPOD_API_KEY and send it to the model server".into(),
            ));
        }
        if self.ssh_alias.is_empty() || self.ssh_alias.contains(char::is_whitespace) {
            return Err(Error::Config(format!(
                "ssh_alias {:?} must be one word",
                self.ssh_alias
            )));
        }
        for p in &self.profiles {
            if p.gpu_count == 0 || p.gpu_type_ids.is_empty() {
                return Err(Error::Config(format!(
                    "profile {} needs a gpu type and a gpu count",
                    p.name
                )));
            }
            if let Some(r) = &p.recipe {
                r.validate(&p.name, p.models.len())?;
            }
            if let Some(j) = &p.job {
                j.validate(&p.name, p.models.len(), p.recipe.is_some())?;
            }
            if let Some(min) = &p.min_cuda
                && cuda_at_least(min).is_none()
            {
                return Err(Error::Config(format!(
                    "profile {}: min_cuda {min:?} is not one of RunPod's CUDA versions ({})",
                    p.name,
                    CUDA_VERSIONS.join(", ")
                )));
            }
            if p.min_vram_gb == Some(0) {
                return Err(Error::Config(format!(
                    "profile {}: min_vram_gb must be above 0 (leave it out for no floor)",
                    p.name
                )));
            }
            if p.network_volume_id.is_some() && p.data_center_id.is_none() {
                return Err(Error::Config(format!(
                    "profile {}: a network volume needs data_center_id (the volume's data center)",
                    p.name
                )));
            }
        }
        Ok(())
    }

    pub fn profile(&self, name: &str) -> Result<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| Error::Config(format!("no profile named {name}")))
    }

    pub fn active(&self) -> Result<&Profile> {
        self.profile(&self.active_profile)
    }

    /// The name this config gives the pod for a profile: `offrig-<profile>` on the
    /// plain lane, `offrig-<tag>-<profile>` on a project lane. A pod's lane is in its
    /// name, so one lane's launch, status and shutdown never match another's pod.
    pub fn pod_name(&self, profile: &Profile) -> String {
        match &self.lane_tag {
            None => format!("offrig-{}", profile.name),
            Some(tag) => format!("offrig-{tag}-{}", profile.name),
        }
    }

    /// Whether `name` is a pod of this config's lane: exactly the name some configured
    /// profile gets here. Nothing else on the account is ever this lane's.
    pub fn owns_pod(&self, name: &str) -> bool {
        self.profiles.iter().any(|p| self.pod_name(p) == name)
    }

    /// This config as seen from `lane`: its ssh alias, its tunnel port and its pod
    /// names. Refused if the result would tunnel on the local Ollama's port.
    pub fn in_lane(&self, lane: &crate::lanes::Lane) -> Result<Config> {
        let mut c = self.clone();
        c.ssh_alias = lane.ssh_alias.clone();
        c.tunnel_port = lane.tunnel_port;
        c.lane_tag = lane.tag.clone();
        c.validate()?;
        Ok(c)
    }

    pub fn tunnel_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.tunnel_port)
    }

    /// The URL Zed is given: the tunnel's OpenAI-compatible endpoint.
    pub fn zed_api_url(&self) -> String {
        format!("{}/v1", self.tunnel_base_url())
    }
}

impl Profile {
    pub fn total_model_gb(&self) -> f64 {
        self.models.iter().map(|m| m.size_gb).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_round_trip() {
        let cfg = Config::default();
        cfg.validate().expect("default config should validate");
        let text = toml::to_string_pretty(&cfg).expect("config should serialize");
        let back: Config = toml::from_str(&text).expect("config should parse back");
        assert_eq!(cfg, back);
        assert_eq!(cfg.zed_api_url(), "http://127.0.0.1:11435/v1");
    }

    #[test]
    fn job_profiles_are_pinned_serve_nothing_and_are_never_a_recipe() {
        let cfg = Config::default();
        let job = cfg.profile("job").expect("job profile");
        assert!(job.is_job() && job.models.is_empty() && job.recipe.is_none());
        let j = job.job.clone().expect("job");
        assert!(j.validate("t", 0, false).is_ok());
        let latest = Job {
            image: "runpod/pytorch:latest".into(),
            min_cuda: None,
        };
        assert!(latest.validate("t", 0, false).is_err(), "latest refused");
        let untagged = Job {
            image: "runpod/pytorch".into(),
            min_cuda: None,
        };
        assert!(
            untagged.validate("t", 0, false).is_err(),
            "untagged refused"
        );
        assert!(j.validate("t", 1, false).is_err(), "a job serves no models");
        assert!(j.validate("t", 0, true).is_err(), "job or recipe, not both");
        let mut bad = cfg.clone();
        if let Some(p) = bad.profiles.iter_mut().find(|p| p.name == "job") {
            p.models = vec![model("qwen3:4b", 2.5, false)];
        }
        assert!(bad.validate().is_err(), "the config check runs it");
        let jobs: Vec<&str> = cfg
            .profiles
            .iter()
            .filter(|p| p.is_job())
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(jobs, ["job", "jam"], "only the job profiles are jobs");
        for name in jobs {
            let j = cfg.profile(name).expect("job").job.clone().expect("job");
            assert_eq!(
                j.min_cuda.as_deref(),
                Some(JOB_IMAGE_CUDA),
                "{name} asks for a host that runs its image"
            );
        }
        let old_cuda = Job {
            image: JOB_IMAGE.into(),
            min_cuda: Some("12.10".into()),
        };
        assert!(
            old_cuda.validate("t", 0, false).is_err(),
            "a CUDA version RunPod does not list is refused"
        );
    }

    #[test]
    fn configs_written_before_the_profile_limits_still_load() {
        // A profile as an older offrig saved it: no min_cuda, no min_vram_gb.
        let cfg = Config::default();
        let medium = cfg.profile("medium").expect("medium").clone();
        let mut text = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(
            !text.contains("min_vram_gb"),
            "unset limits are not written: {text}"
        );
        text = text.replace(
            "min_cuda = \"13.0\"
",
            "",
        );
        let back: Config = toml::from_str(&text).expect("old file loads");
        back.validate().expect("and validates");
        let job = back.profile("job").expect("job");
        assert_eq!(job.min_cuda, None, "absent means unset");
        assert_eq!(job.min_vram_gb, None);
        assert_eq!(
            job.effective_min_cuda().as_deref(),
            Some(JOB_IMAGE_CUDA),
            "the image's own floor still applies"
        );
        assert_eq!(back.profile("medium").expect("medium"), &medium);
    }

    #[test]
    fn the_job_profile_asks_for_cuda_13_and_the_limits_are_checked() {
        let cfg = Config::default();
        let job = cfg.profile("job").expect("job");
        assert_eq!(job.min_cuda.as_deref(), Some("13.0"));
        assert_eq!(job.effective_min_cuda().as_deref(), Some("13.0"));
        assert_eq!(
            cfg.profile("jam")
                .expect("jam")
                .effective_min_cuda()
                .as_deref(),
            Some(JOB_IMAGE_CUDA)
        );
        assert_eq!(cfg.profile("medium").expect("m").effective_min_cuda(), None);
        let mut bad = cfg.clone();
        bad.profiles[0].min_cuda = Some("12.10".into());
        assert!(bad.validate().is_err(), "an unlisted CUDA version");
        let mut zero = cfg.clone();
        zero.profiles[0].min_vram_gb = Some(0);
        assert!(zero.validate().is_err(), "a zero floor is a mistake");
        let mut fine = cfg;
        fine.profiles[0].min_vram_gb = Some(80);
        fine.validate().expect("a real floor");
    }

    #[test]
    fn cuda_versions_compare_as_numbers() {
        assert_eq!(cuda_meets("12.8", "12.8"), Some(true));
        assert_eq!(cuda_meets("12.10", "12.8"), Some(true), "not as text");
        assert_eq!(cuda_meets("12.4", "12.8"), Some(false));
        assert_eq!(cuda_meets("13.0", "12.9"), Some(true));
        assert_eq!(cuda_meets("garbage", "12.8"), None);
        assert_eq!(newer_cuda("12.8", "13.0"), "13.0");
        assert_eq!(newer_cuda("13.0", "12.8"), "13.0");
    }

    #[test]
    fn recipes_are_pinned_and_leave_parallelism_and_ports_to_offrig() {
        let base = Config::default()
            .profile("frontier-mini")
            .expect("rehearsal")
            .recipe
            .clone()
            .expect("recipe");
        let check = |f: &dyn Fn(&mut Recipe), models: usize| {
            let mut r = base.clone();
            f(&mut r);
            r.validate("t", models)
        };
        assert!(check(&|_| {}, 1).is_ok());
        assert!(
            check(&|r| r.image = "lmsysorg/sglang:latest".into(), 1).is_err(),
            "latest refused"
        );
        assert!(
            check(&|r| r.image = "lmsysorg/sglang".into(), 1).is_err(),
            "untagged refused"
        );
        assert!(
            check(&|r| r.args.push("--tp".into()), 1).is_err(),
            "tp comes from gpu_count"
        );
        assert!(
            check(&|r| r.args.push("--host=0.0.0.0".into()), 1).is_err(),
            "never off loopback"
        );
        assert!(check(&|r| r.args.push("two words".into()), 1).is_err());
        assert!(check(&|r| r.hf_token_secret = Some("hf token".into()), 1).is_err());
        assert!(check(&|r| r.hf_token_secret = Some("hf_token".into()), 1).is_ok());
        assert!(check(&|_| {}, 2).is_err(), "one model per recipe");
    }

    #[test]
    fn tunnel_on_local_ollama_port_is_refused() {
        let cfg = Config {
            tunnel_port: 11434,
            ..Config::default()
        };
        let err = cfg.validate().expect_err("11434 must be refused");
        assert!(err.to_string().contains("local Ollama port"));
    }

    #[test]
    fn provider_named_runpod_is_refused() {
        let cfg = Config {
            zed_provider: "RunPod".into(),
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn missing_file_is_default_and_bad_file_is_error() {
        let dir = std::env::temp_dir().join(format!("offrig-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let missing = dir.join("nope.toml");
        assert_eq!(
            Config::load_from(&missing).expect("missing is default"),
            Config::default()
        );
        let bad = dir.join("bad.toml");
        std::fs::write(&bad, "tunnel_port = \"x\"").expect("write");
        assert!(Config::load_from(&bad).is_err());
        let good = dir.join("good.toml");
        Config::default().save_to(&good).expect("save");
        assert_eq!(Config::load_from(&good).expect("load"), Config::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frontier_and_medium_lead_with_rtx_pro_6000() {
        let cfg = Config::default();
        let f = cfg.profile("frontier").expect("frontier");
        assert_eq!(f.gpu_count, 4);
        assert!(f.gpu_type_ids.iter().all(|g| g.contains("RTX PRO 6000")));
        // 4 x 96 GB must hold the largest model with room for context.
        assert!(f.total_model_gb() * 1.15 <= 4.0 * 96.0);
        let m = cfg.profile("medium").expect("medium");
        assert_eq!(m.gpu_count, 1);
        assert!(m.gpu_type_ids[0].contains("RTX PRO 6000"));
    }

    #[test]
    fn every_tier_has_a_default_profile() {
        let cfg = Config::default();
        for tier in [Tier::Small, Tier::Medium, Tier::Frontier] {
            assert!(
                cfg.profiles.iter().any(|p| p.tier == tier),
                "{tier:?} missing"
            );
        }
        assert!(cfg.active().is_ok());
    }
}
