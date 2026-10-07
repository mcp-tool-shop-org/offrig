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
    let m = serve(|_, _| (404, "{}".to_string()));
    let store = Store::open_in_memory().expect("store");
    let plan = committed(&store, 0.0001, None);
    let verdict = watchdog::step(&store, &rp(&m), plan.id).expect("step");
    assert!(
        matches!(&verdict, Verdict::Close { reason } if reason.contains("before any pod")),
        "{verdict:?}"
    );
    assert!(
        m.requests().is_empty(),
        "no pod id means nothing to look up"
    );
    let b = store.budget().expect("budget");
    assert!(b.spent.abs() < 1e-9 && b.committed.abs() < 1e-9, "{b:?}");
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
