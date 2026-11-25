//! One watchdog step against a mock RunPod and an in-memory store: what it looks at,
//! what it does about it, and what it writes down.

mod support;

use offrig_core::runpod::RunPod;
use offrig_core::store::{NewPlan, Plan, Store};
use offrig_core::watchdog::{self, Verdict};
use support::{Mock, serve};

fn rp(m: &Mock) -> RunPod {
    RunPod::new("k", &m.url, &format!("{}/graphql", m.url))
}

/// A committed plan; `hours` of 0.0001 puts its deadline at the moment of commit.
fn committed(store: &Store, hours: f64, pod: Option<&str>) -> Plan {
    store.set_budget_cap(100.0).expect("cap");
    let p = store
        .create_plan(NewPlan {
            profile: "frontier".into(),
            gpu_count: 1,
            gpu_types: vec!["NVIDIA H200".into()],
            max_hours: hours,
            max_price_hr: 3.0,
            note: None,
        })
        .expect("plan");
    store.commit_plan(p.id).expect("commit");
    if let Some(pod) = pod {
        store.attach_pod(p.id, pod).expect("attach");
    }
    store.plan(p.id).expect("read").expect("exists")
}

fn pods_json(status: &str) -> String {
    format!(
        r#"[{{"id":"p1","name":"offrig-frontier","desiredStatus":"{status}","costPerHr":3.0}}]"#
    )
}

#[test]
fn a_pod_past_the_deadline_is_terminated_and_the_books_closed() {
    let m = serve(|req, _| match req.route().as_str() {
        "GET /pods" => (200, pods_json("RUNNING")),
        "DELETE /pods/p1" => (200, "{}".to_string()),
        _ => (404, "{}".to_string()),
    });
    let store = Store::open_in_memory().expect("store");
    let plan = committed(&store, 0.0001, Some("p1"));
    let verdict = watchdog::step(&store, &rp(&m), plan.id).expect("step");
    assert!(
        matches!(&verdict, Verdict::Terminate { reason } if reason.contains("p1")),
        "{verdict:?}"
    );
    assert_eq!(m.count("DELETE /pods/p1"), 1);
    assert_eq!(
        store.plan(plan.id).expect("read").expect("exists").state,
        "closed"
    );
    let b = store.budget().expect("budget");
    assert!(b.committed.abs() < 1e-9, "the commit was released: {b:?}");
    assert!(b.spent < 0.05, "a second or so of a $3/hr pod: {b:?}");
    assert!(watchdog::alive(
        &store,
        plan.id,
        offrig_core::cost::now_unix(),
        60
    ));
    // A closed plan is not the watchdog's business any more.
    let again = watchdog::step(&store, &rp(&m), plan.id).expect("step");
    assert_eq!(again, Verdict::Exit);
    assert_eq!(m.count("DELETE /pods/p1"), 1);
}

#[test]
fn a_pod_inside_the_deadline_is_left_alone() {
    let m = serve(|_, _| (200, pods_json("RUNNING")));
    let store = Store::open_in_memory().expect("store");
    let plan = committed(&store, 2.0, Some("p1"));
    assert_eq!(
        watchdog::step(&store, &rp(&m), plan.id).expect("step"),
        Verdict::Wait
    );
    assert_eq!(m.count("DELETE /pods/p1"), 0);
    assert_eq!(
        store.plan(plan.id).expect("read").expect("exists").state,
        "committed"
    );
}

#[test]
fn a_failed_lookup_never_triggers_a_termination() {
    let m = serve(|_, _| (500, "down".to_string()));
    let store = Store::open_in_memory().expect("store");
    let plan = committed(&store, 0.0001, Some("p1"));
    assert_eq!(
        watchdog::step(&store, &rp(&m), plan.id).expect("step"),
        Verdict::Wait
    );
    assert_eq!(m.count("DELETE /pods/p1"), 0);
    assert_eq!(
        store.plan(plan.id).expect("read").expect("exists").state,
        "committed"
    );
}

