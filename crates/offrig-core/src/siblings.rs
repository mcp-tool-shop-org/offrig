//! Other lanes' pods, made identifiable (issue #26).
//!
//! offrig names its pods `offrig-<tag>-<profile>` (a project lane) or `offrig-<profile>`
//! (the plain lane of the CLI, the app and Zed). The lane registry maps each tag to a
//! project, so a pod that is not this lane's can still be named: which lane, which
//! project, and, read from that project's own store, which plan holds it, what it is
//! for and when it ends.
//!
//! The read is strictly read-only (see [`Store::open_read_only`]): nothing in another
//! lane is created, migrated or changed, and a store that cannot be read degrades to a
//! note on that pod, never to a failed status.

use std::path::Path;

use serde_json::{Value, json};

use crate::config::Config;
use crate::cost::iso_utc;
use crate::lanes::Lane;
use crate::runpod::Pod;
use crate::store::{Plan, Store};

/// The lane label of the CLI, the app and Zed.
pub const PLAIN: &str = "plain";

/// A sibling project's open plan, as its own store records it.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenPlan {
    pub plan_id: i64,
    pub profile: String,
    pub note: Option<String>,
    /// When the plan's time is up (unix seconds), once its worst case is committed.
    pub deadline: Option<i64>,
    /// The worst case the plan committed against its budget, in USD.
    pub committed_worst_case: f64,
}

impl OpenPlan {
    fn of(p: &Plan) -> Self {
        OpenPlan {
            plan_id: p.id,
            profile: p.profile.clone(),
            note: p.note.clone(),
            deadline: p.deadline(),
            committed_worst_case: p.worst_case,
        }
    }
}

/// What reading a sibling's store showed.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanView {
    Open(OpenPlan),
    /// The store opened and holds no committed plan.
    NoOpenPlan,
    /// The plain lane belongs to no one project, so there is no store to read.
    NoProject,
    /// The store is missing, locked, corrupt or from a different offrig.
    Unreadable(String),
}

/// One pod in another lane.
#[derive(Debug, Clone, PartialEq)]
pub struct Sibling {
    /// The lane's tag, or `plain`.
    pub lane: String,
    /// The project path the registry maps the lane to; `None` for the plain lane.
    pub project: Option<String>,
    pub pod: Pod,
    pub plan: PlanView,
}

impl Sibling {
    /// One line for a terminal: the lane, its project, and what its plan says.
    pub fn summary(&self) -> String {
        let who = match &self.project {
            Some(p) => format!("lane {}, project {p}", self.lane),
            None => format!("lane {}", self.lane),
        };
        match &self.plan {
            PlanView::Open(p) => format!(
                "{who}: plan {}{}, ends {}, worst case ${:.2}",
                p.plan_id,
                p.note
                    .as_deref()
                    .map_or(String::new(), |n| format!(" ({n})")),
                p.deadline.map_or("at no set time".into(), iso_utc),
                p.committed_worst_case
            ),
            PlanView::NoOpenPlan => format!("{who}: its store has no open plan"),
            PlanView::NoProject => format!("{who}: belongs to no one project"),
            PlanView::Unreadable(why) => format!("{who}: its store could not be read ({why})"),
        }
    }

    pub fn to_json(&self) -> Value {
        let (plan, note) = match &self.plan {
            PlanView::Open(p) => (
                json!({
                    "plan_id": p.plan_id,
                    "profile": p.profile,
                    "note": p.note,
                    "deadline": p.deadline.map(iso_utc),
                    "committed_worst_case": (p.committed_worst_case * 100.0).round() / 100.0,
                }),
                Value::Null,
            ),
            PlanView::NoOpenPlan => (Value::Null, json!("that project's store has no open plan")),
            PlanView::NoProject => (
                Value::Null,
                json!("the plain lane belongs to no one project; there is no plan to read"),
            ),
            PlanView::Unreadable(why) => (
                Value::Null,
                json!(format!("that project's store could not be read: {why}")),
            ),
        };
        json!({
            "lane": self.lane,
            "project": self.project,
            "pod": self.pod.name,
            "pod_id": self.pod.id,
            "gpu": self.pod.gpu_type(),
            "cost_per_hr": self.pod.cost_per_hr,
            "status": self.pod.desired_status,
            "plan": plan,
            "plan_note": note,
        })
    }
}

