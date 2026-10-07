//! Phase 2 end to end against a mock RunPod: launch, idempotent relaunch, job
//! progress, ask without a ready session, shutdown, and the watchdog process
//! terminating a pod at its plan's deadline. The binaries are debug builds, which
//! honour OFFRIG_TEST_RUNPOD_BASE; release builds ignore it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

mod common;

use common::{count, mock, temp};
use offrig_core::store::{NewHandoff, NewPlan, Store};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

fn graphql(body: &str) -> String {
    if body.contains("myself") {
        return r#"{"data":{"myself":{"clientBalance":12.0,"currentSpendPerHr":0.0,"spendLimit":80}}}"#.into();
    }
    // Every small-tier GPU type, free at 0.25.
    r#"{"data":{"gpuTypes":[
        {"id":"NVIDIA RTX 2000 Ada Generation","displayName":"RTX 2000 Ada","memoryInGb":16,"secureCloud":true,
         "lowestPrice":{"uninterruptablePrice":0.25,"stockStatus":"High"}}]}}"#
        .into()
}

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

#[tokio::test]
async fn launch_job_ask_and_shutdown_against_a_mock_runpod() {
    let deleted = Arc::new(AtomicBool::new(false));
    let gone = Arc::clone(&deleted);
    // The pod the mock hands back carries the lane's name, once the plan has told us
    // the lane (a side-car never takes a pod that is not its lane's).
    let pod_name = Arc::new(std::sync::Mutex::new("offrig-small".to_string()));
    let named = Arc::clone(&pod_name);
    let pod = move || {
        format!(
            r#"{{"id":"p1","name":"{}","desiredStatus":"RUNNING","costPerHr":0.25,"ports":["22/tcp"]}}"#,
            named.lock().expect("name")
        )
    };
    let created: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let created_log = Arc::clone(&created);
    let (url, hits) = mock(move |route, body, _| match route {
        "POST /graphql" => (200, graphql(body)),
        "GET /pods" => (200, "[]".into()),
        "POST /pods" => {
            created_log.lock().expect("log").push(body.to_string());
            (200, pod())
        }
        "GET /pods/p1" if gone.load(Ordering::SeqCst) => (404, r#"{"error":"not found"}"#.into()),
        "GET /pods/p1" => (200, pod()),
        "DELETE /pods/p1" => {
            gone.store(true, Ordering::SeqCst);
            (204, String::new())
        }
        _ => (404, "{}".into()),
    });

    let dir = temp("phase2");
    {
        let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
        s.set_budget_cap(15.0).expect("cap");
        s.add_handoff(NewHandoff {
            role_id: "game-designer".into(),
            mission: "design the standoff duel".into(),
            acceptance: "spec lists verbs and failure states".into(),
            ..Default::default()
        })
        .expect("handoff");
    }
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(&dir);
        c.env("RUNPOD_API_KEY", "test-key");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        c.env("OFFRIG_TEST_RUNPOD_BASE", &url);
        c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
    });
    let client = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");
    let call = |name: &'static str, a: Value| {
        let client = &client;
        async move {
            client
                .call_tool(
                    CallToolRequestParams::new(name)
                        .with_arguments(a.as_object().cloned().expect("object")),
                )
                .await
                .expect("call")
        }
    };

    let names: Vec<String> = client
        .list_all_tools()
        .await
        .expect("list")
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    for want in [
        "offrig_launch",
        "offrig_job",
        "offrig_ask",
        "offrig_shutdown",
        "offrig_run",
    ] {
        assert!(names.contains(&want.to_string()), "missing {want}");
    }

    let plan = call("offrig_plan", json!({"profile": "small", "max_hours": 1.0})).await;
    assert_eq!(plan.is_error, Some(false), "{:?}", body(&plan));
    let plan_id = body(&plan)["plan_id"].as_i64().expect("plan id");
    assert_eq!(body(&plan)["worst_case"], 0.25);
    // The project got its own lane (named for its folder), recorded on the plan.
    let tag = body(&plan)["lane"].as_str().expect("lane tag").to_string();
    assert!(tag.starts_with("offrig-phase2"), "{tag}");
    *pod_name.lock().expect("name") = format!("offrig-{tag}-small");
    let registry =
        std::fs::read_to_string(dir.join("cfg").join("lanes.toml")).expect("lanes.toml written");
    assert!(registry.contains(&format!("tag = \"{tag}\"")), "{registry}");
    assert!(registry.contains(&format!("ssh_alias = \"offrig-{tag}\"")));
    assert!(registry.contains("tunnel_port = 11500"), "{registry}");

    let launch = call("offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(launch.is_error, Some(false), "{:?}", body(&launch));
    let job_id = body(&launch)["job_id"].as_i64().expect("job id");
    assert_eq!(body(&launch)["budget"]["committed"], 0.25);

    let again = call("offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(body(&again)["already_launched"], true, "{:?}", body(&again));
    assert_eq!(body(&again)["job_id"].as_i64(), Some(job_id));

    // The job creates exactly one pod and reaches the wait for its address.
    let started = Instant::now();
    loop {
        let j = call("offrig_job", json!({"job_id": job_id})).await;
        if body(&j)["progress"]["pod_id"] == "p1" {
            assert_eq!(body(&j)["state"], "running");
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "pod never created: {:?}",
            body(&j)
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(count(&hits, "POST /pods"), 1);
    // The pod is named for the lane, never the plain `offrig-small`.
    let sent = created.lock().expect("log").clone();
    let name = serde_json::from_str::<Value>(&sent[0]).expect("create body")["name"]
        .as_str()
        .expect("name")
        .to_string();
    assert_eq!(name, format!("offrig-{tag}-small"));

    // No ssh endpoint yet: asking fails with something to do, and touches no handoff.
    let ask = call(
        "offrig_ask",
        json!({"handoff_id": 1, "instruction": "draft the spec"}),
    )
    .await;
    assert_eq!(ask.is_error, Some(true));
    let st = call("offrig_handoffs", json!({"action": "list"})).await;
    assert_eq!(body(&st)["handoffs"][0]["state"], "pending");

    // A failure outcome must say what failed; the hint says how.
    let bare = call(
        "offrig_handoffs",
        json!({"action": "fail", "handoff_id": 1}),
    )
    .await;
    assert_eq!(bare.is_error, Some(true));
    assert!(
        body(&bare)["next_action"]
            .as_str()
            .is_some_and(|n| n.contains("pass reason")),
        "{:?}",
        body(&bare)
    );

    // The runner waits for a ready launch, and there is no output to read yet.
    let early = call("offrig_run", json!({"plan_id": plan_id})).await;
    assert_eq!(early.is_error, Some(true));
    assert!(
        body(&early)["error"]
            .as_str()
            .is_some_and(|e| e.contains("not ready")),
        "{:?}",
        body(&early)
    );
    let none = call(
        "offrig_handoffs",
        json!({"action": "output", "handoff_id": 1}),
    )
    .await;
    assert_eq!(none.is_error, Some(true));

    // While the pod is up, status points at the live session, not at setup.
    let live = call("offrig_status", json!({})).await;
    assert!(
        body(&live)["next_action"]
            .as_str()
            .is_some_and(|n| n.contains("live on pod p1")),
        "{:?}",
        body(&live)
    );

    assert_eq!(body(&live)["lane"]["tag"], tag.as_str());
    assert_eq!(body(&live)["lane"]["tunnel_port"], 11500);

    let down = call("offrig_shutdown", json!({"plan_id": plan_id})).await;
    assert_eq!(down.is_error, Some(false), "{:?}", body(&down));
    assert!(
        body(&down)["outcome"]
            .as_str()
            .is_some_and(|o| o.contains("p1"))
    );
    assert!(count(&hits, "DELETE /pods/p1") >= 1);
    let twice = call("offrig_shutdown", json!({"plan_id": plan_id})).await;
    assert_eq!(body(&twice)["already"], "closed", "shutdown is idempotent");

    let status = call("offrig_status", json!({})).await;
    let b = &body(&status)["budget"];
    assert_eq!(b["committed"], 0.0, "the commitment was released: {b:?}");
    assert!(b["remaining"].as_f64().is_some_and(|r| r > 14.9), "{b:?}");
    assert_eq!(count(&hits, "POST /pods"), 1, "never a second pod");

    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_watchdog_process_terminates_at_the_deadline() {
    let (url, hits) = mock(|route, _, _| match route {
        "GET /pods" => (
            200,
            r#"[{"id":"p9","name":"offrig-small","desiredStatus":"RUNNING","costPerHr":0.25}]"#
                .into(),
        ),
        "DELETE /pods/p9" => (204, String::new()),
        _ => (404, "{}".into()),
    });
    let dir = temp("watchdog");
    let db = dir.join(".offrig").join("offrig.db");
    let plan_id = {
        let s = Store::open(&db).expect("store");
        s.set_budget_cap(15.0).expect("cap");
        let p = s
            .create_plan(NewPlan {
                profile: "small".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 0.0005, // under two seconds
                max_price_hr: 0.25,
                note: None,
            })
            .expect("plan");
        s.commit_plan(p.id).expect("commit");
        s.attach_pod(p.id, "p9").expect("attach");
        p.id
    };
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp"))
        .args(["--watchdog", &plan_id.to_string(), "--project"])
        .arg(&dir)
        .env("RUNPOD_API_KEY", "test-key")
        .env("OFFRIG_CONFIG_DIR", dir.join("cfg"))
        .env("OFFRIG_TEST_RUNPOD_BASE", &url)
        .env("OFFRIG_TEST_WATCHDOG_POLL_SECS", "1")
        .spawn()
        .expect("spawn watchdog");
    let started = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().expect("wait") {
            break st;
        }
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("watchdog did not finish");
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(status.success());
    assert_eq!(
        count(&hits, "DELETE /pods/p9"),
        1,
        "terminated exactly once"
    );
    let s = Store::open(&db).expect("reopen");
    assert_eq!(
        s.plan(plan_id).expect("read").expect("exists").state,
        "closed"
    );
    assert!((s.budget().expect("b").committed).abs() < 1e-9);
    let log = std::fs::read_to_string(dir.join(".offrig").join(format!("watchdog-{plan_id}.log")))
        .expect("log");
    assert!(log.contains("Terminate"), "{log}");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lane boundary on the destructive path: a plan's shutdown deletes only a pod its
/// own lane named. A lane plan whose recorded pod is the plain lane's `offrig-job`, or
/// another lane's, is refused with no delete sent; a plan from before lanes (no lane
/// recorded) still shuts down its plain-lane pod.
#[tokio::test]
async fn shutdown_never_deletes_a_pod_outside_the_plans_lane() {
    let pod_json = |id: &str, name: &str| {
        format!(r#"{{"id":"{id}","name":"{name}","desiredStatus":"RUNNING","costPerHr":0.25}}"#)
    };
    let (job, theirs, old) = (
        pod_json("p7", "offrig-job"),
        pod_json("p8", "offrig-someone-else-small"),
        pod_json("p9", "offrig-small"),
    );
    let (url, hits) = mock(move |route, _, _| match route {
        "GET /pods/p7" => (200, job.clone()),
        "GET /pods/p8" => (200, theirs.clone()),
        "GET /pods/p9" => (200, old.clone()),
        "DELETE /pods/p7" | "DELETE /pods/p8" | "DELETE /pods/p9" => (204, String::new()),
        _ => (404, "{}".into()),
    });
    let dir = temp("lane-shutdown");
    let cfg_dir = dir.join("cfg");
    let lane = offrig_core::lanes::Registry::at(&cfg_dir)
        .lane_for_project(&dir, &offrig_core::config::Config::default())
        .expect("lane");
    let tag = lane.tag.clone().expect("tag");
    let plan_of = |lane: Option<&str>, pod: &str| {
        let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
        s.set_budget_cap(15.0).expect("cap");
        let p = s
            .create_plan(NewPlan {
                profile: "small".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 1.0,
                max_price_hr: 0.25,
                note: None,
            })
            .expect("plan");
        if let Some(t) = lane {
            s.set_plan_lane(p.id, t).expect("lane");
        }
        s.commit_plan(p.id).expect("commit");
        s.attach_pod(p.id, pod).expect("attach");
        p.id
    };
    // Two lane plans whose pods are not the lane's, and one pre-lanes plan.
    let into_job = plan_of(Some(&tag), "p7");
    let into_theirs = plan_of(Some(&tag), "p8");
    let before_lanes = plan_of(None, "p9");

    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(&dir);
        c.env("RUNPOD_API_KEY", "test-key");
        c.env("OFFRIG_CONFIG_DIR", &cfg_dir);
        c.env("OFFRIG_TEST_RUNPOD_BASE", &url);
        c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
    });
    let client = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");
    let call = |plan_id: i64| {
        let client = &client;
        async move {
            client
                .call_tool(
                    CallToolRequestParams::new("offrig_shutdown").with_arguments(
                        json!({"plan_id": plan_id})
                            .as_object()
                            .cloned()
                            .expect("object"),
                    ),
                )
                .await
                .expect("call")
        }
    };
    for (plan, pod) in [(into_job, "p7"), (into_theirs, "p8")] {
        let r = call(plan).await;
        assert_eq!(r.is_error, Some(true), "{:?}", body(&r));
        assert!(
            body(&r)["error"]
                .as_str()
                .is_some_and(|e| e.contains("not in this project's lane")),
            "{:?}",
            body(&r)
        );
        assert_eq!(count(&hits, &format!("DELETE /pods/{pod}")), 0);
    }
    let r = call(before_lanes).await;
    assert_eq!(r.is_error, Some(false), "{:?}", body(&r));
    assert_eq!(count(&hits, "DELETE /pods/p9"), 1);
    // The refused plans are still open: nothing was closed or billed.
    let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
    for plan in [into_job, into_theirs] {
        assert_eq!(
            s.plan(plan).expect("read").expect("plan").state,
            "committed"
        );
    }
    drop(s);
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}
