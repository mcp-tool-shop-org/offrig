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

/// Whether the plan's watchdog has reported in within `max_age_secs`.
pub fn alive(store: &Store, plan_id: i64, now: i64, max_age_secs: i64) -> bool {
    store
        .setting(&heartbeat_key(plan_id))
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .is_some_and(|t| now - t <= max_age_secs)
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
    let now = now_unix();
    store.set_setting(&heartbeat_key(plan_id), &now.to_string())?;
    let Some(plan) = store.plan(plan_id)? else {
        return Ok(Verdict::Exit);
    };
    if plan.state != "committed" {
        return Ok(Verdict::Exit);
    }
    let seen = look(rp, plan.pod_id.as_deref());
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
    }
}
