//! The app's background thread. It owns the session, the tunnel and the idle
//! tracker; the UI sends it `Cmd`s and draws the `Update`s it sends back.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use eframe::egui;
use offrig_core::config::{Config, Profile};
use offrig_core::cost::{Idle, IdleTracker};
use offrig_core::error::chain;
use offrig_core::guard::{self, Check};
use offrig_core::ollama::{ChatCheck, Ollama, Tag};
use offrig_core::remote::{self, GpuStat, PullState};
use offrig_core::runpod::{Account, GpuOffer, Pod, RunPod};
use offrig_core::session::{Event, Session};
use offrig_core::tunnel::Tunnel;
use offrig_core::zed::{self, DefaultModel};
use offrig_core::{Error, Result, spec};

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Refresh,
    Offers(u32),
    SetProfile(String),
    SetAutoStop(Option<u32>),
    Launch,
    OpenTunnel,
    CloseTunnel,
    Pull(String),
    ConfigureZed { default_model: Option<String> },
    RemoveZed,
    Guard,
    Check(String),
    OpenZedRemote,
}

#[derive(Debug, Clone)]
pub enum Update {
    Config(Box<Config>),
    Account(Account),
    Pods(Vec<Pod>),
    Offers(u32, Vec<GpuOffer>),
    Log(String),
    Error(String),
    Busy(Option<String>),
    Pull { model: String, state: PullState },
    Tunnel(bool),
    PodModels(Vec<Tag>),
    Guard(Vec<Check>),
    Check(ChatCheck),
    Gpu(Vec<GpuStat>),
    Idle(Idle),
    Zed(ZedStatus),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ZedStatus {
    pub provider_models: Vec<String>,
    pub default: Option<DefaultModel>,
    pub key_env: bool,
}

/// Sends updates to the UI and wakes it up.
#[derive(Clone)]
pub struct Outbox {
    tx: Sender<Update>,
    ctx: egui::Context,
}

impl Outbox {
    pub fn new(tx: Sender<Update>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }

    pub fn send(&self, u: Update) {
        // The UI is gone when this fails; the worker exits on its next receive.
        let _ = self.tx.send(u);
        self.ctx.request_repaint();
    }

    fn log(&self, s: impl Into<String>) {
        self.send(Update::Log(s.into()));
    }
}

pub struct Worker {
    session: Session,
    out: Outbox,
    tunnel: Option<Tunnel>,
    want_tunnel: bool,
    idle: Option<IdleTracker>,
    previous_default: Option<DefaultModel>,
}

const REFRESH_EVERY: Duration = Duration::from_secs(15);
const GPU_EVERY: Duration = Duration::from_secs(60);

pub fn spawn(cfg: Config, out: Outbox, rx: Receiver<Cmd>) {
    std::thread::spawn(move || {
        let session = match Session::new(cfg.clone()) {
            Ok(s) => s,
            Err(e) => {
                out.send(Update::Error(chain(&e)));
                out.send(Update::Config(Box::new(cfg)));
                return;
            }
        };
        let idle = cfg
            .auto_stop_idle_minutes
            .map(|m| IdleTracker::new(Duration::from_secs(u64::from(m) * 60)));
        let mut w = Worker {
            session,
            out,
            tunnel: None,
            want_tunnel: false,
            idle,
            previous_default: None,
        };
        w.run(rx);
    });
}

/// Terminate a pod from a fresh thread, so it works while the worker is busy
/// (for example mid-pull).
pub fn shutdown_now(pod: Pod, out: Outbox) {
    std::thread::spawn(move || {
        let result = RunPod::from_env().and_then(|rp| rp.delete_pod(&pod.id));
        match result {
            Ok(()) => out.log(format!("terminated {} ({})", pod.name, pod.id)),
            Err(e) => out.send(Update::Error(format!(
                "shutting down {}: {}",
                pod.name,
                chain(&e)
            ))),
        }
    });
}

impl Worker {
    fn profile(&self) -> Result<Profile> {
        Ok(self.session.cfg.active()?.clone())
    }

