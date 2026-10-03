//! The handoff runner: a detached process (`offrig-mcp --runner <plan>`) that works the
//! queue on the pod model until it is empty, then shuts the pod down.
//!
//! Decisions come from `offrig_core::runner` (pure and tested); this module acts on
//! them. Evidence and the design are in docs/sidecar-design.md, phase 3a.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use offrig_core::checks;
use offrig_core::config::{Config, Profile};
use offrig_core::cost::now_unix;
use offrig_core::error::chain;
use offrig_core::ollama::Ollama;
use offrig_core::roles;
use offrig_core::runner::{self, Next};
use offrig_core::runpod::RunPod;
use offrig_core::session::Session;
use offrig_core::store::{Handoff, NewOutput, NewRecord, State, Store};
use offrig_core::tunnel::Tunnel;
use offrig_core::{Error, Result};
use serde_json::json;

use crate::ops;

/// Room for the role block, memory, dependency results and a previous draft.
pub const RUN_CONTEXT_CHARS: usize = 40_000;
/// Per-turn generation cap, thinking included. A thinking model can spend thousands
/// of tokens before answering (rehearsal, 2026-10-03).
pub const RUN_MAX_TOKENS: u32 = 8_192;
/// A failed or timed-out handoff is retried by the runner until it has had this many
/// attempts; after that it waits for the agent.
pub const MAX_ATTEMPTS: i64 = 2;
/// Stop starting new handoffs this close to the plan deadline.
pub const DEADLINE_MARGIN_SECS: i64 = 120;
/// The runner reports in this often; status treats it as gone after three misses.
pub const HEARTBEAT_SECS: i64 = 20;

pub fn heartbeat_key(plan_id: i64) -> String {
    format!("runner:{plan_id}")
}

/// Whether the plan's runner has reported in recently.
pub fn alive(store: &Store, plan_id: i64, now: i64) -> bool {
    store
        .setting(&heartbeat_key(plan_id))
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .is_some_and(|t| now - t <= HEARTBEAT_SECS * 3)
}

#[derive(Default)]
struct Stats {
    tokens: AtomicI64,
    turns: AtomicI64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn log(project: &Path, plan_id: i64, msg: &str) {
    use std::io::Write;
    let path = project
        .join(".offrig")
        .join(format!("runner-{plan_id}.log"));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let now = now_unix();
        let secs = now.rem_euclid(86_400);
        let _ = writeln!(
            f,
            "{} {:02}:{:02}:{:02} UTC {msg}",
            offrig_core::cost::date_utc(now),
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        );
    }
}

/// Where the pod model answers: through the runner's own tunnel, or (debug builds,
/// tests only) a base URL from the environment.
fn endpoint(cfg: &Config, profile: &Profile, plan_pod: &str) -> Result<(String, Option<Tunnel>)> {
    #[cfg(debug_assertions)]
    if let Ok(base) = std::env::var("OFFRIG_TEST_OLLAMA_BASE") {
        return Ok((base, None));
    }
    let mut own = cfg.clone();
    own.tunnel_port = cfg.tunnel_port + 1;
    if own.tunnel_port == 11434 {
        return Err(Error::Refused(
            "the runner's tunnel port would be the local Ollama's".into(),
        ));
    }
    let session = Session::with_client(own.clone(), RunPod::from_env()?);
    let pod = session.rp.get_pod(plan_pod)?;
    session.write_ssh(&pod)?;
    let t = session.open_tunnel(profile, &mut |_| {})?;
    Ok((own.tunnel_base_url(), Some(t)))
}

