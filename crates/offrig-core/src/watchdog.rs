//! The plan watchdog: a separate process that holds a committed plan to its deadline
//! whether or not the agent, the side-car or this machine's user is still around.
//!
//! Evidence (docs/sidecar-design.md): provider budgets are alerts that lag hours, and
//! agents that lose track of resources keep billing. So the deadline is enforced from
//! the plan record by its own process, and the decision is a pure function so every
//! rule is tested.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cost::now_unix;
use crate::error::{Error, Result};
use crate::proc::{LivenessSnapshot, job_pid_target, launch_job_proven_dead_with_snapshot};
use crate::runpod::{Pod, RunPod};
use crate::store::{Job, Plan, Store};

/// Step/code recorded when a launch terminates unexpectedly while `create_pod` has no outcome.
pub const FAILED_CREATE_UNKNOWN_STEP: &str = "failed_create_unknown";
/// Journal outcome written when a closed plan's launcher died while `create_pod` was unresolved.
pub const ABANDONED_CREATE_UNKNOWN_OUTCOME: &str =
    "abandoned: launching process died after plan closed; whether a pod exists is unknown";
/// Settings key prefix storing candidate unmarked pods found after an abandoned `create_pod`.
pub const UNKNOWN_PODS_KEY_PREFIX: &str = "watchdog_unknown_pods:";