#[test]
fn a_vanished_or_terminated_pod_closes_the_plan_at_its_ceiling_rate() {
    for listing in ["[]".to_string(), pods_json("TERMINATED")] {
        let m = serve(move |_, _| (200, listing.clone()));
        let store = Store::open_in_memory().expect("store");
        let plan = committed(&store, 2.0, Some("p1"));
        let verdict = watchdog::step(&store, &rp(&m), plan.id).expect("step");
        assert!(
            matches!(&verdict, Verdict::Close { reason } if reason.contains("no longer exists")),
            "{verdict:?}"
        );
        assert_eq!(
            store.plan(plan.id).expect("read").expect("exists").state,
            "closed"
        );
    }
}

#[test]
fn a_plan_that_never_rented_closes_free_once_its_time_is_up() {
    let m = serve(|req, _| match req.route().as_str() {
        "GET /pods" => (200, "[]".to_string()),
        _ => (404, "{}".to_string()),
    });
    let store = Store::open_in_memory().expect("store");
    let plan = committed(&store, 0.0001, None);
    let verdict = watchdog::step(&store, &rp(&m), plan.id).expect("step");
    assert!(
        matches!(&verdict, Verdict::Close { reason } if reason.contains("before any pod")),
        "{verdict:?}"
    );
    assert_eq!(
        m.count("GET /pods"),
        1,
        "probes RunPod once for unattached pods before closing"
    );
    let b = store.budget().expect("budget");
    assert!(b.spent.abs() < 1e-9 && b.committed.abs() < 1e-9, "{b:?}");
}

#[test]
fn a_failed_lookup_on_a_podless_plan_never_closes_or_acts() {
    let m = serve(|_, _| (500, "down".to_string()));
    let store = Store::open_in_memory().expect("store");
    let plan = committed(&store, 0.0001, None);
    assert_eq!(
        watchdog::step(&store, &rp(&m), plan.id).expect("step"),
        Verdict::Wait
    );
    assert_eq!(
        store.plan(plan.id).expect("read").expect("exists").state,
        "committed"
    );
}

