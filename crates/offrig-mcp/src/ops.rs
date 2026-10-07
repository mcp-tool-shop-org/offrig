//! The side-car's long and side-effecting operations, as plain blocking functions.
//! Tools run them on a blocking thread; each opens its own store connection so a
//! minutes-long launch never holds the side-car's lock.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use offrig_core::config::Config;
use offrig_core::cost::now_unix;
use offrig_core::error::chain;
use offrig_core::lanes::LaneCtx;
use offrig_core::ollama::Ollama;
use offrig_core::planning::HostCuda;
use offrig_core::remote::PullState;
use offrig_core::runpod::{Pod, RunPod};
use offrig_core::session::{Event, Session, Wait, capacity_wait};
use offrig_core::store::{Plan, State, Store};
use offrig_core::tunnel::Tunnel;
use offrig_core::{Error, Result, planning, roles, runner, spec, watchdog};
use serde_json::{Value, json};

/// State shared by every tool call in one side-car process.
#[derive(Default)]
pub struct Shared {
    /// The tunnel to the current pod, held for as long as the side-car runs.
    pub tunnel: Mutex<Option<Tunnel>>,
    /// Cancel flags for running launch jobs, by plan id.
    pub cancels: Mutex<HashMap<i64, Arc<AtomicBool>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Neither value is left half-updated by a panic: take it as it is.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Context budget for one `offrig_ask` turn, in characters.
pub const ASK_CONTEXT_CHARS: usize = 24_000;

struct Progress<'a> {
    store: &'a Store,
    job: i64,
    v: Value,
    /// What the plan asked for, to set the rented pod against (see `planning::audit_rental`).
    gpu_types: Vec<String>,
    max_price_hr: f64,
    min_cuda: Option<String>,
}

impl Progress<'_> {
    /// The rented pod against the plan: GPU type, host CUDA, price, and any warnings.
    fn rented(&self, pod: &Pod) -> Value {
        planning::audit_rental(
            pod,
            &self.gpu_types,
            self.max_price_hr,
            self.min_cuda.as_deref(),
        )
        .to_json()
    }

    /// As [`Progress::rented`], with the host CUDA measured on the pod over ssh.
    fn rented_with_host(&self, pod: &Pod, host: &HostCuda) -> Value {
        planning::audit_rental_with_host(
            pod,
            host,
            &self.gpu_types,
            self.max_price_hr,
            self.min_cuda.as_deref(),
        )
        .to_json()
    }

    fn push(&self) {
        // Progress is advisory; a failed write must not stop the launch.
        let _ = self.store.update_job(self.job, "running", &self.v, None);
    }

    fn step(&mut self, s: impl Into<String>) {
        self.v["step"] = json!(s.into());
        self.v["at"] = json!(now_unix());
        self.push();
    }

    fn event(&mut self, e: Event) {
        match e {
            Event::Step(s) | Event::Warn(s) => self.step(s),
            Event::Pod(p) => {
                self.v["ssh"] = json!(p.ssh_endpoint().map(|(h, port)| format!("{h}:{port}")));
                self.v["rented"] = self.rented(&p);
                self.push();
            }
            Event::Waiting {
                attempt,
                waited_secs,
                limit_secs,
            } => {
                // Each retry is visible to offrig_job: how many checks, how long, how
                // much is left. Nothing is rented while this repeats.
                self.v["capacity_wait"] = json!({
                    "checks": attempt,
                    "waited_secs": waited_secs,
                    "limit_secs": limit_secs,
                });
                self.step(format!(
                    "no capacity yet for the plan's GPUs: check {attempt}, waited {} of {} min, \
                     checking again in a minute; nothing is rented while waiting",
                    waited_secs / 60,
                    limit_secs / 60
                ));
            }
            Event::Pull { model, state } => {
                let s = match state {
                    PullState::Running {
                        completed, total, ..
                    } if total > 0 => {
                        json!(format!("{}%", completed * 100 / total))
                    }
                    PullState::Running { status, .. } => json!(status),
                    PullState::Done => json!("done"),
                    PullState::Failed(e) => json!(format!("failed: {e}")),
                    PullState::NotStarted => json!("not started"),
                };
                if !self.v["pulls"].is_object() {
                    self.v["pulls"] = json!({});
                }
                self.v["pulls"][model] = s;
                self.push();
            }
        }
    }
}

/// Sets `stop` when dropped: ends a helper thread when the launch returns, however it returns.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The launch job. Writes its outcome into the job row; never panics the side-car.
pub fn run_launch(
    ctx: LaneCtx,
    db: PathBuf,
    shared: Arc<Shared>,
    plan_id: i64,
    job_id: i64,
    cancel: Arc<AtomicBool>,
) {
    let Ok(store) = Store::open(&db) else {
        return;
    };
    let result = launch(&ctx, &db, &store, &shared, plan_id, job_id, &cancel);
    match result {
        Ok(v) => {
            let _ = store.update_job(job_id, "done", &v, None);
        }
        Err(e) => {
            let state = if matches!(e, Error::Cancelled(_)) {
                "cancelled"
            } else {
                "failed"
            };
            let _ = store.update_job(
                job_id,
                state,
                &json!({ "step": "stopped" }),
                Some(&chain(&e)),
            );
        }
    }
    lock(&shared.cancels).remove(&plan_id);
}

/// How often a capacity wait looks again: a minute. Tests shorten it (debug builds only).
fn capacity_poll() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("OFFRIG_TEST_CAPACITY_POLL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    Duration::from_secs(60)
}

/// Run `nvidia-smi` once on the pod through the lane's ssh alias and read the CUDA
/// version from its header. Never fails the launch: the pod is billing and usable, so a
/// missing answer is recorded as such.
fn host_cuda(alias: &str) -> HostCuda {
    match offrig_core::remote::host_cuda_version(alias) {
        Ok(Some(v)) => HostCuda::Measured(v),
        Ok(None) => HostCuda::Unavailable("nvidia-smi printed no CUDA version".into()),
        Err(e) => HostCuda::Unavailable(chain(&e)),
    }
}

