//! The workflow both front ends drive: launch a pod, reach it, fill it with models,
//! wire Zed to it, and shut it down. Progress goes out through a callback so the
//! CLI can print it and the app can draw it.

use std::time::{Duration, Instant};

use crate::config::{Config, Profile};
use crate::error::{Error, Result};
use crate::ollama::Ollama;
use crate::remote::{self, PullState};
use crate::runpod::{Pod, RunPod};
use crate::spec;
use crate::sshconfig::{self, HostEntry};
use crate::tunnel::Tunnel;
use crate::zed::{self, DefaultModel, ZedModel};

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Step(String),
    Pod(Box<Pod>),
    Pull { model: String, state: PullState },
    Warn(String),
}

pub struct Session {
    pub rp: RunPod,
    pub cfg: Config,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ZedOutcome {
    pub settings: std::path::PathBuf,
    pub models: Vec<ZedModel>,
    pub previous_default: Option<DefaultModel>,
    pub api_key_env_ready: bool,
}

/// Launch-time limits. Pulling the image and installing sshd dominates.
pub const POD_READY_TIMEOUT: Duration = Duration::from_secs(15 * 60);
pub const SSH_READY_TIMEOUT: Duration = Duration::from_secs(8 * 60);
pub const OLLAMA_READY_TIMEOUT: Duration = Duration::from_secs(3 * 60);

impl Session {
    pub fn new(cfg: Config) -> Result<Self> {
        cfg.validate()?;
        Ok(Self {
            rp: RunPod::from_env()?,
            cfg,
        })
    }

    pub fn with_client(cfg: Config, rp: RunPod) -> Self {
        Self { rp, cfg }
    }

    /// The live offrig pod for a profile, matched by its name.
    pub fn current_pod(&self, profile: &Profile) -> Result<Option<Pod>> {
        let name = spec::pod_name(profile);
        Ok(self
            .rp
            .list_pods()?
            .into_iter()
            .find(|p| p.name == name && p.desired_status != "TERMINATED"))
    }

    /// Refuse plans that cannot work before any money is spent.
    pub fn plan_check(&self, profile: &Profile) -> Result<()> {
        let need = profile.total_model_gb();
        let disk = if profile.network_volume_id.is_some() {
            None
        } else {
            Some(f64::from(profile.volume_gb))
        };
        if let Some(disk) = disk
            && need * 1.05 > disk
        {
            return Err(Error::Config(format!(
                "profile {} needs about {need:.0} GB of weights but its volume is {disk:.0} GB",
                profile.name
            )));
        }
        for m in &profile.models {
            remote::validate_model_name(&m.name)?;
        }
        Ok(())
    }

    pub fn launch(&self, profile: &Profile, on: &mut dyn FnMut(Event)) -> Result<Pod> {
        self.plan_check(profile)?;
        if let Some(p) = self.current_pod(profile)? {
            on(Event::Step(format!("{} is already up ({})", p.name, p.id)));
            return self.wait_ready(&p.id, on);
        }
        let body = spec::pod_create(&self.cfg, profile);
        on(Event::Step(format!(
            "creating {} on {}x {}",
            body.name,
            body.gpu_count,
            body.gpu_type_ids.join(" | ")
        )));
        let pod = self.rp.create_pod(&body)?;
        on(Event::Step(format!(
            "pod {} created at ${:.2}/hr on {}",
            pod.id,
            pod.cost_per_hr,
            pod.gpu_type().unwrap_or("a matching GPU")
        )));
        self.wait_ready(&pod.id, on)
    }