#[test]
fn an_orphan_pod_created_before_attach_is_found_and_terminated_at_the_deadline() {
    use std::sync::{Arc, Mutex};

    for mode in ["journal", "name", "env_plan"] {
        let store = Store::open_in_memory().expect("store");
        // Plan starts inside its deadline, then another plan uses a past deadline.
        let plan = committed(&store, 2.0, None);
        store.set_plan_lane(plan.id, "alpha").expect("lane");

        let pods: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(vec![
            // Foreign / other-lane pods must never be matched.
            serde_json::json!({
                "id": "p-foreign",
                "name": "unrelated-pod",
                "desiredStatus": "RUNNING",
                "costPerHr": 1.0
            }),
            serde_json::json!({
                "id": "p-other-lane",
                "name": "offrig-beta-frontier",
                "desiredStatus": "RUNNING",
                "costPerHr": 3.0
            }),
        ]));
        let orphan = match mode {
            "journal" => {
                let j = store
                    .journal("create_pod", Some(plan.id), &serde_json::json!({}))
                    .expect("journal");
                store
                    .journal_outcome(j, "created pod p-orphan-j")
                    .expect("outcome");
                serde_json::json!({
                    "id": "p-orphan-j",
                    "name": "offrig-alpha-frontier",
                    "desiredStatus": "RUNNING",
                    "costPerHr": 3.0
                })
            }
            "name" => serde_json::json!({
                "id": "p-orphan-n",
                "name": "offrig-alpha-frontier",
                "desiredStatus": "RUNNING",
                "costPerHr": 3.0
            }),
            _ => serde_json::json!({
                "id": "p-orphan-e",
                "name": "offrig-alpha-frontier",
                "status": "RUNNING",
                "cost": 3.0,
                "env": {
                    "OFFRIG_PLAN": plan.id.to_string(),
                    "OFFRIG_LANE": "alpha"
                }
            }),
        };
        let orphan_id = orphan["id"].as_str().expect("id").to_string();
        pods.lock().expect("pods").push(orphan);

        let pods_srv = Arc::clone(&pods);
        let m = serve(move |req, _| {
            let mut list = pods_srv.lock().expect("pods");
            let route = req.route();
            if route == "GET /pods" {
                (200, serde_json::Value::Array(list.clone()).to_string())
            } else if let Some(id) = route.strip_prefix("DELETE /pods/") {
                list.retain(|p| p["id"] != id);
                (204, String::new())
            } else {
                (404, "{}".to_string())
            }
        });

        // Before deadline: watchdog attaches the orphan and waits.
        let before = watchdog::step(&store, &rp(&m), plan.id).expect("step before");
        assert_eq!(before, Verdict::Wait, "{mode}");
        let attached = store.plan(plan.id).expect("read").expect("plan");
        assert_eq!(
            attached.pod_id.as_deref(),
            Some(orphan_id.as_str()),
            "{mode}"
        );

        // Now test a pod-less plan already past its deadline: attaches AND terminates in one step.
        let expired = committed(&store, 0.0001, None);
        store.set_plan_lane(expired.id, "alpha").expect("lane");
        let expired_orphan = match mode {
            "journal" => {
                let j = store
                    .journal("create_pod", Some(expired.id), &serde_json::json!({}))
                    .expect("journal");
                store
                    .journal_outcome(j, "created pod p-exp-j")
                    .expect("outcome");
                serde_json::json!({
                    "id": "p-exp-j",
                    "name": "offrig-alpha-frontier",
                    "desiredStatus": "RUNNING",
                    "costPerHr": 3.0
                })
            }
            "name" => serde_json::json!({
                "id": "p-exp-n",
                "name": "offrig-alpha-frontier",
                "desiredStatus": "RUNNING",
                "costPerHr": 3.0
            }),
            _ => serde_json::json!({
                "id": "p-exp-e",
                "name": "offrig-alpha-frontier",
                "status": "RUNNING",
                "cost": 3.0,
                "env": {
                    "OFFRIG_PLAN": expired.id.to_string(),
                    "OFFRIG_LANE": "alpha"
                }
            }),
        };
        let exp_id = expired_orphan["id"].as_str().expect("id").to_string();
        pods.lock().expect("pods").push(expired_orphan);

        let verdict = watchdog::step(&store, &rp(&m), expired.id).expect("step expired");
        assert!(
            matches!(&verdict, Verdict::Terminate { reason } if reason.contains(&exp_id)),
            "{mode}: {verdict:?}"
        );
        assert_eq!(m.count(&format!("DELETE /pods/{exp_id}")), 1, "{mode}");
        assert!(
            !pods.lock().expect("pods").iter().any(|p| p["id"] == exp_id),
            "{mode}: orphan removed from RunPod"
        );
        assert_eq!(
            store.plan(expired.id).expect("read").expect("plan").state,
            "closed"
        );
        let j_entries = store.journal_for_plan(expired.id).expect("journal");
        assert!(
            j_entries
                .iter()
                .any(|(a, _, o)| a == "watchdog_attach_orphan" && o.is_some()),
            "{mode}: {j_entries:?}"
        );
        assert!(
            j_entries
                .iter()
                .any(|(a, _, o)| a == "watchdog_terminate" && o.as_deref() == Some("terminated")),
            "{mode}: {j_entries:?}"
        );
    }
}

