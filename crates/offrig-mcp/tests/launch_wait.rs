//! Issue #15 end to end against a mock RunPod: a narrowed plan retries quietly while its
//! GPUs have no capacity (each retry shows in `offrig_job`), the plan's `wait_minutes`
//! and the job profile's default decide how long, and `offrig_job` derives the step of a
//! booting pod from the pod's state on each call. The mock never gives the launch an ssh
//! endpoint, so no ssh config is touched; the live-step test builds the job row by hand.

mod common;

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{count, mock, temp};
use offrig_core::store::Store;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

const RTX_S: &str = "NVIDIA RTX PRO 6000 Blackwell Server Edition";
const RTX_W: &str = "NVIDIA RTX PRO 6000 Blackwell Workstation Edition";

/// Offers for the job profile's cards; `free` false lists no price (nothing free now).
fn graphql(body: &str, free: bool) -> String {
    if body.contains("myself") {
        return r#"{"data":{"myself":{"clientBalance":50.0,"currentSpendPerHr":0.0,"spendLimit":80}}}"#
            .into();
    }
    let price = if free { "2.09" } else { "null" };
    let t = |id: &str, gb: u32| {
        format!(
            r#"{{"id":"{id}","displayName":"{id}","memoryInGb":{gb},"secureCloud":true,"lowestPrice":{{"uninterruptablePrice":{price},"stockStatus":null}}}}"#
        )
    };
    format!(
        r#"{{"data":{{"gpuTypes":[{},{}]}}}}"#,
        t(RTX_S, 96),
        t(RTX_W, 96)
    )
}

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

type Client = RunningService<RoleClient, ()>;

async fn start(dir: &std::path::Path, url: &str) -> Client {
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(dir);
        c.env("RUNPOD_API_KEY", "test-key");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        c.env("OFFRIG_TEST_RUNPOD_BASE", url);
        c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
        c.env("OFFRIG_TEST_CAPACITY_POLL_MS", "100");
    });
    ().serve(TokioChildProcess::new(cmd).expect("spawn"))
        .await
        .expect("connect")
}

async fn call(client: &Client, name: &'static str, a: Value) -> CallToolResult {
    client
        .call_tool(
            CallToolRequestParams::new(name).with_arguments(a.as_object().cloned().expect("obj")),
        )
        .await
        .expect("call")
}

fn project(name: &str) -> std::path::PathBuf {
    let dir = temp(name);
    let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
    s.set_budget_cap(100.0).expect("cap");
    dir
}