/// What the pod itself says about a launch's progress while the pod boots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SshProbe {
    /// Not tried: RunPod has not published an address to try.
    NoEndpoint,
    Answers,
    Silent,
}

/// The step for a launch whose pod is booting, derived from the pod's state right now
/// rather than read from the launch's last event, which goes stale on a slow host
/// (issue #15). `created_ago` is seconds since the pod was created, when known.
pub fn live_step(pod: &Pod, probe: SshProbe, created_ago: Option<i64>) -> String {
    let age = created_ago.map_or(String::new(), |s| format!(", {s}s after it was created"));
    if !pod.is_running() && !pod.desired_status.is_empty() {
        return format!(
            "the pod is {} (not RUNNING){age}; the launch will fail if it does not start",
            pod.desired_status
        );
    }
    match (pod.ssh_endpoint(), probe) {
        (None, _) => format!(
            "waiting for the pod to pull its image and get a public address from RunPod{age}"
        ),
        (Some((host, port)), SshProbe::Answers) => {
            format!("ssh answers at {host}:{port}{age}; the launch is moving on to the host check")
        }
        (Some((host, port)), _) => format!(
            "RunPod published the address {host}:{port}{age}, but sshd is not answering yet"
        ),
    }
}

/// Look at the launch's pod now: ask RunPod for it, try its ssh port, and say what that
/// means for the step. `None` when RunPod could not be asked (the launch's own step is
/// kept then, with the reason beside it).
pub fn live_progress(pod_id: &str, created_at: Option<i64>) -> Result<String> {
    let pod = RunPod::from_env()?.get_pod(pod_id)?;
    let probe = match pod.ssh_endpoint() {
        None => SshProbe::NoEndpoint,
        Some((host, port)) => {
            if offrig_core::remote::ssh_answers(&host, port, Duration::from_secs(3)) {
                SshProbe::Answers
            } else {
                SshProbe::Silent
            }
        }
    };
    Ok(live_step(
        &pod,
        probe,
        created_at.map(|t| (now_unix() - t).max(0)),
    ))
}

/// A pod this lane did not name is not this lane's to touch: another lane's, the plain
/// lane's, or another studio job's. Every path that reaches a pod by id checks this
/// first, so a wrong id can never make a side-car stop or point at someone else's pod.
pub fn ensure_owned(cfg: &Config, pod: &Pod) -> Result<()> {
    if cfg.owns_pod(&pod.name) {
        return Ok(());
    }
    Err(Error::Refused(format!(
        "pod {} ({}) is not in this project's lane ({}); offrig leaves it alone",
        pod.name,
        pod.id,
        cfg.lane_tag.as_deref().unwrap_or("plain")
    )))
}

