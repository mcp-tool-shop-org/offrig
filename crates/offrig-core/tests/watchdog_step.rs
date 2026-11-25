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
        serde_json::json!({
            "id": "p-exact-name-wrong-lane",
            "name": "offrig-alpha-frontier",
            "status": "RUNNING",
            "cost": 3.0,
            "env": {"OFFRIG_PLAN": pid, "OFFRIG_LANE": "beta"}
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
    assert_eq!(m.count("DELETE /pods/p-exact-name-wrong-lane"), 0);
    let closed = store.plan(plan.id).expect("read").expect("plan");
    assert_eq!(closed.pod_id, None);
}

#[test]
fn watchdog_stays_running_on_closed_plan_while_create_pod_is_unresolved() {
    use std::sync::{Arc, Mutex};

    let store = Store::open_in_memory().expect("store");
    let plan = committed(&store, 1.0, None);
    store.set_plan_lane(plan.id, "alpha").expect("lane");
    let now = offrig_core::cost::now_unix();
    let job_id = store
        .create_launch_job(
            plan.id,
            std::process::id(),
            offrig_core::proc::own_start_time().as_deref(),
            now,
        )
        .expect("job");
    let _jc = store
        .journal(
            "create_pod",
            Some(plan.id),
            &serde_json::json!({ "job_id": job_id }),
        )
        .expect("journal");
    // Plan is closed while create_pod still has no outcome and launcher is alive.
    store.close_plan(plan.id, 0.0).expect("close");
    assert_eq!(
        store
            .plans_with_unfinished_create_after_close()
            .expect("closing"),
        vec![(plan.id, _jc)]
    );

    let pods: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
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

    // While create_pod has no outcome and no pod exists yet, watchdog stays Wait (does not exit).
    assert_eq!(
        watchdog::step(&store, &rp(&m), plan.id).expect("step 1"),
        Verdict::Wait
    );

    // Once the pod appears on RunPod, watchdog terminates it and resolves the create_pod journal.
    pods.lock().expect("pods").push(serde_json::json!({
        "id": "p-late",
        "name": "offrig-alpha-frontier",
        "status": "RUNNING",
        "cost": 3.0,
        "env": { "OFFRIG_PLAN": plan.id.to_string(), "OFFRIG_LANE": "alpha" }
    }));
    let v2 = watchdog::step(&store, &rp(&m), plan.id).expect("step 2");
    assert!(
        matches!(&v2, Verdict::Terminate { reason } if reason.contains("p-late")),
        "{v2:?}"
    );
    assert_eq!(m.count("DELETE /pods/p-late"), 1);
    assert!(pods.lock().expect("pods").is_empty());
    assert!(
        store
            .plans_with_unfinished_create_after_close()
            .expect("cleared")
            .is_empty()
    );
    assert_eq!(
        store.plan(plan.id).expect("r").expect("p").pod_id,
        None,
        "pod must not attach to a closed plan"
    );

    // Next step exits cleanly.
    assert_eq!(
        watchdog::step(&store, &rp(&m), plan.id).expect("step 3"),
        Verdict::Exit
    );
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
fn duplicate_sweep_skips_pod_held_by_another_open_plan() {
    let store = Store::open_in_memory().expect("store");
    let closed_plan = committed(&store, 1.0, None);
    store.set_plan_lane(closed_plan.id, "alpha").expect("lane");
    let jd = store
        .journal(
            "terminate_duplicate_pod",
            Some(closed_plan.id),
            &serde_json::json!({ "pod": "p-shared", "reason": "duplicate" }),
        )
        .expect("dup journal");
    store
        .journal_outcome(jd, "delete_failed: simulated 500")
        .expect("fail outcome");
    store.close_plan(closed_plan.id, 0.0).expect("close");

    let other_open = committed(&store, 1.0, None);
    store.set_plan_lane(other_open.id, "beta").expect("lane");
    store
        .attach_pod(other_open.id, "p-shared")
        .expect("attach shared pod to other open plan");

    let list_json = serde_json::Value::Array(vec![serde_json::json!({
        "id": "p-shared",
        "name": "offrig-alpha-frontier",
        "status": "RUNNING",
        "cost": 3.0
    })])
    .to_string();
    let m = serve(move |req, _| match req.route().as_str() {
        "GET /pods" => (200, list_json.clone()),
        _ => (404, "{}".to_string()),
    });

    assert_eq!(
        watchdog::step(&store, &rp(&m), closed_plan.id).expect("step"),
        Verdict::Exit
    );
    assert_eq!(
        m.count("DELETE /pods/p-shared"),
        0,
        "sweep must never delete a pod held by another open plan"
    );
    assert!(
        store
            .unresolved_duplicate_pods_for_plan(closed_plan.id)
            .expect("dups")
            .is_empty()
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

fn make_pod(id: &str, name: &str, status: &str, plan_id: Option<i64>) -> offrig_core::runpod::Pod {
    let mut env = std::collections::BTreeMap::new();
    if let Some(pid) = plan_id {
        env.insert("OFFRIG_PLAN".into(), pid.to_string());
    }
    offrig_core::runpod::Pod {
        id: id.into(),
        name: name.into(),
        desired_status: status.into(),
        image: String::new(),
        public_ip: None,
        port_mappings: None,
        ports: Vec::new(),
        cost_per_hr: 1.0,
        gpu_count: 1,
        last_started_at: None,
        created_at: None,
        network_volume_id: None,
        machine: None,
        env,
    }
}

#[test]
fn relaunch_snapshot_gate_covers_unknown_create_and_need_probe_branches() {
    use offrig_core::config::Config;
    use offrig_core::proc::{LAUNCH_HEARTBEAT_STALE_SECS, LivenessSnapshot};
    use offrig_core::watchdog::{
        FAILED_CREATE_UNKNOWN_STEP, RelaunchBlock, RelaunchGate, check_relaunch_allowed,
        check_relaunch_allowed_with_snapshot, collect_relaunch_probe_targets,
    };

    let store = Store::open_in_memory().expect("store");
    let cfg = Config::default();
    let plan = committed(&store, 1.0, None);
    let now = 2_000_000_000i64;
    let stale_hb = now - LAUNCH_HEARTBEAT_STALE_SECS - 30;

    // Stale running job + unfinished create_pod with empty snapshot -> NeedProbe((pid, start)).
    let j1 = store
        .create_launch_job(plan.id, 999_998, Some("orig-start"), stale_hb)
        .expect("job1");
    let jc = store
        .journal(
            "create_pod",
            Some(plan.id),
            &serde_json::json!({ "job_id": j1 }),
        )
        .expect("jc");
    let (targets, unknown) = collect_relaunch_probe_targets(&store, plan.id).expect("targets");
    assert!(!unknown);
    assert_eq!(targets, vec![(999_998, Some("orig-start".to_string()))]);
    let empty_snap = LivenessSnapshot::default();
    assert!(matches!(
        check_relaunch_allowed_with_snapshot(&store, &cfg, plan.id, now, &empty_snap, None),
        Err(RelaunchBlock::NeedProbe((999_998, Some(ref s)))) if s == "orig-start"
    ));

    // Close create_pod, leave running launch job -> live current PID blocks; dead PID allows.
    store.journal_outcome(jc, "abandoned").expect("close jc");
    let j_self = store
        .create_launch_job(plan.id, std::process::id(), None, now)
        .expect("j_self");
    assert!(matches!(
        check_relaunch_allowed_with_snapshot(&store, &cfg, plan.id, now, &empty_snap, None),
        Err(RelaunchBlock::Refused(_))
    ));
    let _ = store.update_job(
        j_self,
        "running",
        &serde_json::json!({ "pid": 999_998u32, "heartbeat_at": stale_hb }),
        None,
    );
    assert!(matches!(
        check_relaunch_allowed_with_snapshot(&store, &cfg, plan.id, now, &empty_snap, None),
        Err(RelaunchBlock::NeedProbe((999_998, None)))
    ));

    // failed_create_unknown requires list_pods search (`searched_pods`), refuses EXITED pods,
    // and returns AdoptExisting for a live matching pod.
    let _ = store.journal(
        "create_pod",
        Some(plan.id),
        &serde_json::json!({ "job_id": j_self }),
    );
    let _ = store.update_job(
        j_self,
        "failed",
        &serde_json::json!({ "step": FAILED_CREATE_UNKNOWN_STEP, "create_unknown": true }),
        Some("failed_create_unknown"),
    );
    assert!(matches!(
        check_relaunch_allowed_with_snapshot(&store, &cfg, plan.id, now, &empty_snap, None),
        Err(RelaunchBlock::Refused(_))
    ));
    let exited_pod = make_pod("pod-ex", "offrig-frontier", "EXITED", Some(plan.id));
    assert!(matches!(
        check_relaunch_allowed_with_snapshot(
            &store,
            &cfg,
            plan.id,
            now,
            &empty_snap,
            Some(&[exited_pod])
        ),
        Err(RelaunchBlock::Refused(_))
    ));
    let live_pod = make_pod("pod-live", "offrig-frontier", "RUNNING", Some(plan.id));
    assert_eq!(
        check_relaunch_allowed_with_snapshot(
            &store,
            &cfg,
            plan.id,
            now,
            &empty_snap,
            Some(std::slice::from_ref(&live_pod)),
        )
        .expect("adopt gate"),
        RelaunchGate::AdoptExisting("pod-live".into())
    );
    let err = check_relaunch_allowed(&store, plan.id, now, Some(&[live_pod]))
        .expect_err("convenience wrapper refuses raw relaunch when adoptable pod exists");
    assert!(err.to_string().contains("adopt that pod"), "{err}");
}

#[test]
fn duplicate_sweep_and_closed_plan_journal_branches() {
    let store = Store::open_in_memory().expect("store");
    let closed_a = committed(&store, 1.0, None);
    store.set_plan_lane(closed_a.id, "alpha").expect("lane a");
    let d1 = store
        .journal(
            "terminate_duplicate_pod",
            Some(closed_a.id),
            &serde_json::json!({ "pod": "p-gone", "reason": "dup" }),
        )
        .expect("d1");
    let d2 = store
        .journal(
            "terminate_duplicate_pod",
            Some(closed_a.id),
            &serde_json::json!({ "pod": "p-term", "reason": "dup" }),
        )
        .expect("d2");
    let d3 = store
        .journal(
            "terminate_duplicate_pod",
            Some(closed_a.id),
            &serde_json::json!({ "pod": "p-other", "reason": "dup" }),
        )
        .expect("d3");
    let d4 = store
        .journal(
            "terminate_duplicate_pod",
            Some(closed_a.id),
            &serde_json::json!({ "pod": "p-retry", "reason": "dup" }),
        )
        .expect("d4");
    for d in [d1, d2, d3, d4] {
        store.journal_outcome(d, "delete_failed: err").expect("df");
    }
    let jc_settled = store
        .journal(
            "create_pod",
            Some(closed_a.id),
            &serde_json::json!({ "job_id": 1 }),
        )
        .expect("jc_settled");
    store
        .journal_outcome(jc_settled, "created pod p-settled")
        .expect("outcome");
    store.close_plan(closed_a.id, 0.0).expect("close a");

    let list_json = serde_json::Value::Array(vec![
        serde_json::json!({ "id": "p-term", "name": "offrig-alpha-frontier", "status": "TERMINATED", "cost": 3.0 }),
        serde_json::json!({ "id": "p-other", "name": "foreign-pod", "status": "RUNNING", "cost": 3.0 }),
        serde_json::json!({ "id": "p-retry", "name": "offrig-alpha-frontier", "status": "RUNNING", "cost": 3.0 }),
        serde_json::json!({ "id": "p-settled", "name": "offrig-alpha-frontier", "status": "RUNNING", "cost": 3.0 }),
    ])
    .to_string();
    let m_500 = serve(move |req, _| match req.route().as_str() {
        "GET /pods" => (200, list_json.clone()),
        "DELETE /pods/p-retry" => (500, "err".to_string()),
        "DELETE /pods/p-settled" => (500, "err".to_string()),
        _ => (404, "{}".to_string()),
    });
    assert_eq!(
        watchdog::step(&store, &rp(&m_500), closed_a.id).expect("step 500"),
        Verdict::Wait
    );
    assert_eq!(m_500.count("DELETE /pods/p-retry"), 1);
    assert_eq!(m_500.count("DELETE /pods/p-settled"), 1);

    // Retry succeeds and cleans up remaining duplicate + settled-journal pod.
    let list_json2 = serde_json::Value::Array(vec![
        serde_json::json!({ "id": "p-retry", "name": "offrig-alpha-frontier", "status": "RUNNING", "cost": 3.0 }),
        serde_json::json!({ "id": "p-settled", "name": "offrig-alpha-frontier", "status": "RUNNING", "cost": 3.0 }),
    ])
    .to_string();
    let m_200 = serve(move |req, _| match req.route().as_str() {
        "GET /pods" => (200, list_json2.clone()),
        "DELETE /pods/p-retry" => (200, "{}".to_string()),
        "DELETE /pods/p-settled" => (200, "{}".to_string()),
        _ => (404, "{}".to_string()),
    });
    let v = watchdog::step(&store, &rp(&m_200), closed_a.id).expect("step 200");
    assert!(matches!(v, Verdict::Terminate { .. }), "{v:?}");
    assert!(
        store
            .unresolved_duplicate_pods_for_plan(closed_a.id)
            .expect("dups")
            .is_empty()
    );

    // Closed plan with unresolved create_pod: list_pods 500 -> Wait; dead launcher & no candidates -> ABANDONED_CREATE_UNKNOWN_OUTCOME + Exit.
    let closed_b = committed(&store, 1.0, None);
    let now = offrig_core::cost::now_unix();
    let stale = now - offrig_core::proc::LAUNCH_HEARTBEAT_STALE_SECS - 30;
    let jb = store
        .create_launch_job(closed_b.id, 999_997, None, stale)
        .expect("jb");
    let jcb = store
        .journal(
            "create_pod",
            Some(closed_b.id),
            &serde_json::json!({ "job_id": jb }),
        )
        .expect("jcb");
    store.close_plan(closed_b.id, 0.0).expect("close b");

    let m_err = serve(|_, _| (500, "down".to_string()));
    assert_eq!(
        watchdog::step(&store, &rp(&m_err), closed_b.id).expect("err step"),
        Verdict::Wait
    );
    let m_empty = serve(|req, _| match req.route().as_str() {
        "GET /pods" => (200, "[]".to_string()),
        _ => (404, "{}".to_string()),
    });
    assert_eq!(
        watchdog::step(&store, &rp(&m_empty), closed_b.id).expect("empty step"),
        Verdict::Exit
    );
    let outcomes = store.journal_for_plan(closed_b.id).expect("jb outcomes");
    assert!(
        outcomes.iter().any(|(a, _, o)| a == "create_pod"
            && o.as_deref() == Some(watchdog::ABANDONED_CREATE_UNKNOWN_OUTCOME)),
        "{outcomes:?}"
    );
    let _ = jcb;
}