pub fn unknown_pods_key(plan_id: i64) -> String {
    format!("{UNKNOWN_PODS_KEY_PREFIX}{plan_id}")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnknownPodCandidate {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnknownCreateWarning {
    pub plan_id: i64,
    pub profile: String,
    pub lane: Option<String>,
    pub candidates: Vec<UnknownPodCandidate>,
}

/// Decision returned by [`check_relaunch_allowed_with_snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaunchGate {
    Allowed,
    AdoptExisting(String),
}

/// OS `(pid, pid_started_at)` probe target collected before entering a write transaction.
pub type PidProbeTarget = (u32, Option<String>);

/// Why [`check_relaunch_allowed_with_snapshot`] could not proceed inside a transaction.
#[derive(Debug)]
pub enum RelaunchBlock {
    Refused(Error),
    NeedProbe(PidProbeTarget),
}

/// True when `job` recorded `failed_create_unknown` (unexpected stop while `create_pod` had no outcome).
pub fn job_has_failed_create_unknown(job: &Job) -> bool {
    job.state == "failed"
        && (job.progress["step"] == FAILED_CREATE_UNKNOWN_STEP
            || job.progress["create_unknown"] == true)
}

/// Collect any `(pid, pid_started_at)` targets and whether `failed_create_unknown` is active
/// before acquiring a SQLite write lock.
pub fn collect_relaunch_probe_targets(
    store: &Store,
    plan_id: i64,
) -> Result<(Vec<PidProbeTarget>, bool)> {
    let latest_launch = store.job_for_plan(plan_id, "launch")?;
    let unfinished = store.unfinished_create_pod(plan_id)?;
    let creator_job = unfinished
        .as_ref()
        .and_then(|entry| {
            serde_json::from_str::<Value>(&entry.intent)
                .ok()
                .and_then(|v| v["job_id"].as_i64())
                .and_then(|jid| store.job(jid).ok().flatten())
        })
        .or_else(|| latest_launch.clone());
    let has_unknown_create = latest_launch
        .as_ref()
        .is_some_and(job_has_failed_create_unknown)
        || creator_job
            .as_ref()
            .is_some_and(job_has_failed_create_unknown);
    let mut targets = Vec::new();
    if let Some(j) = creator_job.as_ref().and_then(job_pid_target) {
        targets.push(j);
    }
    if let Some(j) = latest_launch.as_ref().and_then(job_pid_target)
        && !targets.contains(&j)
    {
        targets.push(j);
    }
    Ok((targets, has_unknown_create))
}

/// Pure store-and-snapshot relaunch check safe to run inside `BEGIN IMMEDIATE` without
/// spawning subprocesses. Callers pass `searched_pods = Some(pods)` once `list_pods` has run.
pub fn check_relaunch_allowed_with_snapshot(
    store: &Store,
    base_cfg: &crate::config::Config,
    plan_id: i64,
    now: i64,
    snap: &LivenessSnapshot,
    searched_pods: Option<&[Pod]>,
) -> std::result::Result<RelaunchGate, RelaunchBlock> {
    let latest_launch = store
        .job_for_plan(plan_id, "launch")
        .map_err(RelaunchBlock::Refused)?;
    let unfinished = store
        .unfinished_create_pod(plan_id)
        .map_err(RelaunchBlock::Refused)?;
    let creator_job = unfinished
        .as_ref()
        .and_then(|entry| {
            serde_json::from_str::<Value>(&entry.intent)
                .ok()
                .and_then(|v| v["job_id"].as_i64())
                .and_then(|jid| store.job(jid).ok().flatten())
        })
        .or_else(|| latest_launch.clone());

    let unknown_stands = latest_launch
        .as_ref()
        .is_some_and(job_has_failed_create_unknown)
        || creator_job
            .as_ref()
            .is_some_and(job_has_failed_create_unknown);

    if unknown_stands {
        let Some(pods) = searched_pods else {
            return Err(RelaunchBlock::Refused(Error::Refused(format!(
                "plan {plan_id} has failed_create_unknown from an interrupted launch; list_pods search for OFFRIG_PLAN={plan_id} is required before relaunch"
            ))));
        };
        let plan = store.plan(plan_id).map_err(RelaunchBlock::Refused)?;
        let lane_tag = store.plan_lane(plan_id).map_err(RelaunchBlock::Refused)?;
        let cfg = cfg_in_plan_lane(base_cfg, lane_tag.as_deref());
        let expected_lane = cfg.lane_tag.as_deref().unwrap_or("plain");
        let plan_str = plan_id.to_string();
        if let Some(found) = pods.iter().find(|p| {
            p.desired_status != "TERMINATED"
                && p.env.get(crate::spec::ENV_PLAN).map(String::as_str) == Some(plan_str.as_str())
                && p.env
                    .get(crate::spec::ENV_LANE)
                    .is_none_or(|l| l == expected_lane)
        }) {
            let expected_name = plan
                .as_ref()
                .map(|pl| expected_pod_name(cfg.lane_tag.as_deref(), &pl.profile));
            let adoptable = cfg.owns_pod(&found.name)
                && expected_name.as_deref() == Some(found.name.as_str())
                && matches!(
                    found.desired_status.as_str(),
                    "RUNNING" | "PROVISIONING" | "STARTING"
                );
            if adoptable {
                return Ok(RelaunchGate::AdoptExisting(found.id.clone()));
            }
            return Err(RelaunchBlock::Refused(Error::Refused(format!(
                "plan {plan_id} has failed_create_unknown and pod {} (status {}) carries OFFRIG_PLAN={plan_id}; clean up that pod before relaunching",
                found.id, found.desired_status
            ))));
        }
    } else if let Some(entry) = unfinished {
        let proven_dead = match creator_job.as_ref() {
            Some(job) => match launch_job_proven_dead_with_snapshot(job, now, snap) {
                Ok(dead) => dead,
                Err(target) => return Err(RelaunchBlock::NeedProbe(target)),
            },
            None => false,
        };
        if !proven_dead {
            return Err(RelaunchBlock::Refused(Error::Refused(format!(
                "plan {plan_id} has an unfinished create_pod journal entry (id {}) whose launching process is not proven dead; refusing relaunch so a second pod cannot be created",
                entry.id
            ))));
        }
    }

    if let Some(job) = latest_launch
        && job.state == "running"
        && job.progress["pid"].is_u64()
    {
        match launch_job_proven_dead_with_snapshot(&job, now, snap) {
            Ok(true) => {}
            Ok(false) => {
                return Err(RelaunchBlock::Refused(Error::Refused(format!(
                    "plan {plan_id} already has a running launch job {} (pid {}) that is not proven dead; follow it with offrig_job instead of relaunching",
                    job.id, job.progress["pid"]
                ))));
            }
            Err(target) => return Err(RelaunchBlock::NeedProbe(target)),
        }
    }

    Ok(RelaunchGate::Allowed)
}

/// Convenience wrapper for callers outside an explicit transaction: probes PID targets upfront
/// and checks relaunch eligibility (`searched_pods` must be `Some` to clear `failed_create_unknown`).
pub fn check_relaunch_allowed(
    store: &Store,
    plan_id: i64,
    now: i64,
    searched_pods: Option<&[Pod]>,
) -> Result<()> {
    let base_cfg = crate::config::Config::load().unwrap_or_default();
    let (targets, _) = collect_relaunch_probe_targets(store, plan_id)?;
    let snap = LivenessSnapshot::probe(&targets);
    match check_relaunch_allowed_with_snapshot(store, &base_cfg, plan_id, now, &snap, searched_pods)
    {
        Ok(RelaunchGate::Allowed) => Ok(()),
        Ok(RelaunchGate::AdoptExisting(pod_id)) => Err(Error::Refused(format!(
            "plan {plan_id} has failed_create_unknown and pod {pod_id} carries OFFRIG_PLAN={plan_id}; adopt that pod instead of creating a new one"
        ))),
        Err(RelaunchBlock::Refused(e)) => Err(e),
        Err(RelaunchBlock::NeedProbe((pid, start))) => {
            let mut combined = targets;
            combined.push((pid, start));
            let snap2 = LivenessSnapshot::probe(&combined);
            match check_relaunch_allowed_with_snapshot(
                store,
                &base_cfg,
                plan_id,
                now,
                &snap2,
                searched_pods,
            ) {
                Ok(RelaunchGate::Allowed) => Ok(()),
                Ok(RelaunchGate::AdoptExisting(pod_id)) => Err(Error::Refused(format!(
                    "plan {plan_id} has failed_create_unknown and pod {pod_id} carries OFFRIG_PLAN={plan_id}; adopt that pod instead of creating a new one"
                ))),
                Err(RelaunchBlock::Refused(e)) => Err(e),
                Err(RelaunchBlock::NeedProbe(_)) => Err(Error::Refused(format!(
                    "plan {plan_id} relaunch probe remained unresolved"
                ))),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Nothing to do yet.
    Wait,
    /// The deadline has passed and the pod is still up: terminate it.
    Terminate { reason: String },
    /// The pod is gone (or never came): close the plan's books.
    Close { reason: String },
    /// The plan is no longer committed: the watchdog's job is over.
    Exit,
}

/// What RunPod said about the plan's pod. `Unknown` (a failed lookup) never
/// triggers an action: the watchdog only acts on facts.
#[derive(Debug, Clone, PartialEq)]
pub enum PodSeen {
    Present(Box<Pod>),
    Absent,
    Unknown,
}

pub fn decide(plan: &Plan, seen: &PodSeen, now: i64) -> Verdict {
    if plan.state != "committed" {
        return Verdict::Exit;
    }
    let Some(deadline) = plan.deadline() else {
        return Verdict::Wait;
    };
    let past = now >= deadline;
    match (plan.pod_id.as_deref(), seen) {
        (_, PodSeen::Unknown) => Verdict::Wait,
        (None, _) if past => Verdict::Close {
            reason: "deadline passed before any pod was rented".into(),
        },
        (None, _) => Verdict::Wait,
        (Some(id), PodSeen::Absent) => Verdict::Close {
            reason: format!("pod {id} no longer exists"),
        },
        (Some(id), PodSeen::Present(p)) if past => Verdict::Terminate {
            reason: if crate::spec::pod_is_stopped(&p.desired_status) {
                format!(
                    "plan deadline reached and pod {id} is {} ({}); terminating pod {id}",
                    p.desired_status,
                    crate::spec::SELF_STOP_STOPPED_LINE
                )
            } else {
                format!("plan deadline reached; terminating pod {id}")
            },
        },
        (Some(_), PodSeen::Present(_)) => Verdict::Wait,
    }
}

/// Spend for the time the pod ran: rate x hours since it started (or since the plan
/// was committed, if the start was never recorded).
pub fn spend(plan: &Plan, rate_per_hr: f64, now: i64) -> f64 {
    let from = plan.started_at.or(plan.committed_at).unwrap_or(now);
    let hours = (now - from).max(0) as f64 / 3600.0;
    (rate_per_hr * hours * 100.0).ceil() / 100.0
}

/// Spend when the pod is already gone: clamp at `max(min(now, deadline + SELF_STOP_MARGIN_SECS), last_seen)`
/// only when the plan's stored self-stop status proves the on-pod backstop timer armed;
/// otherwise (UNAVAILABLE, unknown, or STOPPED NOT TERMINATED) book up to `now`.
pub fn spend_when_gone(
    plan: &Plan,
    rate_per_hr: f64,
    now: i64,
    self_stop: Option<&str>,
    last_seen: Option<i64>,
) -> f64 {
    let end = match plan.deadline() {
        Some(d) if crate::spec::self_stop_is_armed(self_stop) => {
            let clamped = now.min(d.saturating_add(crate::spec::SELF_STOP_MARGIN_SECS));
            last_seen.map_or(clamped, |ls| clamped.max(ls))
        }
        _ => now,
    };
    spend(plan, rate_per_hr, end)
}

pub fn heartbeat_key(plan_id: i64) -> String {
    format!("watchdog:{plan_id}")
}

pub fn error_key(plan_id: i64) -> String {
    format!("watchdog_error:{plan_id}")
}

/// Record that the plan's watchdog failed to start or cannot guard, and clear its
/// heartbeat so status and the app never mistake it for guarding.
pub fn record_error(store: &Store, plan_id: i64, reason: &str) -> Result<()> {
    store.set_setting(&error_key(plan_id), &crate::trace::redact(reason))?;
    store.clear_setting(&heartbeat_key(plan_id))
}

/// Why the plan's watchdog is not guarding, if it recorded a failure.
pub fn failure(store: &Store, plan_id: i64) -> Result<Option<String>> {
    Ok(store
        .setting(&error_key(plan_id))?
        .filter(|s| !s.trim().is_empty()))
}

/// Whether the plan's watchdog has reported in within `max_age_secs` and has no recorded failure.
pub fn alive(store: &Store, plan_id: i64, now: i64, max_age_secs: i64) -> bool {
    if failure(store, plan_id).ok().flatten().is_some() {
        return false;
    }
    store
        .setting(&heartbeat_key(plan_id))
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .is_some_and(|t| now - t <= max_age_secs)
}

/// An open committed plan whose watchdog recorded a failure and is not guarding.
#[derive(Debug, Clone, PartialEq)]
pub struct UnguardedPlan {
    pub project: String,
    pub lane: Option<String>,
    pub plan_id: i64,
    pub profile: String,
    pub pod_id: Option<String>,
    pub error: String,
}

/// Read-only scan across known projects for open plans whose watchdog is not guarding.
pub fn unguarded_in_scope(scope: &crate::account::Scope) -> Vec<UnguardedPlan> {
    let mut out = Vec::new();
    for key in scope.known().0 {
        let db = std::path::Path::new(&key).join(".offrig").join("offrig.db");
        if !db.is_file() {
            continue;
        }
        let Ok(s) = Store::open_read_only(&db) else {
            continue;
        };
        let Ok(plans) = s.open_plans() else {
            continue;
        };
        for p in plans {
            if let Ok(Some(err)) = failure(&s, p.id) {
                out.push(UnguardedPlan {
                    project: crate::account::folder_name(&key),
                    lane: s.plan_lane(p.id).ok().flatten(),
                    plan_id: p.id,
                    profile: p.profile,
                    pod_id: p.pod_id,
                    error: err,
                });
            }
        }
    }
    out
}

/// How an unattached pod was matched back to its plan after a crash or restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PodMatch {
    Journal,
    EnvPlan,
    PodName,
}

impl PodMatch {
    pub fn as_str(self) -> &'static str {
        match self {
            PodMatch::Journal => "journal",
            PodMatch::EnvPlan => "env_plan",
            PodMatch::PodName => "pod_name",
        }
    }
}

/// The pod name a plan gets in its lane.
pub fn expected_pod_name(lane_tag: Option<&str>, profile: &str) -> String {
    match lane_tag {
        None => format!("offrig-{profile}"),
        Some(tag) => format!("offrig-{tag}-{profile}"),
    }
}

/// Build a `Config` view in `lane_tag` (`None` is the plain lane) without
/// mutating `lanes.toml`.
pub fn cfg_in_plan_lane(
    base: &crate::config::Config,
    lane_tag: Option<&str>,
) -> crate::config::Config {
    let mut cfg = base.clone();
    cfg.lane_tag = lane_tag.map(str::to_string);
    cfg
}

/// Find a live pod belonging to `plan` when `plan.pod_id` is still unset:
/// - filters every candidate through `cfg.owns_pod(&p.name)` (identical to `ops` probe)
///   and the plan's expected lane pod name, never adopting another lane's or empty-named pod;
/// - checks `journal_pod_id` first, then `OFFRIG_PLAN` marker (in the plan's lane);
/// - falls back to `PodName` ONLY when the API listing omitted `env` for every pod
///   (`listing_omitted_env`) AND the plan is in a named project lane (`cfg.lane_tag.is_some()`).
///   A plain-lane plan never adopts a pod by name alone.
///
/// Pods already attached to another open plan (`taken_pod_ids`) are ignored.
pub fn match_plan_pod<'a>(
    plan: &Plan,
    cfg: &crate::config::Config,
    journal_pod_id: Option<&str>,
    taken_pod_ids: &[&str],
    pods: &'a [Pod],
    listing_omitted_env: bool,
) -> Option<(&'a Pod, PodMatch)> {
    let expected_name = expected_pod_name(cfg.lane_tag.as_deref(), &plan.profile);
    let expected_lane = cfg.lane_tag.as_deref().unwrap_or("plain");
    let is_owned_live = |p: &&Pod| {
        p.desired_status != "TERMINATED"
            && !taken_pod_ids.contains(&p.id.as_str())
            && cfg.owns_pod(&p.name)
    };
    if let Some(jid) = journal_pod_id
        && let Some(p) = pods
            .iter()
            .filter(is_owned_live)
            .find(|p| p.id == jid && p.name == expected_name)
    {
        return Some((p, PodMatch::Journal));
    }
    let plan_str = plan.id.to_string();
    if let Some(p) = pods.iter().filter(is_owned_live).find(|p| {
        p.name == expected_name
            && p.env.get(crate::spec::ENV_PLAN).map(String::as_str) == Some(plan_str.as_str())
            && p.env
                .get(crate::spec::ENV_LANE)
                .is_none_or(|l| l == expected_lane)
    }) {
        return Some((p, PodMatch::EnvPlan));
    }
    if listing_omitted_env
        && cfg.lane_tag.is_some()
        && let Some(p) = pods.iter().filter(is_owned_live).find(|p| {
            p.name == expected_name
                && p.env
                    .get(crate::spec::ENV_PLAN)
                    .is_none_or(|v| v == &plan_str)
        })
    {
        return Some((p, PodMatch::PodName));
    }
    None
}

fn look(rp: &RunPod, pod_id: Option<&str>) -> PodSeen {
    let Some(id) = pod_id else {
        return PodSeen::Absent;
    };
    match rp.list_pods() {
        Ok(pods) => match pods.into_iter().find(|p| p.id == id) {
            Some(p) if p.desired_status != "TERMINATED" => PodSeen::Present(Box::new(p)),
            _ => PodSeen::Absent,
        },
        Err(_) => PodSeen::Unknown,
    }
}

fn sweep_unresolved_duplicates(
    store: &Store,
    base_cfg: &crate::config::Config,
    rp: &RunPod,
    plan: &Plan,
) -> Result<bool> {
    let dups = store.unresolved_duplicate_pods_for_plan(plan.id)?;
    if dups.is_empty() {
        return Ok(true);
    }
    let Ok(pods) = rp.list_pods() else {
        return Ok(false);
    };
    let lane_tag = store.plan_lane(plan.id)?;
    let cfg = cfg_in_plan_lane(base_cfg, lane_tag.as_deref());
    let expected_name = expected_pod_name(cfg.lane_tag.as_deref(), &plan.profile);
    let open = store.open_plans()?;
    let mut all_resolved = true;
    for dup in dups {
        if plan.pod_id.as_deref() == Some(dup.pod_id.as_str())
            || open
                .iter()
                .any(|p| p.pod_id.as_deref() == Some(dup.pod_id.as_str()))
        {
            store.journal_outcome(dup.journal_id, "refused sweep: pod is held by an open plan")?;
            continue;
        }
        match pods.iter().find(|p| p.id == dup.pod_id) {
            None => {
                store.journal_outcome(
                    dup.journal_id,
                    &format!(
                        "duplicate pod {} already gone on watchdog sweep",
                        dup.pod_id
                    ),
                )?;
            }
            Some(p) if p.desired_status == "TERMINATED" => {
                store.journal_outcome(
                    dup.journal_id,
                    &format!(
                        "duplicate pod {} already terminated on watchdog sweep",
                        dup.pod_id
                    ),
                )?;
            }
            Some(p) if !cfg.owns_pod(&p.name) || p.name != expected_name => {
                store.journal_outcome(
                    dup.journal_id,
                    &format!(
                        "refused sweep of {}: pod {} is not owned in plan lane",
                        dup.pod_id, p.name
                    ),
                )?;
            }
            Some(_) => match rp.delete_pod(&dup.pod_id) {
                Ok(()) | Err(crate::error::Error::Api { status: 404, .. }) => {
                    let jw = store.journal(
                        "watchdog_sweep_duplicate",
                        Some(plan.id),
                        &serde_json::json!({ "pod": dup.pod_id }),
                    )?;
                    let _ = store.journal_outcome(jw, "terminated");
                    store.journal_outcome(
                        dup.journal_id,
                        &format!("terminated duplicate pod {} on watchdog sweep", dup.pod_id),
                    )?;
                }
                Err(_) => {
                    all_resolved = false;
                }
            },
        }
    }
    Ok(all_resolved)
}

fn pod_already_terminated_in_journal(store: &Store, plan_id: i64, pod_id: &str) -> bool {
    store
        .journal_for_plan(plan_id)
        .unwrap_or_default()
        .into_iter()
        .any(|(action, intent, outcome)| {
            matches!(
                action.as_str(),
                "terminate_pod" | "watchdog_terminate" | "terminate_duplicate_pod"
            ) && intent.contains(pod_id)
                && outcome.as_deref().is_some_and(|o| {
                    !o.starts_with("delete_failed:") && !o.starts_with("terminate failed")
                })
        })
}

fn terminate_closed_plan_create_pod(
    store: &Store,
    rp: &RunPod,
    plan: &Plan,
    entry_id: i64,
    pod_id: &str,
    via: Option<&str>,
) -> Result<Verdict> {
    let reason = if via.is_some() {
        format!(
            "plan is {}; terminating pod from in-flight create_pod",
            plan.state
        )
    } else {
        format!(
            "plan is {}; terminating marked candidate after launcher died",
            plan.state
        )
    };
    let j = store.journal(
        "watchdog_terminate",
        Some(plan.id),
        &serde_json::json!({ "pod": pod_id, "reason": reason }),
    )?;
    match rp.delete_pod(pod_id) {
        Ok(()) | Err(crate::error::Error::Api { status: 404, .. }) => {
            store.journal_outcome(j, "terminated")?;
            let create_outcome = match via {
                Some(how) => {
                    format!(
                        "created pod {pod_id} (terminated by watchdog on closed plan via {how})"
                    )
                }
                None => format!("created pod {pod_id} (terminated by watchdog on closed plan)"),
            };
            store.journal_outcome(entry_id, &create_outcome)?;
            let _ = store.clear_setting(&unknown_pods_key(plan.id));
            Ok(Verdict::Terminate {
                reason: format!("plan is {}; terminated pod {pod_id}", plan.state),
            })
        }
        Err(e) => {
            store.journal_outcome(j, &format!("terminate failed: {e}"))?;
            Ok(Verdict::Wait)
        }
    }
}

fn unresolved_create_after_close(
    store: &Store,
    base_cfg: &crate::config::Config,
    rp: &RunPod,
    plan: &Plan,
    now: i64,
) -> Result<Verdict> {
    if let Some(entry) = store.unfinished_create_pod(plan.id)? {
        let Ok(pods) = rp.list_pods() else {
            return Ok(Verdict::Wait);
        };
        let lane_tag = store.plan_lane(plan.id)?;
        let cfg = cfg_in_plan_lane(base_cfg, lane_tag.as_deref());
        let expected_lane = cfg.lane_tag.as_deref().unwrap_or("plain");
        let journal_id = store.journaled_pod_id(plan.id)?;
        let owned: Vec<Pod> = pods.into_iter().filter(|p| cfg.owns_pod(&p.name)).collect();
        let open = store.open_plans()?;
        let taken: Vec<&str> = open
            .iter()
            .filter(|p| p.id != plan.id)
            .filter_map(|p| p.pod_id.as_deref())
            .collect();
        // Closed plans match ONLY by this plan's own identity (journaled pod id or
        // OFFRIG_PLAN == plan.id in its lane), never by pod name alone.
        if let Some((found, how)) =
            match_plan_pod(plan, &cfg, journal_id.as_deref(), &taken, &owned, false)
        {
            return terminate_closed_plan_create_pod(
                store,
                rp,
                plan,
                entry.id,
                &found.id,
                Some(how.as_str()),
            );
        }
        let creator_job = serde_json::from_str::<serde_json::Value>(&entry.intent)
            .ok()
            .and_then(|v| v["job_id"].as_i64())
            .and_then(|jid| store.job(jid).ok().flatten())
            .or_else(|| store.job_for_plan(plan.id, "launch").ok().flatten());
        let proven_dead = creator_job
            .as_ref()
            .is_some_and(|j| crate::proc::launch_job_proven_dead(j, now));
        if proven_dead {
            let plan_str = plan.id.to_string();
            let candidates: Vec<&Pod> = owned
                .iter()
                .filter(|p| {
                    p.desired_status != "TERMINATED"
                        && !taken.contains(&p.id.as_str())
                        && p.created_at_unix()
                            .is_none_or(|ts| plan.committed_at.is_none_or(|ca| ts >= ca))
                })
                .collect();
            if let Some(marked) = candidates.iter().copied().find(|p| {
                journal_id.as_deref() == Some(p.id.as_str())
                    || (p.env.get(crate::spec::ENV_PLAN).map(String::as_str)
                        == Some(plan_str.as_str())
                        && p.env
                            .get(crate::spec::ENV_LANE)
                            .is_none_or(|l| l == expected_lane))
            }) {
                return terminate_closed_plan_create_pod(
                    store, rp, plan, entry.id, &marked.id, None,
                );
            }
            if candidates.is_empty() {
                store.journal_outcome(entry.id, ABANDONED_CREATE_UNKNOWN_OUTCOME)?;
            } else {
                let cand_list: Vec<UnknownPodCandidate> = candidates
                    .iter()
                    .map(|p| UnknownPodCandidate {
                        id: p.id.clone(),
                        name: p.name.clone(),
                    })
                    .collect();
                let cand_text: Vec<String> = cand_list
                    .iter()
                    .map(|c| format!("{} ({})", c.id, c.name))
                    .collect();
                let warn = UnknownCreateWarning {
                    plan_id: plan.id,
                    profile: plan.profile.clone(),
                    lane: lane_tag,
                    candidates: cand_list,
                };
                if let Ok(json_str) = serde_json::to_string(&warn) {
                    let _ = store.set_setting(&unknown_pods_key(plan.id), &json_str);
                }
                store.journal_outcome(
                    entry.id,
                    &format!(
                        "{ABANDONED_CREATE_UNKNOWN_OUTCOME}; candidate pod(s): {}",
                        cand_text.join(", ")
                    ),
                )?;
            }
        } else {
            return Ok(Verdict::Wait);
        }
    }
    if plan.pod_id.is_none()
        && let Some(jid) = store.journaled_pod_id(plan.id)?
        && !pod_already_terminated_in_journal(store, plan.id, &jid)
    {
        let Ok(pods) = rp.list_pods() else {
            return Ok(Verdict::Wait);
        };
        let open = store.open_plans()?;
        if open
            .iter()
            .any(|p| p.id != plan.id && p.pod_id.as_deref() == Some(jid.as_str()))
        {
            return Ok(Verdict::Exit);
        }
        let lane_tag = store.plan_lane(plan.id)?;
        let cfg = cfg_in_plan_lane(base_cfg, lane_tag.as_deref());
        let expected_name = expected_pod_name(cfg.lane_tag.as_deref(), &plan.profile);
        if let Some(p) = pods.iter().find(|p| {
            p.id == jid
                && p.desired_status != "TERMINATED"
                && cfg.owns_pod(&p.name)
                && p.name == expected_name
        }) {
            let j = store.journal(
                "watchdog_terminate",
                Some(plan.id),
                &serde_json::json!({
                    "pod": p.id,
                    "reason": format!("plan is {}; terminating journaled pod", plan.state)
                }),
            )?;
            match rp.delete_pod(&p.id) {
                Ok(()) | Err(crate::error::Error::Api { status: 404, .. }) => {
                    store.journal_outcome(j, "terminated")?;
                    return Ok(Verdict::Terminate {
                        reason: format!("plan is {}; terminated pod {}", plan.state, p.id),
                    });
                }
                Err(e) => {
                    store.journal_outcome(j, &format!("terminate failed: {e}"))?;
                    return Ok(Verdict::Wait);
                }
            }
        }
    }
    Ok(Verdict::Exit)
}

/// One watchdog step: look, decide, act, record. Returns what it decided.
pub fn step(store: &Store, rp: &RunPod, plan_id: i64) -> Result<Verdict> {
    let base_cfg = crate::config::Config::load().unwrap_or_default();
    step_with_config(store, &base_cfg, rp, plan_id)
}

/// One watchdog step using an explicit base [`crate::config::Config`] for lane ownership filtering.
pub fn step_with_config(
    store: &Store,
    base_cfg: &crate::config::Config,
    rp: &RunPod,
    plan_id: i64,
) -> Result<Verdict> {
    let now = now_unix();
    store.clear_setting(&error_key(plan_id))?;
    store.set_setting(&heartbeat_key(plan_id), &now.to_string())?;
    let Some(mut plan) = store.plan(plan_id)? else {
        return Ok(Verdict::Exit);
    };
    let sweep_clean = sweep_unresolved_duplicates(store, base_cfg, rp, &plan)?;
    if plan.state != "committed" {
        let v = unresolved_create_after_close(store, base_cfg, rp, &plan, now)?;
        return Ok(match v {
            Verdict::Exit if !sweep_clean => Verdict::Wait,
            Verdict::Terminate { .. } if !sweep_clean => Verdict::Wait,
            other => other,
        });
    }
    let seen = if plan.pod_id.is_some() {
        look(rp, plan.pod_id.as_deref())
    } else {
        match rp.list_pods() {
            Err(_) => PodSeen::Unknown,
            Ok(pods) => {
                let journal_id = store.journaled_pod_id(plan_id)?;
                let lane_tag = store.plan_lane(plan_id)?;
                let cfg = cfg_in_plan_lane(base_cfg, lane_tag.as_deref());
                let listing_omitted_env = pods.iter().all(|p| p.env.is_empty());
                let owned: Vec<Pod> = pods.into_iter().filter(|p| cfg.owns_pod(&p.name)).collect();
                let open = store.open_plans()?;
                let taken: Vec<&str> = open
                    .iter()
                    .filter(|p| p.id != plan_id)
                    .filter_map(|p| p.pod_id.as_deref())
                    .collect();
                if let Some((found, how)) = match_plan_pod(
                    &plan,
                    &cfg,
                    journal_id.as_deref(),
                    &taken,
                    &owned,
                    listing_omitted_env,
                ) {
                    let ja = store.journal(
                        "watchdog_attach_orphan",
                        Some(plan_id),
                        &serde_json::json!({
                            "pod": found.id,
                            "name": found.name,
                            "matched_by": how.as_str(),
                        }),
                    )?;
                    store.attach_pod(plan_id, &found.id)?;
                    if found.desired_status.eq_ignore_ascii_case("RUNNING")
                        && let Err(e) = store.set_plan_cost_per_hr(plan_id, found.cost_per_hr)
                    {
                        eprintln!("[offrig-watchdog] failed saving plan {plan_id} rate: {e}");
                    }
                    store.journal_outcome(
                        ja,
                        &format!("attached orphan pod {} ({})", found.id, how.as_str()),
                    )?;
                    plan = store.plan(plan_id)?.ok_or_else(|| {
                        crate::error::Error::Refused(format!("no plan {plan_id}"))
                    })?;
                    PodSeen::Present(Box::new(found.clone()))
                } else {
                    PodSeen::Absent
                }
            }
        }
    };
    if let PodSeen::Present(p) = &seen {
        if let Err(e) = store.set_plan_last_seen(plan_id, now) {
            eprintln!("[offrig-watchdog] failed saving plan {plan_id} last_seen: {e}");
        }
        if p.desired_status.eq_ignore_ascii_case("RUNNING")
            && let Err(e) = store.set_plan_cost_per_hr(plan_id, p.cost_per_hr)
        {
            eprintln!("[offrig-watchdog] failed saving plan {plan_id} rate: {e}");
        }
        if plan.deadline().is_some_and(|d| now >= d)
            && crate::spec::pod_is_stopped(&p.desired_status)
            && let Err(e) = store.set_plan_self_stop(plan_id, crate::spec::SELF_STOP_STOPPED_LINE)
        {
            eprintln!("[offrig-watchdog] failed saving plan {plan_id} self_stop: {e}");
        }
    }
    let verdict = decide(&plan, &seen, now);
    match &verdict {
        Verdict::Terminate { reason } => {
            let saved = store.plan_cost_per_hr(plan_id).ok().flatten();
            let rate = match &seen {
                PodSeen::Present(p) if p.cost_per_hr > 0.0 && p.cost_per_hr.is_finite() => {
                    saved.map_or(p.cost_per_hr, |s| s.max(p.cost_per_hr))
                }
                _ => saved.unwrap_or(plan.max_price_hr),
            };
            let j = store.journal(
                "watchdog_terminate",
                Some(plan_id),
                &serde_json::json!({ "pod": plan.pod_id, "reason": reason }),
            )?;
            if let Some(id) = plan.pod_id.as_deref() {
                rp.delete_pod(id)?;
            }
            store.journal_outcome(j, "terminated")?;
            store.close_plan(plan_id, spend(&plan, rate, now))?;
        }
        Verdict::Close { .. } => {
            // If the pod vanished or was self-deleted, use the persisted real rate
            // (falling back to the ceiling only if never recorded) and clamp at
            // max(min(now, deadline + SELF_STOP_MARGIN_SECS), last_seen) only when armed.
            let cost = if plan.pod_id.is_some() {
                let rate = store
                    .plan_cost_per_hr(plan_id)
                    .ok()
                    .flatten()
                    .unwrap_or(plan.max_price_hr);
                let self_stop = store.plan_self_stop(plan_id).ok().flatten();
                let last_seen = store.plan_last_seen(plan_id).ok().flatten();
                spend_when_gone(&plan, rate, now, self_stop.as_deref(), last_seen)
            } else {
                0.0
            };
            store.close_plan(plan_id, cost)?;
        }
        Verdict::Wait | Verdict::Exit => {}
    }
    if !sweep_clean {
        return Ok(Verdict::Wait);
    }
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(state: &str, pod: Option<&str>, committed: Option<i64>, hours: f64) -> Plan {
        Plan {
            id: 1,
            profile: "frontier".into(),
            gpu_count: 4,
            gpu_types: vec![],
            max_hours: hours,
            max_price_hr: 8.36,
            worst_case: 8.36 * hours,
            state: state.into(),
            pod_id: pod.map(String::from),
            created_at: 0,
            note: None,
            committed_at: committed,
            started_at: committed.map(|c| c + 60),
        }
    }

    fn pod() -> PodSeen {
        PodSeen::Present(Box::new(
            serde_json::from_str(r#"{"id":"p1","desiredStatus":"RUNNING","costPerHr":8.36}"#)
                .expect("pod"),
        ))
    }

    #[test]
    fn exits_once_the_plan_is_not_committed() {
        for st in ["planned", "closed", "cancelled"] {
            assert_eq!(
                decide(&plan(st, Some("p1"), Some(0), 1.0), &pod(), 1_000_000),
                Verdict::Exit
            );
        }
    }

    #[test]
    fn terminates_at_the_deadline_and_not_before() {
        let p = plan("committed", Some("p1"), Some(1000), 1.5);
        assert_eq!(decide(&p, &pod(), 1000 + 5399), Verdict::Wait);
        assert!(matches!(
            decide(&p, &pod(), 1000 + 5400),
            Verdict::Terminate { .. }
        ));
    }

    #[test]
    fn never_acts_on_an_unknown_lookup() {
        let p = plan("committed", Some("p1"), Some(0), 1.0);
        assert_eq!(
            decide(&p, &PodSeen::Unknown, 999_999),
            Verdict::Wait,
            "past deadline but RunPod did not answer"
        );
    }

    #[test]
    fn a_vanished_pod_closes_the_books() {
        let p = plan("committed", Some("p1"), Some(0), 1.0);
        assert!(matches!(
            decide(&p, &PodSeen::Absent, 10),
            Verdict::Close { .. }
        ));
    }

    #[test]
    fn no_pod_by_the_deadline_closes_without_spend() {
        let p = plan("committed", None, Some(0), 1.0);
        assert_eq!(
            decide(&p, &PodSeen::Absent, 100),
            Verdict::Wait,
            "still waiting for GPUs"
        );
        assert!(matches!(
            decide(&p, &PodSeen::Absent, 3600),
            Verdict::Close { .. }
        ));
    }

    #[test]
    fn spend_counts_from_the_pod_start() {
        let p = plan("committed", Some("p1"), Some(0), 2.0);
        // started_at = 60; one hour later at 8.36/hr
        assert!((spend(&p, 8.36, 3660) - 8.36).abs() < 1e-9);
        assert_eq!(spend(&p, 8.36, 0), 0.0, "never negative");
    }

    #[test]
    fn spend_when_gone_caps_at_deadline_plus_self_stop_margin() {
        // committed_at = 0, started_at = 60, max_hours = 0.0834 (~300s deadline) -> fire_at = 420.
        let p = plan("committed", Some("p1"), Some(0), 0.0834);
        assert_eq!(p.deadline(), Some(300));
        let armed = Some("offrig self-stop: armed fire_at=420 via python3");
        // Armed + late close books to deadline + 120s (360s from start = $0.15).
        assert_eq!(spend_when_gone(&p, 1.44, 1020, armed, None), 0.15);
        // Before fire_at still books only elapsed runtime.
        assert_eq!(spend_when_gone(&p, 1.44, 210, armed, None), 0.06);
        // last_seen later than fire_at wins (660s from start = $0.27).
        assert_eq!(spend_when_gone(&p, 1.44, 1020, armed, Some(720)), 0.27);
        // UNAVAILABLE, STOPPED NOT TERMINATED, or unknown books to `now` (960s from start = $0.39).
        assert_eq!(
            spend_when_gone(
                &p,
                1.44,
                1020,
                Some("offrig self-stop: UNAVAILABLE"),
                Some(120)
            ),
            0.39
        );
        assert_eq!(spend_when_gone(&p, 1.44, 1020, None, Some(120)), 0.39);
        assert_eq!(
            spend_when_gone(
                &p,
                1.44,
                1020,
                Some(crate::spec::SELF_STOP_STOPPED_LINE),
                Some(120)
            ),
            0.39
        );
    }

    #[test]
    fn heartbeats_show_whether_the_watchdog_lives() {
        let s = Store::open_in_memory().expect("store");
        assert!(!alive(&s, 1, 100, 180));
        s.set_setting(&heartbeat_key(1), "100").expect("set");
        assert!(alive(&s, 1, 250, 180));
        assert!(!alive(&s, 1, 400, 180));
        record_error(&s, 1, "missing API key").expect("record error");
        assert!(!alive(&s, 1, 250, 180), "recorded error overrides liveness");
        assert_eq!(
            failure(&s, 1).expect("read").as_deref(),
            Some("missing API key")
        );
    }

    #[test]
    fn unguarded_in_scope_reads_recorded_watchdog_failures_read_only() {
        let root = std::env::temp_dir().join(format!("offrig-wd-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proj = root.join("proj-a");
        let cfg = root.join("cfg");
        std::fs::create_dir_all(&proj).expect("proj");
        std::fs::create_dir_all(&cfg).expect("cfg");
        let s = Store::open(&proj.join(".offrig").join("offrig.db")).expect("store");
        s.set_budget_cap(50.0).expect("cap");
        let p = s
            .create_plan(crate::store::NewPlan {
                profile: "small".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 1.0,
                max_price_hr: 0.5,
                note: None,
            })
            .expect("plan");
        s.set_plan_lane(p.id, "proj-a").expect("lane");
        s.commit_plan(p.id).expect("commit");
        record_error(&s, p.id, "cannot guard without RUNPOD_API_KEY").expect("err");
        let reg = crate::lanes::Registry::at(&cfg);
        let scope = crate::account::Scope::beside(&proj, &reg);
        let alerts = unguarded_in_scope(&scope);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].plan_id, p.id);
        assert_eq!(alerts[0].lane.as_deref(), Some("proj-a"));
        assert!(alerts[0].error.contains("RUNPOD_API_KEY"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