    fn run(&mut self, rx: Receiver<Cmd>) {
        self.out
            .send(Update::Config(Box::new(self.session.cfg.clone())));
        self.do_cmd(Cmd::Refresh);
        self.do_cmd(Cmd::Offers(self.profile().map_or(1, |p| p.gpu_count)));
        let mut next_refresh = Instant::now() + REFRESH_EVERY;
        let mut next_gpu = Instant::now() + GPU_EVERY;
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(cmd) => self.do_cmd(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            let now = Instant::now();
            if now >= next_refresh {
                next_refresh = now + REFRESH_EVERY;
                self.quiet(Cmd::Refresh);
            }
            self.supervise_tunnel();
            if now >= next_gpu {
                next_gpu = now + GPU_EVERY;
                self.sample_gpus();
            }
        }
    }

    /// Run a command, reporting failure as an error update.
    fn do_cmd(&mut self, cmd: Cmd) {
        let label = busy_label(&cmd);
        if let Some(l) = &label {
            self.out.send(Update::Busy(Some(l.clone())));
        }
        if let Err(e) = self.handle(cmd) {
            self.out.send(Update::Error(chain(&e)));
        }
        if label.is_some() {
            self.out.send(Update::Busy(None));
        }
    }

    /// Background refreshes report failures to the log only, not as error banners.
    fn quiet(&mut self, cmd: Cmd) {
        if let Err(e) = self.handle(cmd) {
            self.out.log(format!("refresh failed: {}", chain(&e)));
        }
    }

    fn pod(&self) -> Result<Option<Pod>> {
        self.session.current_pod(&self.profile()?)
    }

    fn require_pod(&self) -> Result<Pod> {
        let p = self.profile()?;
        self.session
            .current_pod(&p)?
            .ok_or_else(|| Error::PodNotFound(spec::pod_name(&p)))
    }

    fn events(&self) -> impl FnMut(Event) + '_ {
        move |e| match e {
            Event::Step(s) | Event::Warn(s) => self.out.log(s),
            Event::Pod(p) => {
                self.out
                    .log(format!("pod {} reachable at {:?}", p.id, p.ssh_endpoint()))
            }
            Event::Pull { model, state } => self.out.send(Update::Pull { model, state }),
        }
    }