fn launch(
    ctx: &LaneCtx,
    db: &Path,
    store: &Store,
    shared: &Shared,
    plan_id: i64,
    job_id: i64,
    cancel: &Arc<AtomicBool>,
) -> Result<Value> {
    let plan = store
        .plan(plan_id)?
        .ok_or_else(|| Error::Refused(format!("no plan {plan_id}")))?;
    let cfg = &ctx.cfg_for_plan(store, &plan)?;
    let profile = cfg.profile(&plan.profile)?.clone();
    let session = Session::with_client(cfg.clone(), RunPod::from_env()?);
    // The plan's own CUDA floor, else the profile's (a plan made before plans stored one).
    let min_cuda = store
        .plan_min_cuda(plan_id)?
        .or_else(|| profile.effective_min_cuda());
    let mut progress = Progress {
        store,
        job: job_id,
        v: json!({ "step": "starting" }),
        gpu_types: plan.gpu_types.clone(),
        max_price_hr: plan.max_price_hr,
        min_cuda: min_cuda.clone(),
    };

    // Stop waiting the moment the plan stops being committed (a shutdown, or the
    // watchdog closing it at its deadline).
    let monitor_stop = Arc::new(AtomicBool::new(false));
    let _monitor = StopOnDrop(Arc::clone(&monitor_stop));
    {
        let db = db.to_path_buf();
        let cancel = Arc::clone(cancel);
        std::thread::spawn(move || {
            let Ok(s) = Store::open(&db) else { return };
            while !monitor_stop.load(Ordering::SeqCst) {
                if let Ok(Some(p)) = s.plan(plan_id)
                    && p.state != "committed"
                {
                    cancel.store(true, Ordering::SeqCst);
                    return;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    }

    // Rent only from what the plan priced: its GPU list and its CUDA floor, not the
    // profile's (issues #9 and #10). A plan that stored no list uses the profile's.
    let mut body = spec::pod_create_for_plan(
        cfg,
        &profile,
        &plan.gpu_types,
        min_cuda.as_deref(),
        store.plan_container_disk_gb(plan_id)?,
    );
    spec::mark_plan(&mut body, &plan);
    let now = now_unix();
    let left = plan.deadline().map_or(0, |d| (d - now).max(0)) as u64;
    // The plan's own wait, else the profile's, kept inside the time the plan has left.
    // Nothing is rented while waiting, so the committed worst case is not touched by it.
    let wait = Wait {
        limit: capacity_wait(
            store.plan_wait_minutes(plan_id)?,
            profile.wait_for_gpu_minutes,
            left,
        ),
        poll: capacity_poll(),
    };
    progress.v["phase"] = json!("capacity");
    progress.v["wait_minutes"] = json!(wait.limit.as_secs() / 60);
    let j = store.journal(
        "create_pod",
        Some(plan_id),
        &json!({ "profile": profile.name, "gpus": format!("{}x {}", body.gpu_count, body.gpu_type_ids.join(" | ")) }),
    )?;
    let created = session.create_when_free(&body, wait, cancel, &mut |e| progress.event(e));
    let pod = match created {
        Ok(p) => {
            store.journal_outcome(j, &format!("created pod {}", p.id))?;
            p
        }
        Err(e) => {
            store.journal_outcome(j, &format!("no pod: {}", chain(&e)))?;
            return Err(e);
        }
    };
    store.attach_pod(plan_id, &pod.id)?;
    progress.v["pod_id"] = json!(pod.id);
    progress.v["pod_created_at"] = json!(now_unix());
    // offrig_job derives the step from the pod itself while the pod boots, so a
    // slow host never leaves a stale "waiting for the address" line behind.
    progress.v["phase"] = json!("pod_boot");
    progress.v["cost_per_hr"] = json!(pod.cost_per_hr);
    progress.step(format!(
        "pod {} created at ${:.2}/hr",
        pod.id, pod.cost_per_hr
    ));

    // From here the pod bills. If getting it ready fails, terminate it rather than
    // leave it running unused until the deadline (the launch's compensator).
    let ready = (|| -> Result<Value> {
        let up = session.wait_ready(&pod.id, &mut |e| progress.event(e))?;
        progress.v["phase"] = json!("after_ssh");
        progress.step(format!("ssh is up through the alias {}", cfg.ssh_alias));
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled("getting the pod ready".into()));
        }
        // The pod API does not report the host's CUDA version, so ask the host: one
        // nvidia-smi through the lane. A host below the plan's floor is reported loudly
        // in `rented.warnings`; nothing is terminated for it.
        let host = host_cuda(&cfg.ssh_alias);
        let rented = progress.rented_with_host(&up, &host);
        progress.v["rented"] = rented.clone();
        match &host {
            HostCuda::Measured(v) => progress.step(format!("host CUDA {v} (nvidia-smi)")),
            HostCuda::Unavailable(why) => {
                progress.step(format!("host CUDA unknown: nvidia-smi gave none ({why})"));
            }
            HostCuda::NotAsked => {}
        }
        if let Some(w) = rented["warnings"].as_array() {
            for w in w.iter().filter_map(Value::as_str) {
                progress.step(format!("WARNING: {w}"));
            }
        }
        if profile.is_job() {
            // Nothing is served: ready means sshd answers. Work goes up with offrig_put.
            return Ok(json!({
                "step": "ready",
                "pod_id": pod.id,
                "cost_per_hr": pod.cost_per_hr,
                "rented": rented,
                "ssh_alias": cfg.ssh_alias,
                "lane": cfg.lane_tag,
                "job_dir": spec::JOB_DIR,
                "next": "offrig_put your files, then offrig_exec a command; offrig_get the results",
            }));
        }
        let tunnel = session.open_tunnel(&profile, &mut |e| progress.event(e))?;
        *lock(&shared.tunnel) = Some(tunnel);
        session.ensure_models(&profile, &mut |e| progress.event(e))?;
        Ok(json!({
            "step": "ready",
            "pod_id": pod.id,
            "cost_per_hr": pod.cost_per_hr,
            "rented": rented,
            "tunnel": cfg.tunnel_base_url(),
            "lane": cfg.lane_tag,
            "models": profile.models.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
        }))
    })();
    match ready {
        Ok(v) => Ok(v),
        Err(e) => {
            lock(&shared.tunnel).take();
            let rate = pod.cost_per_hr;
            let jt = store.journal(
                "terminate_pod",
                Some(plan_id),
                &json!({ "pod": pod.id, "reason": "launch failed" }),
            )?;
            let outcome = match session.rp.delete_pod(&pod.id) {
                Ok(()) => {
                    // The lane's ssh block pointed at this pod; it is gone now.
                    let _ = offrig_core::sshconfig::remove_for_pod(&cfg.ssh_alias, &pod.id);
                    "terminated after a failed launch".to_string()
                }
                Err(d) => format!("terminate failed: {}", chain(&d)),
            };
            store.journal_outcome(jt, &outcome)?;
            if let Ok(Some(p)) = store.plan(plan_id) {
                store.close_plan(plan_id, watchdog::spend(&p, rate, now_unix()))?;
            }
            Err(e)
        }
    }
}

/// Reopen the tunnel to the plan's pod when it is missing or dead. Returns the plan's
/// profile and the config of the lane the plan runs in.
fn ensure_tunnel(ctx: &LaneCtx, store: &Store, shared: &Shared) -> Result<(String, Config)> {
    let alive = lock(&shared.tunnel).as_mut().is_some_and(Tunnel::is_alive);
    let plan = store
        .open_plans()?
        .into_iter()
        // A job pod serves no model, so it is never the pod a model turn goes to.
        .find(|p| p.pod_id.is_some() && ctx.base.profile(&p.profile).is_ok_and(|x| !x.is_job()))
        .ok_or_else(|| Error::Refused("no launched session; launch a plan first".into()))?;
    let cfg = ctx.cfg_for_plan(store, &plan)?;
    if alive {
        return Ok((plan.profile, cfg));
    }
    let session = Session::with_client(cfg.clone(), RunPod::from_env()?);
    let pod_id = plan.pod_id.clone().unwrap_or_default();
    let pod = session.rp.get_pod(&pod_id)?;
    ensure_owned(&cfg, &pod)?;
    session.write_ssh(&pod)?;
    let t = session.open_tunnel(cfg.profile(&plan.profile)?, &mut |_| {})?;
    *lock(&shared.tunnel) = Some(t);
    Ok((plan.profile, cfg))
}

/// One role-headed turn for a handoff, context built from the project store.
pub fn ask(
    ctx: &LaneCtx,
    db: &Path,
    shared: &Shared,
    handoff_id: i64,
    instruction: &str,
    model: Option<String>,
) -> Result<Value> {
    let store = Store::open(db)?;
    let h = store
        .handoff(handoff_id)?
        .ok_or_else(|| Error::Refused(format!("no handoff {handoff_id}")))?;
    if h.state.blocked() || h.state.terminal() || matches!(h.state, State::Failed | State::TimedOut)
    {
        return Err(Error::Refused(format!(
            "handoff {handoff_id} is {}; use offrig_handoffs action=retry to move it back",
            h.state.as_str()
        )));
    }
    if let Some(p) = store.open_plans()?.into_iter().find(|p| p.pod_id.is_some())
        && crate::runner::alive(&store, p.id, now_unix())
    {
        return Err(Error::Refused(
            "a runner is working this queue; follow it with offrig_job, or wait for it to finish"
                .into(),
        ));
    }
    let (profile_name, cfg) = ensure_tunnel(ctx, &store, shared)?;
    let model = match model {
        Some(m) => m,
        None => cfg
            .profile(&profile_name)?
            .models
            .first()
            .map(|m| m.name.clone())
            .ok_or_else(|| Error::Refused(format!("profile {profile_name} lists no models")))?,
    };
    let prep = runner::prepare(
        &store,
        cfg.role_os_dir.as_deref().map(Path::new),
        &h,
        instruction,
        ASK_CONTEXT_CHARS,
    )?;
    if h.state == State::Pending {
        store.transition(
            handoff_id,
            State::Dispatched,
            "dispatched by offrig_ask",
            None,
        )?;
    }
    if store
        .handoff(handoff_id)?
        .is_some_and(|x| x.state == State::Dispatched)
    {
        store.transition(handoff_id, State::Running, "pod model working", None)?;
    }
    store.set_run_details(
        handoff_id,
        &model,
        &roles::fingerprint(&prep.role_block),
        &roles::fingerprint(&prep.assembled.text),
    )?;
    let reply = Ollama::new(&cfg.tunnel_base_url()).chat(&model, &prep.assembled.text, None)?;
    store.heartbeat(handoff_id)?;
    Ok(json!({
        "handoff_id": handoff_id,
        "model": model,
        "reply": reply,
        "reply_is_untrusted_model_output": true,
        "context": {
            "always_injected": prep.injected,
            "memory_included": prep.assembled.included,
            "memory_dropped": prep.assembled.dropped,
            "over_budget": prep.assembled.over_budget,
        },
    }))
}

/// Terminate the plan's pod and close its books with the spend measured.
pub fn shutdown(
    db: &Path,
    shared: &Shared,
    ctx: &LaneCtx,
    plan_id: i64,
    reason: Option<&str>,
) -> Result<Value> {
    let store = Store::open(db)?;
    let plan = store
        .plan(plan_id)?
        .ok_or_else(|| Error::Refused(format!("no plan {plan_id}")))?;
    if plan.state != "committed" {
        return Ok(json!({ "plan_id": plan_id, "already": plan.state, "budget": store.budget()? }));
    }
    let cfg = ctx.cfg_for_plan(&store, &plan)?;
    let in_flight: Vec<i64> = store
        .handoffs()?
        .into_iter()
        .filter(|h| h.state.in_flight())
        .map(|h| h.id)
        .collect();
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    if !in_flight.is_empty() && reason.is_none() {
        return Err(Error::Refused(format!(
            "handoffs {in_flight:?} are in flight and their work lives on the pod; record a checkpoint for each \
             (offrig_memory_record kind=checkpoint task_id=<id>) or pass a reason to terminate anyway"
        )));
    }
    // Look the pod up and check its lane before anything is cancelled, closed or
    // journaled: a refusal leaves the session exactly as it was.
    let mut rate = plan.max_price_hr;
    let rp = match plan.pod_id.as_deref() {
        Some(_) => Some(RunPod::from_env()?),
        None => None,
    };
    if let (Some(rp), Some(id)) = (&rp, plan.pod_id.as_deref())
        && let Ok(p) = rp.get_pod(id)
    {
        // Never terminate a pod this plan's lane did not name, whatever id the plan holds.
        ensure_owned(&cfg, &p)?;
        rate = p.cost_per_hr;
    }
    if let Some(c) = lock(&shared.cancels).get(&plan_id) {
        c.store(true, Ordering::SeqCst);
    }
    lock(&shared.tunnel).take();
    let j = store.journal(
        "terminate_pod",
        Some(plan_id),
        &json!({ "pod": plan.pod_id, "reason": reason.unwrap_or("shutdown"), "in_flight": in_flight }),
    )?;
    let outcome = match (&rp, plan.pod_id.as_deref()) {
        (Some(rp), Some(id)) => match rp.delete_pod(id) {
            Ok(()) => format!("terminated {id}"),
            // Already gone is the state we want.
            Err(Error::Api { status: 404, .. }) => format!("{id} was already gone"),
            Err(e) => return Err(e),
        },
        _ => "no pod was rented".to_string(),
    };
    store.journal_outcome(j, &outcome)?;
    // The pod is gone, so the ssh block that named it is stale. Remove this lane's own
    // block, and only if it names this plan's pod: never another lane's, never a block
    // already rewritten for a newer pod. Not worth failing a shutdown over.
    let ssh_block_removed = plan
        .pod_id
        .as_deref()
        .and_then(|id| offrig_core::sshconfig::remove_for_pod(&cfg.ssh_alias, id).ok())
        .unwrap_or(false);
    let spent = if plan.pod_id.is_some() {
        watchdog::spend(&plan, rate, now_unix())
    } else {
        0.0
    };
    let budget = store.close_plan(plan_id, spent)?;
    Ok(json!({
        "plan_id": plan_id,
        "outcome": outcome,
        "ssh_block_removed": ssh_block_removed,
        "spent": spent,
        "budget": budget,
        "terminated_with_in_flight": in_flight,
    }))
}

/// What a job tool acts on: the plan, the lane config it runs under and its pod.
pub struct JobTarget {
    pub plan: Plan,
    pub cfg: Config,
    pub pod: Pod,
}

/// "plan 3 (job, pod offrig-aspire-si-job)": how an error names an open plan.
fn describe_plan(ctx: &LaneCtx, store: &Store, plan: &Plan) -> String {
    let pod = ctx
        .cfg_for_plan(store, plan)
        .ok()
        .and_then(|c| c.profile(&plan.profile).ok().map(|p| c.pod_name(p)))
        .unwrap_or_else(|| "unknown".into());
    format!("plan {} ({}, pod {pod})", plan.id, plan.profile)
}

/// A pod is a plan's only if the lane owns it and its name is exactly the one this
/// plan's profile gets in the plan's lane. Another plan's pod in the same lane (a job
/// next to a jam) is refused here even though the lane owns both names.
pub fn ensure_plan_pod(cfg: &Config, plan: &Plan, pod: &Pod) -> Result<()> {
    ensure_owned(cfg, pod)?;
    let expected = cfg.profile(&plan.profile).map(|p| cfg.pod_name(p))?;
    if pod.name == expected {
        return Ok(());
    }
    Err(Error::Refused(format!(
        "pod {} ({}) is not the pod plan {} owns ({expected}); offrig leaves it alone",
        pod.name, pod.id, plan.id
    )))
}

/// Pick the job plan a job tool acts on and fetch its pod, with no ssh written.
/// Without `plan_id` the one open job plan is used; several are refused and listed.
/// With `plan_id` only that plan is used, and only if its pod is the one it owns.
pub fn resolve_job_target(
    ctx: &LaneCtx,
    store: &Store,
    plan_id: Option<i64>,
    get_pod: &dyn Fn(&str) -> Result<Pod>,
) -> Result<JobTarget> {
    let is_job = |p: &Plan| ctx.base.profile(&p.profile).is_ok_and(|x| x.is_job());
    let plan = match plan_id {
        Some(id) => {
            let plan = store
                .plan(id)?
                .ok_or_else(|| Error::Refused(format!("no plan {id}")))?;
            if plan.state != "committed" {
                return Err(Error::Refused(format!(
                    "plan {id} is {}, not an open plan",
                    plan.state
                )));
            }
            if !is_job(&plan) {
                return Err(Error::Refused(format!(
                    "plan {id} is profile {}, not a job pod; offrig_put, offrig_exec and offrig_get work on job pods",
                    plan.profile
                )));
            }
            if plan.pod_id.is_none() {
                return Err(Error::Refused(format!(
                    "plan {id} has no pod yet; follow its launch with offrig_job"
                )));
            }
            plan
        }
        None => {
            let mut open: Vec<Plan> = store
                .open_plans()?
                .into_iter()
                .filter(|p| p.pod_id.is_some() && is_job(p))
                .collect();
            match open.len() {
                0 => {
                    return Err(Error::Refused(
                        "no launched job pod; plan the job profile (offrig_plan profile=job) and launch it"
                            .into(),
                    ));
                }
                1 => open.remove(0),
                n => {
                    let list: Vec<String> =
                        open.iter().map(|p| describe_plan(ctx, store, p)).collect();
                    return Err(Error::Refused(format!(
                        "{n} open job plans, so which pod is meant is ambiguous; pass plan_id: {}",
                        list.join("; ")
                    )));
                }
            }
        }
    };
    let cfg = ctx.cfg_for_plan(store, &plan)?;
    let pod = get_pod(plan.pod_id.as_deref().unwrap_or_default())?;
    ensure_plan_pod(&cfg, &plan, &pod)?;
    Ok(JobTarget { plan, cfg, pod })
}

/// The job plan's pod, with the ssh alias pointed at it.
fn job_pod(
    ctx: &LaneCtx,
    store: &Store,
    plan_id: Option<i64>,
) -> Result<(i64, String, Option<String>)> {
    let rp = RunPod::from_env()?;
    let t = resolve_job_target(ctx, store, plan_id, &|id| rp.get_pod(id))?;
    Session::with_client(t.cfg.clone(), rp).write_ssh(&t.pod)?;
    Ok((t.plan.id, t.cfg.ssh_alias.clone(), t.cfg.lane_tag.clone()))
}

/// Every job-tool reply says which project, lane and plan it acted on.
fn acted_on(ctx: &LaneCtx, plan_id: i64, lane: Option<String>, mut v: Value) -> Value {
    v["project"] = json!(ctx.project.display().to_string());
    v["lane"] = json!(lane);
    v["plan_id"] = json!(plan_id);
    v
}

/// What a lane already holds that a second launch would collide with: an open
/// (committed, not closed) plan of the lane, then any live pod the lane owns. A lane
/// has one ssh alias, so a second pod would re-point it at itself and send the first
/// plan's put, exec and get to the wrong machine. `pods` is the account's pod list as
/// far as the caller has it (empty skips the pod check).
pub fn lane_busy(
    ctx: &LaneCtx,
    store: &Store,
    cfg: &Config,
    launching: i64,
    pods: &[Pod],
) -> Result<()> {
    let tag = cfg.lane_tag.as_deref().unwrap_or("plain");
    let busy = |name: &str, plan: Option<i64>| {
        Error::Refused(format!(
            "lane {tag} has a live pod {name} ({}); shut it down first",
            plan.map_or("no open plan".to_string(), |id| format!("plan {id}"))
        ))
    };
    let open = store.open_plans()?;
    for p in &open {
        if p.id == launching {
            continue;
        }
        let theirs = ctx.cfg_for_plan(store, p)?;
        if theirs.lane_tag == cfg.lane_tag {
            let name = theirs
                .profile(&p.profile)
                .map_or_else(|_| p.profile.clone(), |x| theirs.pod_name(x));
            return Err(busy(&name, Some(p.id)));
        }
    }
    for pod in pods {
        if cfg.owns_pod(&pod.name) && pod.desired_status != "TERMINATED" {
            let plan = open
                .iter()
                .find(|p| p.pod_id.as_deref() == Some(pod.id.as_str()))
                .map(|p| p.id);
            return Err(busy(&pod.name, plan));
        }
    }
    Ok(())
}

/// What one `offrig_exec` call asks for.
pub struct ExecRequest<'a> {
    /// start, status, stop or run.
    pub action: &'a str,
    /// The job's name (start, status, stop).
    pub name: Option<&'a str>,
    pub command: Option<&'a str>,
    pub tail_lines: u32,
    pub plan_id: Option<i64>,
    /// action=run: seconds before the command is ended (default 30, at most 120).
    pub timeout_secs: Option<u32>,
    /// action=status: a local file to copy the job's whole log to.
    pub save_log: Option<&'a Path>,
}

/// offrig_exec: start, follow or stop a command on the job pod, or run a short one and
/// wait for it.
pub fn job_exec(ctx: &LaneCtx, db: &Path, req: &ExecRequest<'_>) -> Result<Value> {
    let store = Store::open(db)?;
    let (plan_id, alias, lane) = job_pod(ctx, &store, req.plan_id)?;
    let reply = |v: Value| acted_on(ctx, plan_id, lane.clone(), v);
    let command = req.command;
    let tail_lines = req.tail_lines;
    let need_name = || {
        req.name
            .ok_or_else(|| Error::Refused(format!("action={} needs a job name", req.action)))
    };
    match req.action {
        "run" => {
            let command =
                command.ok_or_else(|| Error::Refused("action=run needs a command".into()))?;
            let secs = offrig_core::job::run_timeout(req.timeout_secs);
            let j = store.journal(
                "job_run",
                Some(plan_id),
                &json!({ "command": command, "timeout_secs": secs }),
            )?;
            let ran = offrig_core::job::run(&alias, command, Some(secs));
            store.journal_outcome(
                j,
                &match &ran {
                    Ok(r) if r.timed_out => format!("timed out after {secs}s"),
                    Ok(r) => format!("exit {}", r.exit_code.map_or("?".into(), |c| c.to_string())),
                    Err(e) => format!("not run: {}", chain(e)),
                },
            )?;
            let r = ran?;
            let mut v = serde_json::to_value(&r).unwrap_or_else(|_| json!({}));
            v["output_is_untrusted_pod_output"] = json!(true);
            Ok(reply(v))
        }
        "start" => {
            let name = need_name()?;
            let command =
                command.ok_or_else(|| Error::Refused("action=start needs a command".into()))?;
            let j = store.journal(
                "job_start",
                Some(plan_id),
                &json!({ "name": name, "command": command }),
            )?;
            let started = offrig_core::job::start(&alias, name, command)?;
            store.journal_outcome(
                j,
                if started {
                    "started"
                } else {
                    "already running"
                },
            )?;
            Ok(reply(json!({ "name": name, "started": started })))
        }
        "status" => {
            let name = need_name()?;
            let s = offrig_core::job::status(&alias, name, tail_lines)?;
            let mut v = json!({ "name": name, "status": s });
            if let Some(local) = req.save_log {
                // The whole log, as the job wrote it (progress bars included), so the
                // tail above can stay short.
                let bytes = offrig_core::job::fetch_log(&alias, name, local)?;
                v["saved_log"] = json!({
                    "path": local.display().to_string(),
                    "bytes": bytes,
                    "pod": offrig_core::job::log_path(name)?,
                });
            }
            Ok(reply(v))
        }
        "stop" => {
            let name = need_name()?;
            let j = store.journal("job_stop", Some(plan_id), &json!({ "name": name }))?;
            offrig_core::job::stop(&alias, name)?;
            store.journal_outcome(j, "stop sent")?;
            Ok(reply(json!({ "name": name, "stopped": true })))
        }
        other => Err(Error::Refused(format!(
            "action {other:?}: use start, status, stop or run"
        ))),
    }
}

/// offrig_put / offrig_get: copy a file or directory to or from the job pod.
pub fn job_copy(
    ctx: &LaneCtx,
    db: &Path,
    upload: bool,
    local: &Path,
    remote: &str,
    plan_id: Option<i64>,
) -> Result<Value> {
    let store = Store::open(db)?;
    let (plan_id, alias, lane) = job_pod(ctx, &store, plan_id)?;
    let pod_path = if upload {
        offrig_core::job::put(&alias, local, remote)?
    } else {
        offrig_core::job::get(&alias, remote, local)?
    };
    Ok(acted_on(
        ctx,
        plan_id,
        lane,
        json!({
            "direction": if upload { "up" } else { "down" },
            "local": local.display().to_string(),
            "pod": pod_path,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use offrig_core::lanes::{Lane, Registry};
    use offrig_core::store::NewPlan;

    fn pod(name: &str) -> Pod {
        serde_json::from_str(&format!(
            r#"{{"id":"p1","name":"{name}","desiredStatus":"RUNNING","costPerHr":0.25}}"#
        ))
        .expect("pod")
    }

    fn lane_cfg(tag: &str) -> Config {
        Config::default()
            .in_lane(&Lane {
                tag: Some(tag.into()),
                ssh_alias: format!("offrig-{tag}"),
                tunnel_port: 11500,
            })
            .expect("lane config")
    }

    #[test]
    fn a_lane_owns_only_its_own_pods() {
        let mine = lane_cfg("aspire-si");
        assert!(ensure_owned(&mine, &pod("offrig-aspire-si-small")).is_ok());
        assert!(ensure_owned(&mine, &pod("offrig-aspire-si-job")).is_ok());
        for name in [
            "offrig-job",
            "offrig-small",
            "offrig-ai-jam-sessions-small",
            "offrig-stage-small",
            "ai-playtest-personas",
        ] {
            let err = ensure_owned(&mine, &pod(name)).expect_err(name);
            assert!(matches!(err, Error::Refused(_)), "{name}");
        }
    }

    #[test]
    fn the_plain_lane_keeps_its_pods_and_owns_no_lanes() {
        let plain = Config::default();
        assert!(ensure_owned(&plain, &pod("offrig-job")).is_ok());
        assert!(ensure_owned(&plain, &pod("offrig-small")).is_ok());
        assert!(ensure_owned(&plain, &pod("offrig-aspire-si-small")).is_err());
        assert!(ensure_owned(&plain, &pod("training-run")).is_err());
    }

    fn named_pod(id: &str, name: &str) -> Pod {
        serde_json::from_str(&format!(
            r#"{{"id":"{id}","name":"{name}","desiredStatus":"RUNNING","costPerHr":0.25}}"#
        ))
        .expect("pod")
    }

    /// A project folder with its own lane (in a temp registry), a store with a budget,
    /// and a context to resolve plans against. Nothing outside the temp dir is touched.
    struct Fixture {
        dir: PathBuf,
        ctx: LaneCtx,
        store: Store,
        tag: String,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("offrig-ops-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let project = dir.join("projects").join(name);
            std::fs::create_dir_all(&project).expect("project dir");
            let ctx = LaneCtx::new(Config::default(), &project, Registry::at(dir.join("cfg")));
            let tag = ctx.own().expect("lane").tag.expect("tag");
            let store = Store::open(&project.join(".offrig").join("offrig.db")).expect("store");
            store.set_budget_cap(500.0).expect("cap");
            Fixture {
                dir,
                ctx,
                store,
                tag,
            }
        }

        /// A planned plan in this project's lane.
        fn planned(&self, profile: &str) -> i64 {
            let p = self
                .store
                .create_plan(NewPlan {
                    profile: profile.into(),
                    gpu_count: 1,
                    gpu_types: vec![],
                    max_hours: 1.0,
                    max_price_hr: 0.5,
                    note: None,
                })
                .expect("plan");
            self.store.set_plan_lane(p.id, &self.tag).expect("lane");
            p.id
        }

        /// An open plan holding `pod_id`.
        fn open(&self, profile: &str, pod_id: &str) -> i64 {
            let id = self.planned(profile);
            self.store.commit_plan(id).expect("commit");
            self.store.attach_pod(id, pod_id).expect("attach");
            id
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A pod lookup that answers from a table and records every id it was asked for.
    fn lookup<'a>(
        pods: &'a [Pod],
        asked: &'a Mutex<Vec<String>>,
    ) -> impl Fn(&str) -> Result<Pod> + 'a {
        move |id| {
            asked.lock().expect("asked").push(id.to_string());
            pods.iter()
                .find(|p| p.id == id)
                .cloned()
                .ok_or_else(|| Error::Refused(format!("no pod {id}")))
        }
    }

    #[test]
    fn two_open_job_plans_and_no_plan_id_refuse_and_list_both() {
        let f = Fixture::new("two-plans");
        let job = f.open("job", "p-job");
        let jam = f.open("jam", "p-jam");
        let asked = Mutex::new(Vec::new());
        let Err(err) = resolve_job_target(&f.ctx, &f.store, None, &lookup(&[], &asked)) else {
            panic!("an ambiguous call must be refused");
        };
        let msg = err.to_string();
        assert!(matches!(err, Error::Refused(_)), "{msg}");
        assert!(
            msg.contains("2 open job plans") && msg.contains("pass plan_id"),
            "{msg}"
        );
        assert!(
            msg.contains(&format!("plan {job} (job, pod offrig-{}-job)", f.tag)),
            "{msg}"
        );
        assert!(
            msg.contains(&format!("plan {jam} (jam, pod offrig-{}-jam)", f.tag)),
            "{msg}"
        );
        // Refused before any pod was looked up.
        assert!(asked.lock().expect("asked").is_empty());
    }

    #[test]
    fn plan_id_selects_that_plans_pod_and_no_other() {
        let f = Fixture::new("select");
        let job = f.open("job", "p-job");
        let jam = f.open("jam", "p-jam");
        let pods = [
            named_pod("p-job", &format!("offrig-{}-job", f.tag)),
            named_pod("p-jam", &format!("offrig-{}-jam", f.tag)),
        ];
        for (want, pod_id) in [(jam, "p-jam"), (job, "p-job")] {
            let asked = Mutex::new(Vec::new());
            let t = resolve_job_target(&f.ctx, &f.store, Some(want), &lookup(&pods, &asked))
                .expect("the named plan's pod");
            assert_eq!((t.plan.id, t.pod.id.as_str()), (want, pod_id));
            assert_eq!(t.cfg.ssh_alias, format!("offrig-{}", f.tag));
            assert_eq!(*asked.lock().expect("asked"), vec![pod_id.to_string()]);
        }
    }

    #[test]
    fn a_pod_the_plan_does_not_own_is_refused() {
        let f = Fixture::new("not-owned");
        let job = f.open("job", "p-job");
        let jam_name = format!("offrig-{}-jam", f.tag);
        let cases = [
            // The lane's other pod: the lane owns the name, the plan does not.
            ("p-job", jam_name.as_str(), "is not the pod plan"),
            // Another lane's pod, the plain lane's, and a studio job.
            (
                "p-job",
                "offrig-someone-else-job",
                "not in this project's lane",
            ),
            ("p-job", "offrig-job", "not in this project's lane"),
            ("p-job", "training-run", "not in this project's lane"),
        ];
        for (id, name, expect) in cases {
            let pods = [named_pod(id, name)];
            let asked = Mutex::new(Vec::new());
            let Err(err) = resolve_job_target(&f.ctx, &f.store, Some(job), &lookup(&pods, &asked))
            else {
                panic!("{name} must be refused");
            };
            let msg = err.to_string();
            assert!(
                matches!(err, Error::Refused(_)) && msg.contains(expect),
                "{name}: {msg}"
            );
        }
        // The same check holds when the plan is picked implicitly (one open plan).
        let pods = [named_pod("p-job", &jam_name)];
        let asked = Mutex::new(Vec::new());
        assert!(resolve_job_target(&f.ctx, &f.store, None, &lookup(&pods, &asked)).is_err());
    }

    #[test]
    fn one_open_job_plan_and_no_plan_id_works_as_before() {
        let f = Fixture::new("single");
        let job = f.open("job", "p-job");
        // A model plan open beside it is not a job plan and does not make it ambiguous.
        f.open("small", "p-small");
        let pods = [named_pod("p-job", &format!("offrig-{}-job", f.tag))];
        let asked = Mutex::new(Vec::new());
        let t = resolve_job_target(&f.ctx, &f.store, None, &lookup(&pods, &asked))
            .expect("the only job plan");
        assert_eq!((t.plan.id, t.pod.id.as_str()), (job, "p-job"));
        // No job plan at all keeps its old refusal.
        let empty = Fixture::new("none");
        let Err(err) = resolve_job_target(&empty.ctx, &empty.store, None, &lookup(&pods, &asked))
        else {
            panic!("no job pod");
        };
        assert!(err.to_string().contains("no launched job pod"), "{err}");
    }

    #[test]
    fn plan_id_must_name_an_open_job_plan_with_a_pod() {
        let f = Fixture::new("bad-ids");
        let model = f.open("small", "p-small");
        let waiting = {
            let id = f.planned("job");
            f.store.commit_plan(id).expect("commit");
            id
        };
        let closed = f.open("jam", "p-jam");
        f.store.close_plan(closed, 0.0).expect("close");
        let never = f.planned("job");
        let asked = Mutex::new(Vec::new());
        for (id, expect) in [
            (9999, "no plan 9999"),
            (model, "not a job pod"),
            (waiting, "has no pod yet"),
            (closed, "not an open plan"),
            (never, "not an open plan"),
        ] {
            let Err(err) = resolve_job_target(&f.ctx, &f.store, Some(id), &lookup(&[], &asked))
            else {
                panic!("plan {id} must be refused");
            };
            assert!(err.to_string().contains(expect), "{id}: {err}");
        }
        assert!(asked.lock().expect("asked").is_empty());
    }

    #[test]
    fn a_second_launch_on_a_lane_with_a_live_plan_is_refused() {
        let f = Fixture::new("busy-plan");
        let first = f.open("job", "p-job");
        let second = f.planned("jam");
        let cfg = f.ctx.own_cfg().expect("cfg");
        let err = lane_busy(&f.ctx, &f.store, &cfg, second, &[]).expect_err("lane is busy");
        assert_eq!(
            err.to_string(),
            format!(
                "refused: lane {0} has a live pod offrig-{0}-job (plan {first}); shut it down first",
                f.tag
            )
        );
        // Once the first plan is closed the lane is free again.
        f.store.close_plan(first, 0.0).expect("close");
        lane_busy(&f.ctx, &f.store, &cfg, second, &[]).expect("free");
        // The plan being launched never blocks itself.
        let third = f.open("jam", "p-jam");
        lane_busy(&f.ctx, &f.store, &cfg, third, &[]).expect("only itself");
    }

    #[test]
    fn a_live_pod_of_the_lane_blocks_a_launch_even_with_no_plan() {
        let f = Fixture::new("busy-pod");
        let planned = f.planned("jam");
        let cfg = f.ctx.own_cfg().expect("cfg");
        let live = named_pod("p9", &format!("offrig-{}-job", f.tag));
        let err = lane_busy(&f.ctx, &f.store, &cfg, planned, &[live]).expect_err("pod is live");
        assert_eq!(
            err.to_string(),
            format!(
                "refused: lane {0} has a live pod offrig-{0}-job (no open plan); shut it down first",
                f.tag
            )
        );
        // Pods that are not the lane's, or are gone, never block it.
        let mut gone = named_pod("p8", &format!("offrig-{}-job", f.tag));
        gone.desired_status = "TERMINATED".into();
        let others = [
            gone,
            named_pod("p7", "offrig-someone-else-job"),
            named_pod("p6", "offrig-job"),
            named_pod("p5", "training-run"),
        ];
        lane_busy(&f.ctx, &f.store, &cfg, planned, &others).expect("none is this lane's");
    }

    #[test]
    fn another_lanes_plan_does_not_block_this_lane() {
        let f = Fixture::new("other-lane");
        // A plan from before lanes holds a plain-lane pod: a different alias.
        let id = f
            .store
            .create_plan(NewPlan {
                profile: "job".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 1.0,
                max_price_hr: 0.5,
                note: None,
            })
            .expect("plan")
            .id;
        f.store.commit_plan(id).expect("commit");
        f.store.attach_pod(id, "p-old").expect("attach");
        let planned = f.planned("jam");
        let cfg = f.ctx.own_cfg().expect("cfg");
        lane_busy(&f.ctx, &f.store, &cfg, planned, &[]).expect("plain lane is another alias");
    }

    #[test]
    fn every_job_reply_states_the_project_lane_and_plan() {
        let f = Fixture::new("reply");
        let v = acted_on(&f.ctx, 7, Some(f.tag.clone()), json!({"name": "render"}));
        assert_eq!(v["plan_id"], 7);
        assert_eq!(v["lane"], f.tag.as_str());
        assert_eq!(v["name"], "render");
        assert_eq!(v["project"], f.ctx.project.display().to_string());
    }

    fn booting(status: &str, endpoint: bool) -> Pod {
        let mut v =
            json!({"id": "p1", "name": "offrig-x-job", "desiredStatus": status, "costPerHr": 2.09});
        if endpoint {
            v["publicIp"] = json!("203.0.113.9");
            v["portMappings"] = json!({"22": 40022});
        }
        serde_json::from_value(v).expect("pod")
    }

    #[test]
    fn the_live_step_follows_the_pod_not_the_launchs_last_event() {
        // No address yet: the wait for the image, with how long it has been.
        let s = live_step(&booting("RUNNING", false), SshProbe::NoEndpoint, Some(582));
        assert!(
            s.starts_with("waiting for the pod to pull its image"),
            "{s}"
        );
        assert!(s.contains("582s after it was created"), "{s}");
        // The address is out and sshd answers: the step says so, and drops the old wait.
        let s = live_step(&booting("RUNNING", true), SshProbe::Answers, Some(700));
        assert!(s.starts_with("ssh answers at 203.0.113.9:40022"), "{s}");
        assert!(!s.contains("pull its image"), "{s}");
        // The address is out and sshd is not answering yet.
        let s = live_step(&booting("RUNNING", true), SshProbe::Silent, None);
        assert!(
            s.contains("203.0.113.9:40022") && s.contains("not answering yet"),
            "{s}"
        );
        // A pod that is not running is said to be so.
        let s = live_step(&booting("EXITED", false), SshProbe::NoEndpoint, Some(30));
        assert!(s.starts_with("the pod is EXITED"), "{s}");
    }
}