async fn until<F: FnMut() -> bool>(what: &str, mut f: F) {
    let started = Instant::now();
    while !f() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn a_plan_stores_its_wait_and_the_job_profile_waits_by_default() {
    let (url, _hits) = mock(|route, b, _| match route {
        "POST /graphql" => (200, graphql(b, true)),
        _ => (404, "{}".into()),
    });
    let dir = project("wait-plan");
    let client = start(&dir, &url).await;
    let db = dir.join(".offrig").join("offrig.db");

    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "no_fallback": true}),
    )
    .await;
    assert_eq!(p.is_error, Some(false), "{:?}", body(&p));
    assert_eq!(body(&p)["wait_minutes"], 20, "the job profile's default");
    let first = body(&p)["plan_id"].as_i64().expect("plan id");

    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "wait_minutes": 45}),
    )
    .await;
    assert_eq!(body(&p)["wait_minutes"], 45);
    let second = body(&p)["plan_id"].as_i64().expect("plan id");

    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "wait_minutes": 0}),
    )
    .await;
    assert_eq!(body(&p)["wait_minutes"], 0, "a plan can ask for no wait");
    let third = body(&p)["plan_id"].as_i64().expect("plan id");

    let s = Store::open(&db).expect("store");
    assert_eq!(s.plan_wait_minutes(first).expect("read"), None);
    assert_eq!(s.plan_wait_minutes(second).expect("read"), Some(45));
    assert_eq!(s.plan_wait_minutes(third).expect("read"), Some(0));
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_narrowed_plan_retries_quietly_and_each_retry_shows_in_offrig_job() {
    let full = Arc::new(AtomicBool::new(false));
    let market = Arc::clone(&full);
    let (url, hits) = mock(move |route, b, _| match route {
        "POST /graphql" => (200, graphql(b, !market.load(Ordering::SeqCst))),
        "GET /pods" => (200, "[]".into()),
        // No ssh endpoint: the launch waits for an address, touching no ssh config.
        "POST /pods" => (
            200,
            r#"{"id":"p1","name":"offrig-x","desiredStatus":"RUNNING","costPerHr":2.09}"#.into(),
        ),
        "GET /pods/p1" | "DELETE /pods/p1" => (
            200,
            r#"{"id":"p1","name":"offrig-x","desiredStatus":"RUNNING","costPerHr":2.09}"#.into(),
        ),
        _ => (404, "{}".into()),
    });
    let dir = project("wait-launch");
    let client = start(&dir, &url).await;
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "no_fallback": true, "max_price_hr": 2.2}),
    )
    .await;
    assert_eq!(p.is_error, Some(false), "{:?}", body(&p));
    let plan_id = body(&p)["plan_id"].as_i64().expect("plan id");

    // The market fills after the plan was priced: the launch must wait, not fail.
    full.store(true, Ordering::SeqCst);
    let l = call(&client, "offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(l.is_error, Some(false), "{:?}", body(&l));
    assert_eq!(body(&l)["capacity_wait_minutes"], 20);

    let mut checks = 0;
    let started = Instant::now();
    while checks < 3 {
        assert!(started.elapsed() < Duration::from_secs(30), "no retries");
        let j = call(&client, "offrig_job", json!({"plan_id": plan_id})).await;
        let b = body(&j);
        assert_eq!(b["state"], "running", "{b:?}");
        checks = b["progress"]["capacity_wait"]["checks"]
            .as_u64()
            .unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let j = call(&client, "offrig_job", json!({"plan_id": plan_id})).await;
    let b = body(&j);
    let step = b["progress"]["step"].as_str().expect("step");
    assert!(
        step.contains("no capacity yet") || step.starts_with("no "),
        "{step}"
    );
    assert_eq!(b["progress"]["capacity_wait"]["limit_secs"], 20 * 60);
    assert_eq!(b["progress"]["phase"], "capacity");
    assert_eq!(
        count(&hits, "POST /pods"),
        0,
        "nothing is created while it is full"
    );

    // Capacity returns: the launch creates the pod without being asked again.
    full.store(false, Ordering::SeqCst);
    until("the pod create", || count(&hits, "POST /pods") == 1).await;
    let j = call(&client, "offrig_job", json!({"plan_id": plan_id})).await;
    assert_eq!(body(&j)["progress"]["pod_id"], "p1", "{:?}", body(&j));

    let _ = call(&client, "offrig_shutdown", json!({"plan_id": plan_id})).await;
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_plan_with_no_wait_still_fails_at_once_naming_how_to_set_one() {
    let (url, hits) = mock(|route, b, _| match route {
        "POST /graphql" => (200, graphql(b, false)),
        "GET /pods" => (200, "[]".into()),
        _ => (404, "{}".into()),
    });
    let dir = project("wait-none");
    // The plan is priced while the market lists a price; the mock above lists none, so
    // price it directly in the store the way offrig_plan would have.
    let db = dir.join(".offrig").join("offrig.db");
    let plan_id = {
        let s = Store::open(&db).expect("store");
        let plan = s
            .create_plan(offrig_core::store::NewPlan {
                profile: "job".into(),
                gpu_count: 1,
                gpu_types: vec![RTX_S.into(), RTX_W.into()],
                max_hours: 1.0,
                max_price_hr: 2.09,
                note: None,
            })
            .expect("plan");
        s.set_plan_wait_minutes(plan.id, 0).expect("wait");
        plan.id
    };
    let client = start(&dir, &url).await;
    let l = call(&client, "offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(l.is_error, Some(false), "{:?}", body(&l));
    until("the launch to fail", || {
        Store::open(&db)
            .ok()
            .and_then(|s| s.job_for_plan(plan_id, "launch").ok().flatten())
            .is_some_and(|j| j.state == "failed")
    })
    .await;
    let j = call(&client, "offrig_job", json!({"plan_id": plan_id})).await;
    let err = body(&j)["error"].as_str().expect("error").to_string();
    assert!(
        err.contains("no capacity") && err.contains("wait_minutes"),
        "{err}"
    );
    assert_eq!(count(&hits, "POST /pods"), 0);
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_booting_pods_step_is_derived_from_its_state_on_every_poll() {
    // A listener that answers like sshd, standing in for the pod's published port.
    let sshd = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = sshd.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for mut s in sshd.incoming().map_while(Result::ok) {
            let _ = s.write_all(b"SSH-2.0-OpenSSH_9.6\r\n");
        }
    });
    let phase: Arc<Mutex<&'static str>> = Arc::new(Mutex::new("no_address"));
    let seen = Arc::clone(&phase);
    let (url, _hits) = mock(move |route, _, _| match route {
        "GET /pods/p1" => {
            let pod = match *seen.lock().expect("phase") {
                "no_address" => {
                    r#"{"id":"p1","name":"offrig-x","desiredStatus":"RUNNING","costPerHr":2.09}"#
                        .to_string()
                }
                _ => format!(
                    r#"{{"id":"p1","name":"offrig-x","desiredStatus":"RUNNING","costPerHr":2.09,"publicIp":"127.0.0.1","portMappings":{{"22":{port}}}}}"#
                ),
            };
            (200, pod)
        }
        _ => (404, "{}".into()),
    });
    let dir = project("live-step");
    let db = dir.join(".offrig").join("offrig.db");
    let (plan_id, job_id) = {
        let s = Store::open(&db).expect("store");
        let plan = s
            .create_plan(offrig_core::store::NewPlan {
                profile: "job".into(),
                gpu_count: 1,
                gpu_types: vec![RTX_S.into()],
                max_hours: 1.0,
                max_price_hr: 2.09,
                note: None,
            })
            .expect("plan");
        let job = s.create_job(plan.id, "launch").expect("job");
        // What the launch thread left behind: a step that went stale on a slow host.
        s.update_job(
            job,
            "running",
            &json!({
                "step": "waiting for the pod to pull its image and get a public address (582s)",
                "at": 1,
                "phase": "pod_boot",
                "pod_id": "p1",
                "pod_created_at": offrig_core::cost::now_unix() - 582,
            }),
            None,
        )
        .expect("progress");
        (plan.id, job)
    };
    let client = start(&dir, &url).await;

    let b = body(&call(&client, "offrig_job", json!({"job_id": job_id})).await);
    let step = b["progress"]["step"].as_str().expect("step").to_string();
    assert!(
        step.starts_with("waiting for the pod to pull its image"),
        "{step}"
    );
    assert!(step.contains("s after it was created"), "{step}");
    assert_eq!(
        b["progress"]["step_source"],
        "the pod's state, checked just now"
    );

    // The address appears and sshd answers: the next poll says so, not the old line.
    *phase.lock().expect("phase") = "ssh_up";
    let b = body(&call(&client, "offrig_job", json!({"plan_id": plan_id})).await);
    let step = b["progress"]["step"].as_str().expect("step").to_string();
    assert!(
        step.starts_with(&format!("ssh answers at 127.0.0.1:{port}")),
        "{step}"
    );
    assert!(
        b["progress"]["launch_step"]
            .as_str()
            .is_some_and(|s| s.contains("(582s)")),
        "the launch thread's own last step stays visible, labelled"
    );

    // A finished launch is shown as recorded, with no live look.
    {
        let s = Store::open(&db).expect("store");
        s.update_job(job_id, "done", &json!({"step": "ready"}), None)
            .expect("done");
    }
    let b = body(&call(&client, "offrig_job", json!({"job_id": job_id})).await);
    assert_eq!(b["progress"]["step"], "ready");
    assert!(b["progress"]["step_source"].is_null());
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}