pub fn run(cfg: &Config, project: &Path, plan_id: i64, keep_pod: bool) -> anyhow::Result<()> {
    let db = project.join(".offrig").join("offrig.db");
    let store = Store::open(&db)?;
    let plan = store
        .plan(plan_id)?
        .ok_or_else(|| Error::Refused(format!("no plan {plan_id}")))?;
    let pod_id = plan
        .pod_id
        .clone()
        .ok_or_else(|| Error::Refused(format!("plan {plan_id} has no pod")))?;
    let profile = cfg.profile(&plan.profile)?.clone();
    let model = profile
        .models
        .first()
        .map(|m| m.name.clone())
        .ok_or_else(|| Error::Refused(format!("profile {} lists no models", profile.name)))?;
    let job = match store.job_for_plan(plan_id, "run")? {
        Some(j) => j.id,
        None => store.create_job(plan_id, "run")?,
    };
    let slots = profile.parallel.max(1) as usize + 1;
    log(
        project,
        plan_id,
        &format!("runner started: {model}, {slots} in flight"),
    );

    let (base, mut tunnel) = match endpoint(cfg, &profile, &pod_id) {
        Ok(v) => v,
        Err(e) => {
            store.update_job(
                job,
                "failed",
                &json!({"step": "no tunnel"}),
                Some(&chain(&e)),
            )?;
            log(project, plan_id, &format!("no tunnel: {}", chain(&e)));
            return Err(e.into());
        }
    };
    let role_os = cfg.role_os_dir.clone().map(PathBuf::from);
    let stats = Arc::new(Stats::default());
    let stop = Arc::new(AtomicBool::new(false));
    let busy: Arc<Mutex<HashSet<i64>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    let started = Instant::now();

    let outcome = loop {
        let now = now_unix();
        store.set_setting(&heartbeat_key(plan_id), &now.to_string())?;
        let Some(plan) = store.plan(plan_id)? else {
            break "plan vanished";
        };
        if plan.state != "committed" {
            break "the plan was closed";
        }
        if let Some(t) = tunnel.as_mut()
            && !t.is_alive()
        {
            log(project, plan_id, "tunnel died; reopening");
            tunnel = match endpoint(cfg, &profile, &pod_id) {
                Ok((_, t)) => t,
                Err(e) => {
                    log(project, plan_id, &format!("reopen failed: {}", chain(&e)));
                    None
                }
            };
        }
        workers.retain(|w| !w.is_finished());
        let near_deadline = plan
            .deadline()
            .is_some_and(|d| d - now < DEADLINE_MARGIN_SECS);

        let all = store.handoffs()?;
        let in_busy = lock(&busy).clone();
        // Retry what failed, once; adopt work sent back from review.
        for h in &all {
            if matches!(h.state, State::Failed | State::TimedOut) && h.attempts < MAX_ATTEMPTS {
                store.transition(h.id, State::Dispatched, "runner retry", None)?;
            }
        }
        let all = store.handoffs()?;
        let mut startable: Vec<Handoff> = all
            .iter()
            .filter(|h| h.state == State::Dispatched && !in_busy.contains(&h.id))
            .cloned()
            .collect();
        startable.extend(store.ready()?);

        if !near_deadline {
            while lock(&busy).len() < slots {
                let taken = lock(&busy).clone();
                let Some(id) = runner::pick(&startable, &all, &taken) else {
                    break;
                };
                let h = store
                    .handoff(id)?
                    .ok_or_else(|| Error::Refused(format!("no handoff {id}")))?;
                if h.state == State::Pending {
                    store.transition(id, State::Dispatched, "picked by the runner", None)?;
                }
                lock(&busy).insert(id);
                let ctx = Worker {
                    db: db.clone(),
                    project: project.to_path_buf(),
                    plan_id,
                    role_os: role_os.clone(),
                    base: base.clone(),
                    model: model.clone(),
                    stats: Arc::clone(&stats),
                    busy: Arc::clone(&busy),
                    stop: Arc::clone(&stop),
                };
                workers.push(std::thread::spawn(move || ctx.work(id)));
            }
        }

        let all = store.handoffs()?;
        let count = |s: State| all.iter().filter(|h| h.state == s).count();
        let in_flight: Vec<i64> = lock(&busy).iter().copied().collect();
        let secs = started.elapsed().as_secs_f64().max(1.0);
        let tokens = stats.tokens.load(Ordering::SeqCst);
        store.update_job(
            job,
            "running",
            &json!({
                "step": if near_deadline { "near the deadline: finishing, starting nothing new" } else { "working the queue" },
                "model": model,
                "slots": slots,
                "in_flight": in_flight,
                "complete": count(State::Complete),
                "review": count(State::Review),
                "pending": count(State::Pending),
                "failed": count(State::Failed) + count(State::TimedOut),
                "blocked": count(State::InvalidOutput) + count(State::OwnershipViolation),
                "turns": stats.turns.load(Ordering::SeqCst),
                "tokens": tokens,
                "tok_per_s": (tokens as f64 / secs * 10.0).round() / 10.0,
                "at": now,
            }),
            None,
        )?;

        let retryable = all.iter().any(|h| {
            matches!(h.state, State::Failed | State::TimedOut) && h.attempts < MAX_ATTEMPTS
        });
        let waiting = all.iter().any(|h| h.state == State::Dispatched);
        if in_flight.is_empty()
            && !retryable
            && !waiting
            && (store.ready()?.is_empty() || near_deadline)
        {
            break "the queue has nothing left the runner can work";
        }
        std::thread::sleep(poll());
    };

    stop.store(true, Ordering::SeqCst);
    for w in workers {
        let _ = w.join();
    }
    drop(tunnel);
    log(project, plan_id, &format!("runner stopping: {outcome}"));
    let mut progress = store
        .job(job)?
        .map(|j| j.progress)
        .unwrap_or_else(|| json!({}));
    progress["step"] = json!(outcome);
    let closing = outcome.starts_with("the queue") && !keep_pod;
    if closing {
        match ops::shutdown(
            &db,
            &ops::Shared::default(),
            plan_id,
            Some("runner: queue drained"),
        ) {
            Ok(v) => {
                progress["shutdown"] = v;
                log(project, plan_id, "pod shut down");
            }
            Err(e) => {
                progress["shutdown_error"] = json!(chain(&e));
                log(project, plan_id, &format!("shutdown failed: {}", chain(&e)));
            }
        }
    }
    store.update_job(job, "done", &progress, None)?;
    Ok(())
}