/// The pods split three ways: this lane's are not here (the caller took them out).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Split {
    /// Pods named for another lane, with that lane's plan read from its store.
    pub siblings: Vec<Sibling>,
    /// Pods offrig did not create (or cannot place in a lane).
    pub foreign: Vec<Pod>,
}

/// The lane a pod name belongs to: the tag (or `plain`) and the project the registry
/// gives it. Exact names only: some configured profile's name in that lane.
pub fn lane_of(
    name: &str,
    base: &Config,
    lanes: &[(String, Lane)],
) -> Option<(String, Option<String>)> {
    for (project, lane) in lanes {
        let Some(tag) = lane.tag.as_deref() else {
            continue;
        };
        if base
            .profiles
            .iter()
            .any(|p| name == format!("offrig-{tag}-{}", p.name))
        {
            return Some((tag.to_string(), Some(project.clone())));
        }
    }
    base.profiles
        .iter()
        .any(|p| name == format!("offrig-{}", p.name))
        .then(|| (PLAIN.to_string(), None))
}

/// Read a project's open plan, strictly read-only. `pod_id` picks the plan that holds
/// that pod when the store has several open plans.
pub fn read_plan(project: &Path, pod_id: &str) -> PlanView {
    let db = project.join(".offrig").join("offrig.db");
    if !db.is_file() {
        return PlanView::Unreadable("no offrig store at that project".into());
    }
    let plans = Store::open_read_only(&db).and_then(|s| s.open_plans());
    match plans {
        Err(e) => PlanView::Unreadable(crate::error::chain(&e)),
        Ok(plans) => plans
            .iter()
            .find(|p| p.pod_id.as_deref() == Some(pod_id))
            .or_else(|| plans.first())
            .map_or(PlanView::NoOpenPlan, |p| PlanView::Open(OpenPlan::of(p))),
    }
}