#[test]
fn env_plan_marker_in_another_lane_never_matches() {
    let store = Store::open_in_memory().expect("store");
    let plan = committed(&store, 0.0001, None);
    store.set_plan_lane(plan.id, "alpha").expect("lane");

    let cfg = watchdog::cfg_in_plan_lane(&offrig_core::config::Config::default(), Some("alpha"));
    let pid = plan.id.to_string();
    let raw_pods = vec![
        serde_json::json!({
            "id": "p-beta",
            "name": "offrig-beta-frontier",
            "status": "RUNNING",
            "cost": 3.0,
            "env": {"OFFRIG_PLAN": pid, "OFFRIG_LANE": "beta"}
        }),
        serde_json::json!({
            "id": "p-plain",
            "name": "offrig-frontier",
            "status": "RUNNING",
            "cost": 3.0,
            "env": {"OFFRIG_PLAN": pid, "OFFRIG_LANE": "plain"}
        }),
        serde_json::json!({
            "id": "p-empty",
            "name": "",
            "status": "RUNNING",
            "cost": 3.0,
            "env": {"OFFRIG_PLAN": pid, "OFFRIG_LANE": "alpha"}
        }),
        serde_json::json!({
            "id": "p-wrong-profile",
            "name": "offrig-alpha-jam",
            "status": "RUNNING",
            "cost": 3.0,
            "env": {"OFFRIG_PLAN": pid, "OFFRIG_LANE": "alpha"}
        }),
    ];
    let parsed: Vec<offrig_core::runpod::Pod> = raw_pods
        .iter()
        .map(|v| serde_json::from_value(v.clone()).expect("pod"))
        .collect();
    assert_eq!(
        watchdog::match_plan_pod(&plan, &cfg, None, &[], &parsed, false),
        None,
        "OFFRIG_PLAN on another lane or mismatched pod name must never match"
    );

    let list_body = serde_json::Value::Array(raw_pods).to_string();
    let m = serve(move |req, _| match req.route().as_str() {
        "GET /pods" => (200, list_body.clone()),
        _ => (404, "{}".to_string()),
    });
    let verdict = watchdog::step(&store, &rp(&m), plan.id).expect("step");
    assert!(
        matches!(&verdict, Verdict::Close { reason } if reason.contains("before any pod")),
        "{verdict:?}"
    );
    assert_eq!(m.count("DELETE /pods/p-beta"), 0);
    assert_eq!(m.count("DELETE /pods/p-plain"), 0);
    assert_eq!(m.count("DELETE /pods/p-empty"), 0);
    assert_eq!(m.count("DELETE /pods/p-wrong-profile"), 0);
    let closed = store.plan(plan.id).expect("read").expect("plan");
    assert_eq!(closed.pod_id, None);
}

#[test]
fn plain_lane_plan_and_env_present_listing_never_adopt_by_name_alone() {
    let store = Store::open_in_memory().expect("store");
    let plain_plan = committed(&store, 2.0, None);
    let unmarked_plain = vec![serde_json::json!({
        "id": "p-plain-unmarked",
        "name": "offrig-frontier",
        "desiredStatus": "RUNNING",
        "costPerHr": 3.0
    })];
    let plain_list = serde_json::Value::Array(unmarked_plain.clone()).to_string();
    let m_plain = serve(move |req, _| match req.route().as_str() {
        "GET /pods" => (200, plain_list.clone()),
        _ => (404, "{}".to_string()),
    });
    assert_eq!(
        watchdog::step(&store, &rp(&m_plain), plain_plan.id).expect("step"),
        Verdict::Wait
    );
    assert_eq!(
        store
            .plan(plain_plan.id)
            .expect("read")
            .expect("plan")
            .pod_id,
        None,
        "plain-lane plan must never adopt an unmarked offrig-<profile> pod"
    );

    // Even in a project lane, if any pod in the listing carries env, an unmarked pod
    // must not be adopted by name alone.
    let lane_plan = committed(&store, 2.0, None);
    store.set_plan_lane(lane_plan.id, "alpha").expect("lane");
    let mixed = vec![
        serde_json::json!({
            "id": "p-alpha-unmarked",
            "name": "offrig-alpha-frontier",
            "status": "RUNNING",
            "cost": 3.0
        }),
        serde_json::json!({
            "id": "p-beta-marked",
            "name": "offrig-beta-job",
            "status": "RUNNING",
            "cost": 0.5,
            "env": {"OFFRIG_PLAN": "999", "OFFRIG_LANE": "beta"}
        }),
    ];
    let mixed_list = serde_json::Value::Array(mixed).to_string();
    let m_mixed = serve(move |req, _| match req.route().as_str() {
        "GET /pods" => (200, mixed_list.clone()),
        _ => (404, "{}".to_string()),
    });
    assert_eq!(
        watchdog::step(&store, &rp(&m_mixed), lane_plan.id).expect("step"),
        Verdict::Wait
    );
    assert_eq!(
        store
            .plan(lane_plan.id)
            .expect("read")
            .expect("plan")
            .pod_id,
        None
    );
}

#[test]
fn an_unknown_plan_ends_the_watchdog() {
    let m = serve(|_, _| (404, "{}".to_string()));
    let store = Store::open_in_memory().expect("store");
    assert_eq!(
        watchdog::step(&store, &rp(&m), 4242).expect("step"),
        Verdict::Exit
    );
    assert!(m.requests().is_empty());
}