fn poll() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("OFFRIG_TEST_RUNNER_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return Duration::from_millis(ms);
    }
    Duration::from_secs(2)
}

struct Worker {
    db: PathBuf,
    project: PathBuf,
    plan_id: i64,
    role_os: Option<PathBuf>,
    base: String,
    model: String,
    stats: Arc<Stats>,
    busy: Arc<Mutex<HashSet<i64>>>,
    stop: Arc<AtomicBool>,
}

impl Worker {
    fn work(self, id: i64) {
        if let Err(e) = self.turns(id) {
            log(
                &self.project,
                self.plan_id,
                &format!("handoff {id}: {}", chain(&e)),
            );
            if let Ok(s) = Store::open(&self.db)
                && s.handoff(id)
                    .ok()
                    .flatten()
                    .is_some_and(|h| h.state.in_flight())
            {
                let _ = s.transition(id, State::Failed, &format!("runner: {}", chain(&e)), None);
            }
        }
        lock(&self.busy).remove(&id);
    }

    fn turns(&self, id: i64) -> Result<()> {
        let store = Store::open(&self.db)?;
        let h = store
            .handoff(id)?
            .ok_or_else(|| Error::Refused(format!("no handoff {id}")))?;
        let sent_back = sent_back_feedback(&store, id)?;
        store.transition(id, State::Running, "pod model working", None)?;
        let alive = Arc::new(AtomicBool::new(true));
        let beat = {
            let (db, alive) = (self.db.clone(), Arc::clone(&alive));
            std::thread::spawn(move || {
                let Ok(s) = Store::open(&db) else { return };
                while alive.load(Ordering::SeqCst) {
                    let _ = s.heartbeat(id);
                    std::thread::sleep(Duration::from_secs(HEARTBEAT_SECS as u64));
                }
            })
        };
        let result = self.turn_loop(&store, &h, sent_back);
        alive.store(false, Ordering::SeqCst);
        drop(beat);
        result
    }