/// Sort the pods that are not this lane's into sibling lanes' pods and foreign ones.
pub fn split(pods: Vec<Pod>, base: &Config, lanes: &[(String, Lane)]) -> Split {
    let mut out = Split::default();
    for pod in pods {
        match lane_of(&pod.name, base, lanes) {
            Some((lane, project)) => {
                let plan = match &project {
                    Some(p) => read_plan(Path::new(p), &pod.id),
                    None => PlanView::NoProject,
                };
                out.siblings.push(Sibling {
                    lane,
                    project,
                    pod,
                    plan,
                });
            }
            None => out.foreign.push(pod),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewPlan;

    fn lane(tag: &str) -> Lane {
        Lane {
            tag: Some(tag.into()),
            ssh_alias: format!("offrig-{tag}"),
            tunnel_port: 11500,
        }
    }

    fn pod(id: &str, name: &str) -> Pod {
        serde_json::from_value(
            json!({"id": id, "name": name, "desiredStatus": "RUNNING", "costPerHr": 1.5}),
        )
        .expect("pod")
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("offrig-sib-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("dir");
        d
    }

    fn plan(s: &Store, note: &str, pod_id: &str) -> i64 {
        let p = s
            .create_plan(NewPlan {
                profile: "job".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 2.0,
                max_price_hr: 2.0,
                note: Some(note.into()),
            })
            .expect("plan");
        s.commit_plan(p.id).expect("commit");
        s.attach_pod(p.id, pod_id).expect("attach");
        p.id
    }

    #[test]
    fn a_pod_name_is_placed_by_exact_lane_and_profile() {
        let base = Config::default();
        let lanes = vec![("/p/a".to_string(), lane("alpha"))];
        let of = |n: &str| lane_of(n, &base, &lanes);
        assert_eq!(
            of("offrig-alpha-job"),
            Some(("alpha".into(), Some("/p/a".into())))
        );
        assert_eq!(of("offrig-small"), Some((PLAIN.into(), None)));
        // Close to a lane's names, but not exactly one: not placed.
        assert_eq!(of("offrig-alpha-nope"), None);
        assert_eq!(of("offrig-beta-job"), None);
        assert_eq!(of("offrig-alpha"), None);
        assert_eq!(of("somebody-elses"), None);
        assert_eq!(of("offrig-stage-small"), None);
    }

    #[test]
    fn split_reads_each_siblings_plan_and_leaves_foreign_pods_as_pods() {
        let base = Config::default();
        let a = temp("a");
        let s = Store::open(&a.join(".offrig").join("offrig.db")).expect("store");
        s.set_budget_cap(100.0).expect("cap");
        let id = plan(&s, "render the trailer", "pa");
        drop(s);
        let b = temp("b");
        let lanes = vec![
            (a.to_string_lossy().into_owned(), lane("alpha")),
            (b.to_string_lossy().into_owned(), lane("beta")),
        ];
        let out = split(
            vec![
                pod("pa", "offrig-alpha-job"),
                pod("pb", "offrig-beta-job"),
                pod("pc", "offrig-small"),
                pod("pd", "other"),
            ],
            &base,
            &lanes,
        );
        assert_eq!(out.foreign.len(), 1);
        assert_eq!(out.foreign[0].name, "other");
        assert_eq!(out.siblings.len(), 3);
        let PlanView::Open(p) = &out.siblings[0].plan else {
            panic!("alpha's plan: {:?}", out.siblings[0].plan);
        };
        assert_eq!(
            (p.plan_id, p.note.as_deref()),
            (id, Some("render the trailer"))
        );
        assert_eq!(p.committed_worst_case, 4.0);
        assert!(p.deadline.is_some());
        let j = out.siblings[0].to_json();
        assert_eq!(j["lane"], "alpha");
        assert_eq!(j["plan"]["plan_id"], id);
        assert_eq!(j["plan"]["committed_worst_case"], 4.0);
        assert!(
            j["plan"]["deadline"]
                .as_str()
                .is_some_and(|d| d.ends_with('Z'))
        );
        assert!(matches!(out.siblings[1].plan, PlanView::Unreadable(_)));
        assert!(
            out.siblings[1].to_json()["plan_note"]
                .as_str()
                .is_some_and(|n| n.contains("could not be read"))
        );
        assert_eq!(out.siblings[2].plan, PlanView::NoProject);
        let line = out.siblings[0].summary();
        assert!(
            line.starts_with("lane alpha, project ")
                && line.contains(&format!("plan {id} (render the trailer), ends 20"))
                && line.ends_with("worst case $4.00"),
            "{line}"
        );
        assert!(out.siblings[1].summary().contains("could not be read"));
        assert_eq!(
            out.siblings[2].summary(),
            "lane plain: belongs to no one project"
        );
        assert_eq!(out.siblings[2].lane, PLAIN);
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn the_plan_holding_the_pod_wins_among_several_open_plans() {
        let dir = temp("several");
        let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
        s.set_budget_cap(100.0).expect("cap");
        let _first = plan(&s, "first", "p-first");
        let second = plan(&s, "second", "p-second");
        drop(s);
        let view = |pod: &str| read_plan(&dir, pod);
        let PlanView::Open(p) = view("p-second") else {
            panic!("open plan");
        };
        assert_eq!(p.plan_id, second);
        // A pod no plan holds falls back to the first open plan.
        let PlanView::Open(p) = view("unknown") else {
            panic!("open plan");
        };
        assert_eq!(p.note.as_deref(), Some("first"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_store_has_no_open_plan_and_a_closed_plan_is_not_open() {
        let dir = temp("empty");
        let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
        s.set_budget_cap(100.0).expect("cap");
        assert_eq!(read_plan(&dir, "x"), PlanView::NoOpenPlan);
        let id = plan(&s, "done", "x");
        s.close_plan(id, 0.0).expect("close");
        drop(s);
        assert_eq!(read_plan(&dir, "x"), PlanView::NoOpenPlan);
        assert!(
            Sibling {
                lane: "a".into(),
                project: Some("/p".into()),
                pod: pod("x", "offrig-a-job"),
                plan: PlanView::NoOpenPlan,
            }
            .to_json()["plan_note"]
                .as_str()
                .is_some_and(|n| n.contains("no open plan"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plan_without_a_deadline_or_note_still_has_a_line() {
        let s = Sibling {
            lane: "a".into(),
            project: Some("/p".into()),
            pod: pod("x", "offrig-a-job"),
            plan: PlanView::Open(OpenPlan {
                plan_id: 4,
                profile: "job".into(),
                note: None,
                deadline: None,
                committed_worst_case: 1.0,
            }),
        };
        assert_eq!(
            s.summary(),
            "lane a, project /p: plan 4, ends at no set time, worst case $1.00"
        );
        assert!(s.to_json()["plan"]["deadline"].is_null());
        let none = Sibling {
            plan: PlanView::NoOpenPlan,
            ..s
        };
        assert_eq!(
            none.summary(),
            "lane a, project /p: its store has no open plan"
        );
    }

    /// A v4 side-car reads a sibling's v3 store normally (a read-only view never
    /// migrates), and a store newer than this offrig knows is "unreadable", never an
    /// error: the same check is what makes a v3 side-car report a v4 sibling that way.
    #[test]
    fn an_older_sibling_store_reads_and_a_newer_one_is_unreadable() {
        let dir = temp("v3");
        let db = dir.join(".offrig").join("offrig.db");
        std::fs::create_dir_all(db.parent().expect("parent")).expect("dir");
        {
            let c = rusqlite::Connection::open(&db).expect("open");
            c.execute_batch(
                "CREATE TABLE plans (id INTEGER PRIMARY KEY, profile TEXT NOT NULL, gpu_count INTEGER NOT NULL,
                   gpu_types TEXT NOT NULL, max_hours REAL NOT NULL, max_price_hr REAL NOT NULL, worst_case REAL NOT NULL,
                   state TEXT NOT NULL DEFAULT 'planned', pod_id TEXT, created_at INTEGER NOT NULL, note TEXT,
                   committed_at INTEGER, started_at INTEGER);
                 CREATE TABLE ledger (id INTEGER PRIMARY KEY, plan_id INTEGER NOT NULL, kind TEXT NOT NULL,
                   amount REAL NOT NULL, at INTEGER NOT NULL, note TEXT);
                 INSERT INTO plans(profile, gpu_count, gpu_types, max_hours, max_price_hr, worst_case, state, pod_id,
                   created_at, note, committed_at)
                   VALUES ('job', 1, '[]', 2.0, 2.0, 4.0, 'committed', 'p3', 1, 'step 2', 1);
                 PRAGMA user_version = 3;",
            )
            .expect("v3 store");
        }
        match read_plan(&dir, "p3") {
            PlanView::Open(p) => assert_eq!(p.note.as_deref(), Some("step 2")),
            other => panic!("a v3 sibling should read: {other:?}"),
        }
        let still: i64 = rusqlite::Connection::open(&db)
            .expect("reopen")
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(still, 3, "reading a sibling never migrates it");

        rusqlite::Connection::open(&db)
            .expect("reopen")
            .execute_batch("PRAGMA user_version = 99;")
            .expect("future version");
        assert!(
            matches!(read_plan(&dir, "p3"), PlanView::Unreadable(ref m) if m.contains("v99")),
            "a newer sibling store is unreadable, not an error"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
