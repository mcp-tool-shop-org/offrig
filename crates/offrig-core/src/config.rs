//! offrig's own settings: `<config dir>/offrig/config.toml`. It holds no secrets; the
//! RunPod key stays in the `RUNPOD_API_KEY` environment variable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The pinned pod image. Moving it is a reviewed change, never a floating `latest`.
pub const OLLAMA_IMAGE: &str = "ollama/ollama:0.35.0";
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
    /// Keep weights on a network volume so a new pod skips the download.
    #[serde(default)]
    pub network_volume_id: Option<String>,
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
            volume_gb: 30,
            container_disk_gb: 30,
            context_length: 32_768,
            // Not qwen3:8b: that one is in this rig's local Ollama, and a pod model
            // must never share a name with a local one.
            models: vec![model("qwen3:4b", 2.5, false)],
            wait_for_gpu_minutes: 0,
            parallel: 4,
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
            volume_gb: 150,
            container_disk_gb: 30,
            context_length: 65_536,
            models: vec![
                model("qwen3-coder:30b-a3b-q8_0", 32.0, false),
                model("gpt-oss:120b", 65.0, false),
            ],
            wait_for_gpu_minutes: 0,
            parallel: 4,
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
            volume_gb: 400,
            container_disk_gb: 40,
            context_length: 65_536,
            models: vec![model("qwen3-coder:480b", 290.0, false)],
            // 4x comes and goes within minutes; wait for it rather than settle.
            wait_for_gpu_minutes: 120,
            parallel: 4,
        },
    ]
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

pub fn config_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| Error::Config("no config directory on this system".into()))?;
    Ok(dir.join("offrig").join("config.toml"))
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
