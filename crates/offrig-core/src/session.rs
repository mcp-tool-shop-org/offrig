//! The workflow both front ends drive: launch a pod, reach it, fill it with models,
//! wire Zed to it, and shut it down. Progress goes out through a callback so the
//! CLI can print it and the app can draw it.

use std::sync::atomic::{AtomicBool, Ordering};
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
/// A recipe engine downloads its weights from Hugging Face, then loads them: about
/// 21 minutes for 252 GB at 200 MB/s plus the load. The plan deadline still rules.
pub const ENGINE_READY_TIMEOUT: Duration = Duration::from_secs(75 * 60);

/// Refuse a profile whose models already exist in the local Ollama: the same name
/// could then be served on this machine. Checked before any money is spent.
pub fn local_conflicts(profile: &Profile, local_models: &[String]) -> Result<()> {
    let clash: Vec<&str> = profile
        .models
        .iter()
        .map(|m| m.name.as_str())
        .filter(|n| {
            local_models
                .iter()
                .any(|l| l == n || *l == format!("{n}:latest"))
        })
        .collect();
    if clash.is_empty() {
        Ok(())
    } else {
        Err(Error::Guard(format!(
            "profile {} would run {} on the pod, but the local Ollama has it too; \
             remove it locally (`ollama rm`) or pick another model, so that name can never run on this GPU",
            profile.name,
            clash.join(", ")
        )))
    }
}

/// RunPod answers an exhausted GPU list with a 500 whose body says so.
pub fn is_no_capacity(e: &Error) -> bool {
    matches!(e, Error::Api { body, .. } if body.contains("no instances currently available"))
}

fn no_capacity(body: &crate::runpod::PodCreate, waited: Duration) -> Error {
    let what = format!("{}x of [{}]", body.gpu_count, body.gpu_type_ids.join(" | "));
    Error::NoCapacity(if waited.is_zero() {
        format!("RunPod has no {what} free right now; try again later or set a wait")
    } else {
        format!(
            "RunPod had no {what} free during a {} minute wait; nothing was rented",
            waited.as_secs() / 60
        )
    })
}

/// How the wait for capacity behaves. `poll` is a minute in use; tests shorten it.
#[derive(Debug, Clone, Copy)]
pub struct Wait {
    pub limit: Duration,
    pub poll: Duration,
}

impl Wait {
    pub fn none() -> Self {
        Self {
            limit: Duration::ZERO,
            poll: Duration::from_secs(60),
        }
    }