    fn handle(&mut self, cmd: Cmd) -> Result<()> {
        match cmd {
            Cmd::Refresh => {
                self.out.send(Update::Account(self.session.rp.account()?));
                self.out.send(Update::Pods(self.session.rp.list_pods()?));
                self.out.send(Update::Zed(zed_status(&self.session.cfg)?));
                Ok(())
            }
            Cmd::Offers(n) => {
                self.out
                    .send(Update::Offers(n, self.session.rp.gpu_offers(n)?));
                Ok(())
            }
            Cmd::SetProfile(name) => {
                self.session.cfg.profile(&name)?;
                self.session.cfg.active_profile = name;
                self.session.cfg.save()?;
                self.out
                    .send(Update::Config(Box::new(self.session.cfg.clone())));
                let n = self.profile()?.gpu_count;
                self.handle(Cmd::Offers(n))
            }
            Cmd::SetAutoStop(m) => {
                self.session.cfg.auto_stop_idle_minutes = m;
                self.session.cfg.save()?;
                self.idle = m.map(|m| IdleTracker::new(Duration::from_secs(u64::from(m) * 60)));
                self.out
                    .send(Update::Config(Box::new(self.session.cfg.clone())));
                Ok(())
            }
            Cmd::Launch => {
                let p = self.profile()?;
                let pod = self.session.launch(&p, &mut self.events())?;
                self.out.send(Update::Pods(self.session.rp.list_pods()?));
                self.open_tunnel()?;
                self.session.ensure_models(&p, &mut self.events())?;
                self.refresh_models()?;
                self.configure_zed(None)?;
                self.run_guard(&pod)?;
                self.out.log(format!("{} is ready", pod.name));
                Ok(())
            }
            Cmd::OpenTunnel => {
                let pod = self.require_pod()?;
                self.session.write_ssh(&pod)?;
                self.open_tunnel()?;
                self.refresh_models()
            }
            Cmd::CloseTunnel => {
                self.want_tunnel = false;
                self.tunnel = None;
                self.out.send(Update::Tunnel(false));
                self.out.log("tunnel closed");
                Ok(())
            }
            Cmd::Pull(model) => {
                remote::validate_model_name(&model)?;
                let pod = self.require_pod()?;
                self.session.write_ssh(&pod)?;
                let alias = self.session.cfg.ssh_alias.clone();
                remote::pull_start(&alias, &model)?;
                self.out.log(format!("{model}: pull started on the pod"));
                loop {
                    std::thread::sleep(Duration::from_secs(4));
                    let state = remote::pull_state(&alias, &model)?;
                    self.out.send(Update::Pull {
                        model: model.clone(),
                        state: state.clone(),
                    });
                    match state {
                        PullState::Done => break,
                        PullState::Failed(e) => {
                            return Err(Error::Ollama(format!("pull of {model} failed: {e}")));
                        }
                        PullState::NotStarted => {
                            return Err(Error::Ollama(format!("pull of {model} vanished")));
                        }
                        PullState::Running { .. } => {}
                    }
                }
                self.out.log(format!("{model}: pulled"));
                if self.tunnel.is_some() {
                    self.refresh_models()?;
                }
                Ok(())
            }
            Cmd::ConfigureZed { default_model } => self.configure_zed(default_model),
            Cmd::RemoveZed => {
                self.session
                    .unconfigure_zed(self.previous_default.as_ref())?;
                self.out.log("offrig's provider removed from Zed");
                self.out.send(Update::Zed(zed_status(&self.session.cfg)?));
                Ok(())
            }
            Cmd::Guard => {
                let pod = self.require_pod()?;
                self.run_guard(&pod)
            }
            Cmd::Check(model) => {
                self.ensure_tunnel()?;
                let c = Ollama::new(&self.session.cfg.tunnel_base_url()).chat_check(&model)?;
                if let Some(t) = self.idle.as_mut() {
                    t.touch();
                }
                self.out.send(Update::Check(c));
                Ok(())
            }
            Cmd::OpenZedRemote => {
                let pod = self.require_pod()?;
                self.session.write_ssh(&pod)?;
                let target = format!("ssh://{}/workspace", self.session.cfg.ssh_alias);
                std::process::Command::new("zed")
                    .arg(&target)
                    .spawn()
                    .map_err(|e| Error::Io {
                        what: "launching zed".into(),
                        source: e,
                    })?;
                self.out.log(format!("opened {target} in Zed"));
                Ok(())
            }
        }
    }

    fn open_tunnel(&mut self) -> Result<()> {
        self.want_tunnel = true;
        self.tunnel = None;
        let t = self.session.open_tunnel(&mut self.events())?;
        self.tunnel = Some(t);
        self.out.send(Update::Tunnel(true));
        Ok(())
    }

    fn ensure_tunnel(&mut self) -> Result<()> {
        if self.tunnel.as_mut().is_some_and(Tunnel::is_alive) {
            return Ok(());
        }
        let pod = self.require_pod()?;
        self.session.write_ssh(&pod)?;
        self.open_tunnel()
    }

    fn refresh_models(&mut self) -> Result<()> {
        let tags = Ollama::new(&self.session.cfg.tunnel_base_url()).tags()?;
        self.out.send(Update::PodModels(tags));
        Ok(())
    }

