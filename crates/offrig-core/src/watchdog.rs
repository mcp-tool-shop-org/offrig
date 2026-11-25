//! The plan watchdog: a separate process that holds a committed plan to its deadline
//! whether or not the agent, the side-car or this machine's user is still around.
//!
//! Evidence (docs/sidecar-design.md): provider budgets are alerts that lag hours, and
//! agents that lose track of resources keep billing. So the deadline is enforced from
//! the plan record by its own process, and the decision is a pure function so every
//! rule is tested.

use crate::cost::now_unix;
use crate::error::Result;
use crate::runpod::{Pod, RunPod};
use crate::store::{Plan, Store};

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
        (Some(id), PodSeen::Present(_)) if past => Verdict::Terminate {
            reason: format!("plan deadline reached; terminating pod {id}"),
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
    if plan.state != "committed" {
        return Ok(Verdict::Exit);
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
    let verdict = decide(&plan, &seen, now);
    match &verdict {
        Verdict::Terminate { reason } => {
            let rate = match &seen {
                PodSeen::Present(p) => p.cost_per_hr,
                _ => plan.max_price_hr,
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
            // The pod's last rate is unknown once it is gone; charge the plan's
            // ceiling so the books never understate spend.
            let cost = if plan.pod_id.is_some() {
                spend(&plan, plan.max_price_hr, now)
            } else {
                0.0
            };
            store.close_plan(plan_id, cost)?;
        }
        Verdict::Wait | Verdict::Exit => {}
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