    pub fn minutes(m: u32) -> Self {
        Self {
            limit: Duration::from_secs(u64::from(m) * 60),
            poll: Duration::from_secs(60),
        }
    }
}

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
        let local = crate::ollama::Ollama::new(&format!(
            "http://127.0.0.1:{}",
            crate::config::LOCAL_OLLAMA_PORT
        ));
        // A local Ollama that is not running holds no models; that is fine.
        if let Ok(tags) = local.tags() {
            let names: Vec<String> = tags.into_iter().map(|t| t.name).collect();
            local_conflicts(profile, &names)?;
        }
        Ok(())
    }

    pub fn launch(&self, profile: &Profile, on: &mut dyn FnMut(Event)) -> Result<Pod> {
        self.launch_waiting(profile, Wait::none(), &AtomicBool::new(false), on)
    }

    /// Launch, waiting up to `wait.limit` for the profile's GPUs to come free. Nothing
    /// is rented while waiting; setting `cancel` stops the wait.
    pub fn launch_waiting(
        &self,
        profile: &Profile,
        wait: Wait,
        cancel: &AtomicBool,
        on: &mut dyn FnMut(Event),
    ) -> Result<Pod> {
        self.plan_check(profile)?;
        if let Some(p) = self.current_pod(profile)? {
            on(Event::Step(format!("{} is already up ({})", p.name, p.id)));
            return self.wait_ready(&p.id, on);
        }
        let body = spec::pod_create(&self.cfg, profile);
        let pod = self.create_when_free(&body, wait, cancel, on)?;
        on(Event::Step(format!(
            "pod {} created at ${:.2}/hr on {}",
            pod.id,
            pod.cost_per_hr,
            pod.gpu_type().unwrap_or("a matching GPU")
        )));
        self.wait_ready(&pod.id, on)
    }

    /// Whether any of the body's GPU types is free at its count. `None` when the
    /// price API cannot say (it is GraphQL, which RunPod may retire).
    fn capacity_free(&self, body: &crate::runpod::PodCreate) -> Option<bool> {
        let offers = self.rp.gpu_offers(body.gpu_count).ok()?;
        Some(
            offers
                .iter()
                .any(|o| body.gpu_type_ids.contains(&o.id) && o.price_per_hr.is_some()),
        )
    }

    /// Create the pod as soon as RunPod has the GPUs, or give up at the limit.
    pub fn create_when_free(
        &self,
        body: &crate::runpod::PodCreate,
        wait: Wait,
        cancel: &AtomicBool,
        on: &mut dyn FnMut(Event),
    ) -> Result<Pod> {
        let started = Instant::now();
        let what = format!("{}x {}", body.gpu_count, body.gpu_type_ids.join(" | "));
        let mut next_note = started;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(Error::Cancelled(format!("waiting for {what}")));
            }
            // Ask the price API first so a full market costs no create calls; if it
            // cannot answer, try the create and let RunPod say no.
            if self.capacity_free(body) != Some(false) {
                on(Event::Step(format!("creating {} on {what}", body.name)));
                match self.rp.create_pod(body) {
                    Ok(pod) => return Ok(pod),
                    Err(e) if is_no_capacity(&e) => {}
                    Err(e) => return Err(e),
                }
            }
            let waited = started.elapsed();
            if waited >= wait.limit {
                return Err(no_capacity(body, wait.limit));
            }
            if Instant::now() >= next_note {
                on(Event::Step(format!(
                    "no {what} free yet; checking every {}s, {} of {} min waited",
                    wait.poll.as_secs(),
                    waited.as_secs() / 60,
                    wait.limit.as_secs() / 60
                )));
                next_note = Instant::now() + Duration::from_secs(300).max(wait.poll);
            }
            let wake = Instant::now() + wait.poll;
            while Instant::now() < wake {
                if cancel.load(Ordering::SeqCst) {
                    return Err(Error::Cancelled(format!("waiting for {what}")));
                }
                std::thread::sleep(Duration::from_millis(200).min(wait.poll));
            }
        }
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

    /// Open the tunnel. For Ollama, wait for it to answer through the tunnel; a recipe
    /// engine answers only once its weights load, which `ensure_models` waits for.
    pub fn open_tunnel(&self, profile: &Profile, on: &mut dyn FnMut(Event)) -> Result<Tunnel> {
        let mut tunnel = Tunnel::start(
            &self.cfg.ssh_alias,
            self.cfg.tunnel_port,
            Duration::from_secs(40),
        )?;
        on(Event::Step(format!(
            "tunnel up on 127.0.0.1:{}",
            self.cfg.tunnel_port
        )));
        if profile.recipe.is_some() {
            return Ok(tunnel);
        }
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

    /// Pull every profile model that is missing, on the pod, and wait for them. A
    /// recipe engine fetches its own weights; this waits until it serves them.
    pub fn ensure_models(&self, profile: &Profile, on: &mut dyn FnMut(Event)) -> Result<()> {
        if profile.recipe.is_some() {
            return self.wait_engine(profile, ENGINE_READY_TIMEOUT, on);
        }
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

    /// Wait until the recipe engine serves the profile's model through the tunnel,
    /// reporting download and load progress; fail at once if the engine exits.
    pub fn wait_engine(
        &self,
        profile: &Profile,
        limit: Duration,
        on: &mut dyn FnMut(Event),
    ) -> Result<()> {
        let want = profile
            .models
            .first()
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let api = Ollama::new(&self.cfg.tunnel_base_url());
        let started = Instant::now();
        let mut next_look = Instant::now();
        loop {
            if api.answers("/health")
                && let Ok(ids) = api.openai_models()
                && ids.iter().any(|i| i == &want)
            {
                on(Event::Step(format!(
                    "engine serving {want} after {}s",
                    started.elapsed().as_secs()
                )));
                return Ok(());
            }
            if Instant::now() >= next_look {
                next_look = Instant::now() + Duration::from_secs(20);
                if let Ok(st) = remote::engine_state(&self.cfg.ssh_alias) {
                    let last = st.log_tail.lines().last().unwrap_or("").trim().to_string();
                    if !st.running && started.elapsed() > Duration::from_secs(30) {
                        return Err(Error::Engine(format!(
                            "the engine stopped before serving {want}: {}",
                            st.log_tail.trim()
                        )));
                    }
                    on(Event::Step(format!(
                        "engine starting: {:.1} GB of weights on disk ({}s){}",
                        st.hf_bytes as f64 / 1e9,
                        started.elapsed().as_secs(),
                        if last.is_empty() {
                            String::new()
                        } else {
                            format!("; {}", clip_line(&last, 120))
                        }
                    )));
                }
            }
            if started.elapsed() >= limit {
                return Err(Error::Timeout(format!(
                    "the engine did not serve {want} within {} minutes",
                    limit.as_secs() / 60
                )));
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    /// Zed model entries for the profile's models. Ollama says what each can do; a
    /// recipe engine's model is described by the profile.
    pub fn zed_models(&self, profile: &Profile) -> Result<Vec<ZedModel>> {
        if profile.recipe.is_some() {
            return Ok(profile
                .models
                .iter()
                .map(|m| ZedModel {
                    name: m.name.clone(),
                    display_name: format!("RunPod · {}", m.name),
                    max_tokens: profile.context_length,
                    tools: m.tools,
                    images: m.images,
                })
                .collect());
        }
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

fn clip_line(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}...", s.chars().take(max).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> crate::runpod::PodCreate {
        let cfg = Config::default();
        spec::pod_create(&cfg, cfg.profile("medium").expect("medium"))
    }

    const NO_INSTANCES: &str =
        r#"{"error":"create pod: There are no instances currently available"}"#;

    #[test]
    fn no_capacity_is_recognised() {
        let full = Error::Api {
            what: "create pod".into(),
            status: 500,
            body: NO_INSTANCES.into(),
        };
        assert!(is_no_capacity(&full));
        let auth = Error::Api {
            what: "create pod".into(),
            status: 401,
            body: "unauthorized".into(),
        };
        assert!(!is_no_capacity(&auth));
        let msg = no_capacity(&body(), Duration::ZERO).to_string();
        assert!(
            msg.contains("no 1x of [NVIDIA RTX PRO 6000 Blackwell Server Edition"),
            "{msg}"
        );
        let waited = no_capacity(&body(), Duration::from_secs(120 * 60)).to_string();
        assert!(
            waited.contains("120 minute wait; nothing was rented"),
            "{waited}"
        );
    }

    /// A tiny HTTP server standing in for RunPod. `handler` gets the route
    /// ("POST /graphql", "POST /pods") and how many times that route was hit.
    struct Mock {
        url: String,
        hits: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Mock {
        fn count(&self, route: &str) -> usize {
            self.hits
                .lock()
                .expect("hits lock")
                .iter()
                .filter(|h| *h == route)
                .count()
        }
    }

    fn mock(handler: impl Fn(&str, usize) -> (u16, String) + Send + 'static) -> Mock {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = std::sync::Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming().map_while(std::result::Result::ok) {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let route = format!(
                    "{} {}",
                    parts.next().unwrap_or(""),
                    parts.next().unwrap_or("").split('?').next().unwrap_or("")
                );
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                let _ = reader.read_exact(&mut body);
                let nth = {
                    let mut l = log.lock().expect("hits lock");
                    let n = l.iter().filter(|h| **h == route).count();
                    l.push(route.clone());
                    n
                };
                let (status, text) = handler(&route, nth);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = reader.get_mut().write_all(resp.as_bytes());
            }
        });
        Mock { url, hits }
    }

    fn offers(free: bool) -> String {
        let price = if free { "8.36" } else { "null" };
        format!(
            r#"{{"data":{{"gpuTypes":[{{"id":"NVIDIA RTX PRO 6000 Blackwell Server Edition","displayName":"RTX PRO 6000","memoryInGb":96,"secureCloud":true,"lowestPrice":{{"uninterruptablePrice":{price},"stockStatus":null}}}}]}}}}"#
        )
    }

    const POD: &str =
        r#"{"id":"p1","name":"offrig-frontier","desiredStatus":"RUNNING","costPerHr":8.36}"#;

    fn session_on(m: &Mock) -> Session {
        let rp = RunPod::new("test-key", &m.url, &format!("{}/graphql", m.url));
        Session::with_client(Config::default(), rp)
    }

    fn frontier() -> crate::runpod::PodCreate {
        let cfg = Config::default();
        spec::pod_create(&cfg, cfg.profile("frontier").expect("frontier"))
    }

    fn fast(limit_ms: u64) -> Wait {
        Wait {
            limit: Duration::from_millis(limit_ms),
            poll: Duration::from_millis(20),
        }
    }

    #[test]
    fn waits_without_renting_until_the_gpus_free_up() {
        let m = mock(|route, nth| match route {
            "POST /graphql" => (200, offers(nth >= 2)),
            "POST /pods" => (200, POD.into()),
            _ => (404, "{}".into()),
        });
        let s = session_on(&m);
        let mut steps = Vec::new();
        let pod = s
            .create_when_free(
                &frontier(),
                fast(5_000),
                &AtomicBool::new(false),
                &mut |e| steps.push(e),
            )
            .expect("the pod is created once the GPUs are free");
        assert_eq!(pod.id, "p1");
        assert_eq!(m.count("POST /graphql"), 3, "two full answers, then free");
        assert_eq!(
            m.count("POST /pods"),
            1,
            "no create while the market was full"
        );
        assert!(
            steps
                .iter()
                .any(|e| matches!(e, Event::Step(s) if s.starts_with("no 4x")))
        );
    }

    #[test]
    fn gives_up_at_the_limit_having_rented_nothing() {
        let m = mock(|route, _| match route {
            "POST /graphql" => (200, offers(false)),
            _ => (200, POD.into()),
        });
        let s = session_on(&m);
        let err = s
            .create_when_free(&frontier(), fast(150), &AtomicBool::new(false), &mut |_| {})
            .expect_err("nothing frees up");
        assert!(matches!(err, Error::NoCapacity(_)), "{err}");
        assert_eq!(m.count("POST /pods"), 0);
    }

    #[test]
    fn without_the_price_api_it_retries_the_create() {
        let m = mock(|route, nth| match route {
            "POST /graphql" => (500, "{}".into()),
            "POST /pods" if nth < 2 => (500, NO_INSTANCES.into()),
            "POST /pods" => (200, POD.into()),
            _ => (404, "{}".into()),
        });
        let s = session_on(&m);
        let pod = s
            .create_when_free(
                &frontier(),
                fast(5_000),
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .expect("third create succeeds");
        assert_eq!(pod.id, "p1");
        assert_eq!(m.count("POST /pods"), 3);
    }

    #[test]
    fn other_create_errors_stop_at_once() {
        let m = mock(|route, _| match route {
            "POST /graphql" => (200, offers(true)),
            _ => (401, "unauthorized".into()),
        });
        let s = session_on(&m);
        let err = s
            .create_when_free(
                &frontier(),
                fast(5_000),
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .expect_err("401 is not a capacity problem");
        assert!(matches!(err, Error::Api { status: 401, .. }), "{err}");
        assert_eq!(m.count("POST /pods"), 1);
    }

    #[test]
    fn cancel_stops_the_wait() {
        let m = mock(|route, _| match route {
            "POST /graphql" => (200, offers(false)),
            _ => (200, POD.into()),
        });
        let s = session_on(&m);
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let err = s
            .create_when_free(&frontier(), fast(60_000), &cancel, &mut |_| {})
            .expect_err("cancelled");
        assert!(matches!(err, Error::Cancelled(_)), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(m.count("POST /pods"), 0);
    }

    #[test]
    fn local_copies_of_profile_models_are_refused() {
        let cfg = Config::default();
        let medium = cfg.profile("medium").expect("medium");
        let local = vec!["qwen3:8b".to_string(), "gemma4:31b".to_string()];
        assert!(local_conflicts(medium, &local).is_ok());
        let clash = vec!["gpt-oss:120b".to_string()];
        let err = local_conflicts(medium, &clash).expect_err("must refuse");
        assert!(err.to_string().contains("gpt-oss:120b"), "{err}");
        assert!(matches!(err, Error::Guard(_)));
    }

    #[test]
    fn default_profiles_avoid_this_rigs_local_models() {
        // Measured 2026-10-02: qwen3:8b is in this rig's local Ollama, which is why
        // the small profile does not use it.
        let cfg = Config::default();
        let local = vec![
            "qwen3:8b".to_string(),
            "qwen3:4b-instruct-2507-q4_K_M".to_string(),
        ];
        for p in &cfg.profiles {
            local_conflicts(p, &local).unwrap_or_else(|e| panic!("{}: {e}", p.name));
        }
    }
}