    fn configure_zed(&mut self, default_model: Option<String>) -> Result<()> {
        self.ensure_tunnel()?;
        let p = self.profile()?;
        let models = self.session.zed_models(&p)?;
        let out = self
            .session
            .configure_zed(&models, default_model.as_deref())?;
        if default_model.is_some() && self.previous_default.is_none() {
            self.previous_default = out
                .previous_default
                .filter(|d| d.provider != self.session.cfg.zed_provider);
        }
        self.out.log(format!(
            "Zed provider written to {}",
            out.settings.display()
        ));
        self.out.send(Update::Zed(zed_status(&self.session.cfg)?));
        Ok(())
    }

    fn run_guard(&mut self, pod: &Pod) -> Result<()> {
        self.ensure_tunnel()?;
        let facts = guard::gather(&self.session.cfg, pod, &zed::settings_path()?)?;
        self.out.send(Update::Guard(guard::evaluate(&facts)));
        Ok(())
    }

    /// Reopen a dropped tunnel while the user wants one and the pod is up.
    fn supervise_tunnel(&mut self) {
        let alive = self.tunnel.as_mut().is_some_and(Tunnel::is_alive);
        if alive || !self.want_tunnel {
            return;
        }
        let why = self
            .tunnel
            .as_ref()
            .map(Tunnel::last_error)
            .unwrap_or_default();
        self.tunnel = None;
        self.out.send(Update::Tunnel(false));
        self.out.log(format!("tunnel dropped ({why}); reopening"));
        let result = self.pod().and_then(|p| match p {
            Some(pod) => {
                self.session.write_ssh(&pod)?;
                self.open_tunnel()
            }
            None => {
                self.want_tunnel = false;
                self.out.log("the pod is gone; tunnel closed");
                Ok(())
            }
        });
        if let Err(e) = result {
            self.out.log(format!("tunnel not reopened: {}", chain(&e)));
        }
    }

    fn sample_gpus(&mut self) {
        if self.tunnel.is_none() {
            return;
        }
        let stats = remote::gpu_stats(&self.session.cfg.ssh_alias).unwrap_or_default();
        self.out.send(Update::Gpu(stats.clone()));
        let Some(tracker) = self.idle.as_mut() else {
            return;
        };
        let verdict = tracker.observe(&stats, Instant::now());
        let limit_min = tracker.limit.as_secs() / 60;
        self.out.send(Update::Idle(verdict));
        if verdict == Idle::Stop
            && let Ok(Some(pod)) = self.pod()
        {
            self.out.log(format!(
                "every GPU idle for {limit_min} min: terminating {}",
                pod.name
            ));
            self.want_tunnel = false;
            self.tunnel = None;
            self.out.send(Update::Tunnel(false));
            if let Err(e) = self.session.rp.delete_pod(&pod.id) {
                self.out
                    .send(Update::Error(format!("auto-stop failed: {}", chain(&e))));
            }
        }
    }
}

fn busy_label(cmd: &Cmd) -> Option<String> {
    match cmd {
        Cmd::Refresh | Cmd::Offers(_) | Cmd::SetProfile(_) | Cmd::SetAutoStop(_) => None,
        Cmd::Launch => Some("Launching the pod".into()),
        Cmd::OpenTunnel => Some("Opening the tunnel".into()),
        Cmd::CloseTunnel => None,
        Cmd::Pull(m) => Some(format!("Pulling {m}")),
        Cmd::ConfigureZed { .. } => Some("Writing Zed settings".into()),
        Cmd::RemoveZed => Some("Removing the Zed provider".into()),
        Cmd::Guard => Some("Running guard checks".into()),
        Cmd::Check(m) => Some(format!("Testing {m}")),
        Cmd::OpenZedRemote => None,
    }
}

pub fn zed_status(cfg: &Config) -> Result<ZedStatus> {
    let path = zed::settings_path()?;
    let text = zed::read_settings(&path)?;
    let provider_models = zed::read_provider(&text, &path, &cfg.zed_provider)?
        .and_then(|p| p["available_models"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["name"].as_str().map(str::to_string))
        .collect();
    Ok(ZedStatus {
        provider_models,
        default: zed::default_model(&text, &path)?,
        key_env: zed::api_key_env_present(&cfg.zed_provider),
    })
}