    /// Wait for the SSH endpoint, write the alias, and wait for sshd to answer.
    pub fn wait_ready(&self, pod_id: &str, on: &mut dyn FnMut(Event)) -> Result<Pod> {
        let started = Instant::now();
        let deadline = started + POD_READY_TIMEOUT;
        let mut next_note = started;
        let pod = loop {
            let pod = self.rp.get_pod(pod_id)?;
            if pod.ssh_endpoint().is_some() {
                break pod;
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout(format!(
                    "pod {pod_id} to get an ssh endpoint"
                )));
            }
            if Instant::now() >= next_note {
                on(Event::Step(format!(
                    "waiting for the pod to pull its image and get a public address ({}s)",
                    started.elapsed().as_secs()
                )));
                next_note = Instant::now() + Duration::from_secs(30);
            }
            std::thread::sleep(Duration::from_secs(5));
        };
        on(Event::Pod(Box::new(pod.clone())));
        self.write_ssh(&pod)?;
        on(Event::Step(format!(
            "ssh alias {} written; waiting for sshd",
            self.cfg.ssh_alias
        )));
        remote::wait_for_ssh(&self.cfg.ssh_alias, SSH_READY_TIMEOUT)?;
        on(Event::Step("ssh is up".into()));
        Ok(pod)
    }

    pub fn write_ssh(&self, pod: &Pod) -> Result<()> {
        let (host, port) = pod.ssh_endpoint().ok_or_else(|| Error::NoSshEndpoint {
            name: pod.name.clone(),
        })?;
        sshconfig::apply(&HostEntry {
            alias: self.cfg.ssh_alias.clone(),
            host,
            port,
            identity_file: self.cfg.identity_file.clone(),
            label: format!("{} ({})", pod.name, pod.id),
        })
    }