    fn turn_loop(&self, store: &Store, h: &Handoff, sent_back: Option<String>) -> Result<()> {
        let ollama = Ollama::new(&self.base);
        let prior = store.outputs(h.id)?;
        let mut stored_turn = prior.iter().map(|o| o.turn).max().unwrap_or(0);
        let mut previous: Option<String> = None;
        let mut instruction = match (&sent_back, runner::best(&prior)) {
            (Some(fb), Some(best)) => format!(
                "A reviewer sent your previous output back with this feedback:\n{fb}\n\nRevise it to \
                 address exactly that. Keep everything else that works. Return the full revised \
                 deliverable, in Markdown, with no preamble.\n\nYour previous output:\n\n{}",
                best.body.trim()
            ),
            _ => runner::draft_instruction(h),
        };
        for turn in 1.. {
            if self.stop.load(Ordering::SeqCst) {
                return Err(Error::Refused("the runner is stopping".into()));
            }
            let prep = runner::prepare(
                store,
                self.role_os.as_deref(),
                h,
                &instruction,
                RUN_CONTEXT_CHARS,
            )?;
            store.set_run_details(
                h.id,
                &self.model,
                &roles::fingerprint(&prep.role_block),
                &roles::fingerprint(&prep.assembled.text),
            )?;
            let reply =
                ollama.chat_reply(&self.model, &prep.assembled.text, Some(RUN_MAX_TOKENS))?;
            self.stats.turns.fetch_add(1, Ordering::SeqCst);
            self.stats
                .tokens
                .fetch_add(reply.tokens.unwrap_or(0), Ordering::SeqCst);
            let outcomes = checks::evaluate(&reply.text, &runner::effective_checks(h));
            stored_turn += 1;
            store.add_output(NewOutput {
                handoff_id: h.id,
                turn: stored_turn,
                body: &reply.text,
                outcomes: &outcomes,
                model: &self.model,
                tokens: reply.tokens,
            })?;
            store.heartbeat(h.id)?;
            let next = match runner::after_turn(h, turn, &outcomes) {
                Next::Revise if previous.as_deref().is_some_and(|p| runner::stalled(p, &reply.text)) => {
                    Next::Review {
                        why: "the revision came back unchanged; the model is not acting on the failed checks".into(),
                    }
                }
                n => n,
            };
            match next {
                Next::Revise => {
                    instruction = runner::revise_instruction(&reply.text, &outcomes);
                    previous = Some(reply.text.clone());
                }
                Next::Complete => {
                    store.transition(
                        h.id,
                        State::Complete,
                        &format!("checks pass and cover the acceptance check (turn {stored_turn})"),
                        None,
                    )?;
                    return Ok(());
                }
                Next::Review { why } => {
                    store.transition(h.id, State::Review, &why, None)?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

/// The reviewer's feedback when the handoff was last sent back from review.
fn sent_back_feedback(store: &Store, id: i64) -> Result<Option<String>> {
    Ok(store
        .events(id)?
        .into_iter()
        .rev()
        .find(|(from, to, _, _)| from == "review" && to == "dispatched")
        .map(|(_, _, reason, _)| reason))
}

/// Record a reviewer's send-back as a checkpoint too, so any later turn sees it.
pub fn record_feedback(store: &Store, id: i64, feedback: &str) -> Result<i64> {
    store.record(NewRecord {
        kind: Some(offrig_core::store::Kind::Checkpoint),
        body: format!("Reviewer sent this back: {feedback}"),
        author: "reviewer".into(),
        source: Some(format!("handoff #{id} review")),
        task_id: Some(id),
        ..Default::default()
    })
}
