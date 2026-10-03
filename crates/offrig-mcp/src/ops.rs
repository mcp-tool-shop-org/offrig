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
use offrig_core::ollama::Ollama;
use offrig_core::remote::PullState;
use offrig_core::runpod::RunPod;
use offrig_core::session::{Event, Session, Wait};
use offrig_core::store::{State, Store};
use offrig_core::tunnel::Tunnel;
use offrig_core::{Error, Result, roles, runner, spec, watchdog};
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
}

impl Progress<'_> {
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
                self.push();
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
    cfg: Config,
    db: PathBuf,
    shared: Arc<Shared>,
    plan_id: i64,
    job_id: i64,
    cancel: Arc<AtomicBool>,
) {
    let Ok(store) = Store::open(&db) else {
        return;
    };
    let result = launch(&cfg, &db, &store, &shared, plan_id, job_id, &cancel);
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

fn launch(
    cfg: &Config,
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
    let profile = cfg.profile(&plan.profile)?.clone();
    let session = Session::with_client(cfg.clone(), RunPod::from_env()?);
    let mut progress = Progress {
        store,
        job: job_id,
        v: json!({ "step": "starting" }),
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

    let body = spec::pod_create(cfg, &profile);
    let now = now_unix();
    let left = plan.deadline().map_or(0, |d| (d - now).max(0)) as u64;
    let wait = Wait {
        limit: Duration::from_secs((u64::from(profile.wait_for_gpu_minutes) * 60).min(left)),
        poll: Duration::from_secs(60),
    };
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
    progress.v["cost_per_hr"] = json!(pod.cost_per_hr);
    progress.step(format!(
        "pod {} created at ${:.2}/hr",
        pod.id, pod.cost_per_hr
    ));

    // From here the pod bills. If getting it ready fails, terminate it rather than
    // leave it running unused until the deadline (the launch's compensator).
    let ready = (|| -> Result<Value> {
        session.wait_ready(&pod.id, &mut |e| progress.event(e))?;
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled("getting the pod ready".into()));
        }
        let tunnel = session.open_tunnel(&profile, &mut |e| progress.event(e))?;
        *lock(&shared.tunnel) = Some(tunnel);
        session.ensure_models(&profile, &mut |e| progress.event(e))?;
        Ok(json!({
            "step": "ready",
            "pod_id": pod.id,
            "cost_per_hr": pod.cost_per_hr,
            "tunnel": cfg.tunnel_base_url(),
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
                Ok(()) => "terminated after a failed launch".to_string(),
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

/// Reopen the tunnel to the plan's pod when it is missing or dead.
fn ensure_tunnel(cfg: &Config, store: &Store, shared: &Shared) -> Result<String> {
    let alive = lock(&shared.tunnel).as_mut().is_some_and(Tunnel::is_alive);
    let plan = store
        .open_plans()?
        .into_iter()
        .find(|p| p.pod_id.is_some())
        .ok_or_else(|| Error::Refused("no launched session; launch a plan first".into()))?;
    if alive {
        return Ok(plan.profile);
    }
    let session = Session::with_client(cfg.clone(), RunPod::from_env()?);
    let pod_id = plan.pod_id.clone().unwrap_or_default();
    let pod = session.rp.get_pod(&pod_id)?;
    session.write_ssh(&pod)?;
    let t = session.open_tunnel(cfg.profile(&plan.profile)?, &mut |_| {})?;
    *lock(&shared.tunnel) = Some(t);
    Ok(plan.profile)
}

/// One role-headed turn for a handoff, context built from the project store.
pub fn ask(
    cfg: &Config,
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
    let profile_name = ensure_tunnel(cfg, &store, shared)?;
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
pub fn shutdown(db: &Path, shared: &Shared, plan_id: i64, reason: Option<&str>) -> Result<Value> {
    let store = Store::open(db)?;
    let plan = store
        .plan(plan_id)?
        .ok_or_else(|| Error::Refused(format!("no plan {plan_id}")))?;
    if plan.state != "committed" {
        return Ok(json!({ "plan_id": plan_id, "already": plan.state, "budget": store.budget()? }));
    }
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
    if let Some(c) = lock(&shared.cancels).get(&plan_id) {
        c.store(true, Ordering::SeqCst);
    }
    lock(&shared.tunnel).take();
    let j = store.journal(
        "terminate_pod",
        Some(plan_id),
        &json!({ "pod": plan.pod_id, "reason": reason.unwrap_or("shutdown"), "in_flight": in_flight }),
    )?;
    let mut rate = plan.max_price_hr;
    let outcome = match plan.pod_id.as_deref() {
        Some(id) => {
            let rp = RunPod::from_env()?;
            if let Ok(p) = rp.get_pod(id) {
                rate = p.cost_per_hr;
            }
            match rp.delete_pod(id) {
                Ok(()) => format!("terminated {id}"),
                // Already gone is the state we want.
                Err(Error::Api { status: 404, .. }) => format!("{id} was already gone"),
                Err(e) => return Err(e),
            }
        }
        None => "no pod was rented".to_string(),
    };
    store.journal_outcome(j, &outcome)?;
    let spent = if plan.pod_id.is_some() {
        watchdog::spend(&plan, rate, now_unix())
    } else {
        0.0
    };
    let budget = store.close_plan(plan_id, spent)?;
    Ok(json!({
        "plan_id": plan_id,
        "outcome": outcome,
        "spent": spent,
        "budget": budget,
        "terminated_with_in_flight": in_flight,
    }))
}