    /// Open the tunnel and wait for the pod's Ollama to answer through it.
    pub fn open_tunnel(&self, on: &mut dyn FnMut(Event)) -> Result<Tunnel> {
        let mut tunnel = Tunnel::start(
            &self.cfg.ssh_alias,
            self.cfg.tunnel_port,
            Duration::from_secs(40),
        )?;
        on(Event::Step(format!(
            "tunnel up on 127.0.0.1:{}",
            self.cfg.tunnel_port
        )));
        let ollama = Ollama::new(&self.cfg.tunnel_base_url());
        let deadline = Instant::now() + OLLAMA_READY_TIMEOUT;
        loop {
            match ollama.version() {
                Ok(v) => {
                    on(Event::Step(format!(
                        "pod Ollama {v} answering through the tunnel"
                    )));
                    return Ok(tunnel);
                }
                Err(e) if Instant::now() >= deadline => {
                    return Err(Error::Timeout(format!(
                        "pod Ollama through the tunnel ({e})"
                    )));
                }
                Err(_) => {
                    if !tunnel.is_alive() {
                        return Err(Error::Ssh(format!("tunnel died: {}", tunnel.last_error())));
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }

    /// Pull every profile model that is missing, on the pod, and wait for them.
    pub fn ensure_models(&self, profile: &Profile, on: &mut dyn FnMut(Event)) -> Result<()> {
        let ollama = Ollama::new(&self.cfg.tunnel_base_url());
        let have: Vec<String> = ollama.tags()?.into_iter().map(|t| t.name).collect();
        let mut pending: Vec<String> = profile
            .models
            .iter()
            .map(|m| m.name.clone())
            .filter(|n| !have.iter().any(|h| h == n || h == &format!("{n}:latest")))
            .collect();
        if pending.is_empty() {
            on(Event::Step("all profile models are on the pod".into()));
            return Ok(());
        }
        let alias = &self.cfg.ssh_alias;
        for m in &pending {
            match remote::pull_state(alias, m)? {
                PullState::Running { .. } => on(Event::Step(format!("{m}: pull already running"))),
                _ => {
                    remote::pull_start(alias, m)?;
                    on(Event::Step(format!("{m}: pull started on the pod")));
                }
            }
        }
        while !pending.is_empty() {
            std::thread::sleep(Duration::from_secs(4));
            let mut still = Vec::new();
            for m in pending {
                let state = remote::pull_state(alias, &m)?;
                on(Event::Pull {
                    model: m.clone(),
                    state: state.clone(),
                });
                match state {
                    PullState::Done => on(Event::Step(format!("{m}: pulled"))),
                    PullState::Failed(e) => {
                        return Err(Error::Ollama(format!("pull of {m} failed: {e}")));
                    }
                    PullState::NotStarted => {
                        return Err(Error::Ollama(format!("pull of {m} vanished")));
                    }
                    PullState::Running { .. } => still.push(m),
                }
            }
            pending = still;
        }
        Ok(())
    }

    /// Zed model entries for the profile's models, using what Ollama says each can do.
    pub fn zed_models(&self, profile: &Profile) -> Result<Vec<ZedModel>> {
        let ollama = Ollama::new(&self.cfg.tunnel_base_url());
        profile
            .models
            .iter()
            .map(|m| {
                let info = ollama.info(&m.name)?;
                let ctx = info
                    .context_length
                    .map_or(profile.context_length, |c| c.min(profile.context_length));
                Ok(ZedModel {
                    name: m.name.clone(),
                    display_name: format!("RunPod · {}", m.name),
                    max_tokens: ctx,
                    tools: info.tools && m.tools,
                    images: info.images && m.images,
                })
            })
            .collect()
    }

    /// Write offrig's provider into Zed's settings, optionally make `default` the agent's
    /// model, and make sure Zed has the key variable it insists on.
    pub fn configure_zed(&self, models: &[ZedModel], default: Option<&str>) -> Result<ZedOutcome> {
        let path = zed::settings_path()?;
        let text = zed::read_settings(&path)?;
        let local = zed::local_ollama_models(&text, &path)?;
        if let Some(clash) = models.iter().find(|m| local.contains(&m.name)) {
            return Err(Error::Guard(format!(
                "{} is also in Zed's local Ollama list; remove it there first so the name cannot run locally",
                clash.name
            )));
        }
        let mut out = zed::apply_provider(
            &text,
            &path,
            &self.cfg.zed_provider,
            &self.cfg.zed_api_url(),
            models,
        )?;
        let previous_default = zed::default_model(&text, &path)?;
        if let Some(model) = default {
            out = zed::set_default_model(
                &out,
                &path,
                &DefaultModel {
                    provider: self.cfg.zed_provider.clone(),
                    model: model.to_string(),
                },
            )?;
        }
        if out != text {
            zed::write_settings(&path, &out)?;
        }
        let mut api_key_env_ready = zed::api_key_env_present(&self.cfg.zed_provider);
        if !api_key_env_ready {
            zed::set_api_key_env(&self.cfg.zed_provider)?;
            api_key_env_ready = zed::api_key_env_present(&self.cfg.zed_provider);
        }
        Ok(ZedOutcome {
            settings: path,
            models: models.to_vec(),
            previous_default,
            api_key_env_ready,
        })
    }

    /// Take offrig out of Zed: drop the provider and restore the previous default model.
    pub fn unconfigure_zed(&self, restore_default: Option<&DefaultModel>) -> Result<()> {
        let path = zed::settings_path()?;
        let text = zed::read_settings(&path)?;
        let mut out = zed::remove_provider(&text, &path, &self.cfg.zed_provider)?;
        if let Some(cur) = zed::default_model(&out, &path)?
            && cur.provider == self.cfg.zed_provider
            && let Some(prev) = restore_default
        {
            out = zed::set_default_model(&out, &path, prev)?;
        }
        if out != text {
            zed::write_settings(&path, &out)?;
        }
        Ok(())
    }

    /// Terminate the pod. Weights on a network volume survive; a pod volume does not.
    pub fn shutdown(&self, pod: &Pod, on: &mut dyn FnMut(Event)) -> Result<()> {
        self.rp.delete_pod(&pod.id)?;
        on(Event::Step(format!("terminated {} ({})", pod.name, pod.id)));
        Ok(())
    }
}
