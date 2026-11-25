//! End-to-end tests for MCP Tasks (`io.modelcontextprotocol/tasks`, SEP-2663) on
//! `offrig_launch` and the server-carried static `offrig-session` skill
//! (`io.modelcontextprotocol/skills`, SEP-2640). Uses only the local fake RunPod
//! HTTP server and fake SSH binary; `RUNPOD_API_KEY` is never sent anywhere real.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

use common::{Hits, count, install_fake_tools, mock, path_with, temp};
use offrig_core::config::Config;
use offrig_core::lanes::Registry;
use offrig_core::store::Store;
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams, ClientCapabilities,
    ClientConfig, ErrorCode, GetSkillRequestParams, GetTaskParams, Implementation,
    ReadResourceRequestParams, RequestMetaObject, RequestParamsMeta, ResourceContents,
    SkillResources, TaskPayload, TaskStatus, UpdateTaskParams,
};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

type Pods = Arc<Mutex<Vec<Value>>>;

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

fn offers_json() -> String {
    let ids = Config::default()
        .profile("job")
        .expect("job")
        .gpu_type_ids
        .clone();
    let types: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({
                "id": id, "name": id, "memory": 80, "secure": true,
                "price": {"secure": 0.50}, "maxCount": {"secure": 8}, "availability": "HIGH"
            })
        })
        .collect();
    json!({"gpus": types}).to_string()
}

fn stateful_runpod_with_create_status(
    pods: Pods,
    create_available: Arc<std::sync::atomic::AtomicBool>,
) -> impl Fn(&str, &str, usize) -> (u16, String) + Send + Sync + 'static {
    move |route, body, _| {
        let mut pods = pods.lock().expect("pods");
        match route {
            "POST /graphql" if body.contains("myself") => (
                200,
                r#"{"data":{"myself":{"clientBalance":40.0,"currentSpendPerHr":0.0,"spendLimit":80}}}"#
                    .into(),
            ),
            "GET /catalog/gpus" => {
                if create_available.load(std::sync::atomic::Ordering::SeqCst) {
                    (200, offers_json())
                } else {
                    (200, r#"{"gpus":[]}"#.into())
                }
            }
            "GET /pods" => (200, Value::Array(pods.clone()).to_string()),
            "POST /pods" => {
                if !create_available.load(std::sync::atomic::Ordering::SeqCst) {
                    return (
                        500,
                        r#"{"error":"There are no instances available of the requested type"}"#.into(),
                    );
                }
                let want: Value = serde_json::from_str(body).unwrap_or_default();
                let id = format!("pod{}", pods.len() + 1);
                let p = json!({
                    "id": id,
                    "name": want["name"],
                    "status": "RUNNING",
                    "cost": 0.50,
                    "ports": ["22/tcp"],
                    "env": want["env"],
                    "ssh": {
                        "direct": {
                            "host": "203.0.113.9",
                            "port": 40022
                        }
                    },
                    "gpu": {
                        "id": "NVIDIA A100-SXM4-80GB",
                        "count": 1
                    },
                    "cudaVersion": "13.0"
                });
                pods.push(p.clone());
                (200, p.to_string())
            }
            r if r.starts_with("GET /pods/") => {
                let id = &r["GET /pods/".len()..];
                match pods.iter().find(|p| p["id"] == id) {
                    Some(p) => (200, p.to_string()),
                    None => (404, r#"{"error":"not found"}"#.into()),
                }
            }
            r if r.starts_with("DELETE /pods/") => {
                let id = &r["DELETE /pods/".len()..];
                pods.retain(|p| p["id"] != id);
                (204, String::new())
            }
            _ => (404, "{}".into()),
        }
    }
}

fn spawn_cmd(
    dir: &std::path::Path,
    url: &str,
    bin: &std::path::Path,
    fake: &std::path::Path,
    no_watchdog: bool,
    extra_env: &[(&str, String)],
) -> tokio::process::Command {
    let extra = extra_env
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect::<Vec<_>>();
    tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.kill_on_drop(true);
        c.arg("--project").arg(dir);
        c.env("RUNPOD_API_KEY", "test-key");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        c.env("OFFRIG_TEST_HOME", dir.join("home"));
        c.env("OFFRIG_TEST_RUNPOD_BASE", url);
        if no_watchdog {
            c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
        }
        c.env("OFFRIG_TEST_WATCHDOG_POLL_SECS", "1");
        c.env("OFFRIG_TEST_DELETE_BACKOFF_MS", "10");
        c.env("FAKE_SSH_DIR", fake);
        c.env("PATH", path_with(bin));
        for (k, v) in &extra {
            c.env(k, v);
        }
    })
}

async fn connect_tasks_client(
    dir: &std::path::Path,
    url: &str,
    bin: &std::path::Path,
    fake: &std::path::Path,
    no_watchdog: bool,
    extra_env: &[(&str, String)],
) -> rmcp::service::RunningService<rmcp::RoleClient, ClientConfig> {
    let cfg = ClientConfig::new(
        ClientCapabilities::builder().enable_tasks().build(),
        Implementation::new("tasks-test-client", "1.0.0"),
    );
    let cmd = spawn_cmd(dir, url, bin, fake, no_watchdog, extra_env);
    cfg.serve(TokioChildProcess::new(cmd).expect("spawn"))
        .await
        .expect("connect tasks client")
}

async fn connect_plain_client(
    dir: &std::path::Path,
    url: &str,
    bin: &std::path::Path,
    fake: &std::path::Path,
    no_watchdog: bool,
    extra_env: &[(&str, String)],
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let cmd = spawn_cmd(dir, url, bin, fake, no_watchdog, extra_env);
    ().serve(TokioChildProcess::new(cmd).expect("spawn"))
        .await
        .expect("connect plain client")
}

fn setup_fake_ready_rules(fake: &std::path::Path) {
    std::fs::write(
        fake.join("rules.json"),
        serde_json::json!([
            { "contains": "self-stop.status", "stdout": "offrig self-stop: armed fire_at=1999999999\n" },
            { "contains": "nvidia-smi --query-gpu=name", "stdout": "NVIDIA A100-SXM4-80GB\n" },
            { "contains": "nvidia-smi", "stdout": "CUDA Version: 13.0\n" }
        ])
        .to_string(),
    )
    .expect("rules.json");
}

fn init_project(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join(".offrig").join("offrig.db");
    let s = Store::open(&db).expect("store");
    s.set_budget_cap(50.0).expect("cap");
    db
}

async fn plan_job<H: rmcp::ClientHandler>(
    client: &rmcp::service::RunningService<rmcp::RoleClient, H>,
    max_hours: f64,
    wait_minutes: u32,
) -> i64 {
    let res = client
        .call_tool(
            CallToolRequestParams::new("offrig_plan").with_arguments(
                json!({
                    "profile": "job",
                    "max_hours": max_hours,
                    "wait_minutes": wait_minutes,
                })
                .as_object()
                .cloned()
                .expect("obj"),
            ),
        )
        .await
        .expect("plan call");
    assert_eq!(res.is_error, Some(false), "{:?}", body(&res));
    body(&res)["plan_id"].as_i64().expect("plan_id")
}

fn assert_zero_live_pods(pods: &Pods) {
    let list = pods.lock().expect("pods");
    let live: Vec<&Value> = list
        .iter()
        .filter(|p| p["status"].as_str() != Some("TERMINATED"))
        .collect();
    assert!(
        live.is_empty(),
        "expected zero live fake pods, found: {live:?}"
    );
}

#[tokio::test]
async fn cancel_mid_launch_sets_flag_and_runs_shutdown_with_task_cancelled() {
    let pods: Pods = Arc::default();
    let avail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (url, hits): (_, Hits) = mock(stateful_runpod_with_create_status(
        Arc::clone(&pods),
        Arc::clone(&avail),
    ));
    let dir = temp("ts-cancel-mid");
    let db = init_project(&dir);
    let (bin, fake) = install_fake_tools(&dir);
    setup_fake_ready_rules(&fake);

    // Part A: Mid-launch capacity wait cancellation sets the cancel flag and runs shutdown
    // with the fixed reason "task cancelled".
    let client = connect_tasks_client(
        &dir,
        &url,
        &bin,
        &fake,
        true,
        &[("OFFRIG_TEST_CAPACITY_POLL_MS", "250".to_string())],
    )
    .await;
    let plan1 = plan_job(&client, 1.0, 5).await;
    avail.store(false, std::sync::atomic::Ordering::SeqCst);
    let resp = client
        .call_tool_once(
            CallToolRequestParams::new("offrig_launch")
                .with_arguments(json!({"plan_id": plan1}).as_object().cloned().expect("obj")),
        )
        .await
        .expect("launch");
    let CallToolResponse::Task(created) = resp else {
        panic!("expected CreateTaskResult, got {resp:?}");
    };
    let task_id = created.task.task_id.clone();
    assert_eq!(created.task.poll_interval_ms, Some(5_000));
    assert!(created.task.ttl_ms.is_some());

    // Wait until background launch entered capacity polling (GET /catalog/gpus called by launch).
    let w = Instant::now();
    while count(&hits, "GET /catalog/gpus") < 2 {
        assert!(w.elapsed() < Duration::from_secs(15));
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    let gt0 = client
        .get_task(GetTaskParams::new(&task_id))
        .await
        .expect("get_task while working");
    assert_eq!(gt0.task.status(), TaskStatus::Working);

    client
        .cancel_task(CancelTaskParams::new(&task_id))
        .await
        .expect("cancel_task mid-launch");

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let gt = client
            .get_task(GetTaskParams::new(&task_id))
            .await
            .expect("get_task after cancel");
        if gt.task.status() == TaskStatus::Cancelled {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "task did not reach Cancelled: {gt:?}"
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
    }

    // Wait until the launch background thread also observes the cancel flag and exits as
    // `cancelled`, closing `create_pod` so the lane is clear for the next launch.
    let settle_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let s = Store::open(&db).expect("reopen");
        let p1 = s.plan(plan1).expect("p1").expect("exists");
        let j1_job = s
            .job_for_plan(plan1, "launch")
            .expect("job")
            .expect("exists");
        let create_settled = s.unfinished_create_pod(plan1).expect("u").is_none();
        if p1.state == "closed" && j1_job.state == "cancelled" && create_settled {
            let j1 = s.journal_for_plan(plan1).expect("j1");
            assert!(
                j1.iter()
                    .any(|(action, intent, _)| action == "terminate_pod"
                        && intent.contains("\"reason\":\"task cancelled\"")),
                "expected terminate_pod with reason 'task cancelled': {j1:?}"
            );
            break;
        }
        assert!(
            Instant::now() < settle_deadline,
            "launch job did not observe cancel flag: state={} create_settled={}",
            j1_job.state,
            create_settled
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_zero_live_pods(&pods);

    // Part B: Shutdown refusal when handoffs are in flight is NOT bypassed silently;
    // tasks/cancel surfaces the refusal on the task while the pod remains governed by explicit shutdown.
    avail.store(true, std::sync::atomic::Ordering::SeqCst);
    let plan2 = plan_job(&client, 1.0, 0).await;
    let resp2 = client
        .call_tool_once(
            CallToolRequestParams::new("offrig_launch")
                .with_arguments(json!({"plan_id": plan2}).as_object().cloned().expect("obj")),
        )
        .await
        .expect("launch plan2");
    let CallToolResponse::Task(created2) = resp2 else {
        panic!("expected CreateTaskResult, got {resp2:?}");
    };
    let task2_id = created2.task.task_id.clone();

    let ready_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let gt = client
            .get_task(GetTaskParams::new(&task2_id))
            .await
            .expect("poll plan2");
        if gt.task.status() == TaskStatus::Completed {
            break;
        }
        assert!(
            Instant::now() < ready_deadline,
            "plan2 did not complete: {gt:?}"
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
    }

    // Queue a handoff so an in-flight handoff is present on the store.
    let add_h = client
        .call_tool(
            CallToolRequestParams::new("offrig_handoffs").with_arguments(
                json!({
                    "action": "add",
                    "role": "game-designer",
                    "mission": "draft combat loop",
                    "acceptance": "lists actions",
                })
                .as_object()
                .cloned()
                .expect("obj"),
            ),
        )
        .await
        .expect("add handoff");
    assert_eq!(add_h.is_error, Some(false), "{:?}", body(&add_h));
    let hid = body(&add_h)["id"].as_i64().expect("hid");
    Store::open(&db)
        .expect("reopen for handoff")
        .transition(hid, offrig_core::store::State::Dispatched, "start", None)
        .expect("dispatch handoff");

    client
        .cancel_task(CancelTaskParams::new(&task2_id))
        .await
        .expect("cancel_task ack even when shutdown refuses");

    let refused_gt = client
        .get_task(GetTaskParams::new(&task2_id))
        .await
        .expect("get_task after refused cancel");
    assert_eq!(refused_gt.task.status(), TaskStatus::Completed);
    let msg = refused_gt.task.task.status_message.as_deref().unwrap_or("");
    assert!(msg.contains("cancel refused"), "status_message: {msg}");
    let TaskPayload::Completed { ref result } = refused_gt.task.payload else {
        panic!("expected Completed payload with refusal");
    };
    assert_eq!(result.get("isError").and_then(Value::as_bool), Some(true));
    assert_eq!(
        pods.lock().expect("pods").len(),
        1,
        "pod stays live until valid shutdown"
    );

    // Explicit shutdown with reason cleans up to zero live pods.
    let shut = client
        .call_tool(
            CallToolRequestParams::new("offrig_shutdown").with_arguments(
                json!({ "plan_id": plan2, "reason": "verified clean up" })
                    .as_object()
                    .cloned()
                    .expect("obj"),
            ),
        )
        .await
        .expect("explicit shutdown");
    assert_eq!(shut.is_error, Some(false), "{:?}", body(&shut));
    assert_zero_live_pods(&pods);

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn ttl_expiry_purges_task_while_plan_deadline_and_watchdog_govern_pod() {
    let pods: Pods = Arc::default();
    let avail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (url, _hits): (_, Hits) = mock(stateful_runpod_with_create_status(
        Arc::clone(&pods),
        Arc::clone(&avail),
    ));
    let dir = temp("ts-ttl");
    let db = init_project(&dir);
    let _ = Registry::at(dir.join("cfg"))
        .lane_for_project(&dir, &Config::default())
        .expect("lane");
    let (bin, fake) = install_fake_tools(&dir);
    setup_fake_ready_rules(&fake);

    // Part A: Short task TTL (250ms) expires before the ~7s plan deadline; tasks/get starts
    // failing (-32602), yet the pod keeps running until the plan deadline watchdog deletes it.
    let client_short = connect_tasks_client(
        &dir,
        &url,
        &bin,
        &fake,
        false,
        &[("OFFRIG_TEST_TASK_TTL_MS", "250".to_string())],
    )
    .await;
    let plan_short = plan_job(&client_short, 0.002, 0).await;
    let resp = client_short
        .call_tool_once(
            CallToolRequestParams::new("offrig_launch").with_arguments(
                json!({"plan_id": plan_short})
                    .as_object()
                    .cloned()
                    .expect("obj"),
            ),
        )
        .await
        .expect("launch");
    let CallToolResponse::Task(created) = resp else {
        panic!("expected task");
    };
    let task_id = created.task.task_id.clone();
    assert_eq!(created.task.ttl_ms, Some(250));

    // Wait until pod1 exists on the fake.
    let wait_pod = Instant::now() + Duration::from_secs(15);
    while pods.lock().expect("pods").is_empty() {
        assert!(Instant::now() < wait_pod, "pod never created");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Sleep past the 250ms task TTL (well before the ~7.2s plan deadline).
    tokio::time::sleep(Duration::from_millis(350)).await;
    let err = client_short
        .get_task(GetTaskParams::new(&task_id))
        .await
        .expect_err("expired task must disappear");
    assert!(
        err.to_string().contains("-32602") || err.to_string().contains("unknown task"),
        "expected invalid_params unknown task error, got: {err}"
    );
    assert_eq!(
        pods.lock().expect("pods").len(),
        1,
        "short TTL expiry must not kill pod before plan deadline"
    );

    // Wait for watchdog + plan deadline to terminate the pod.
    let wd_deadline = Instant::now() + Duration::from_secs(20);
    while !pods.lock().expect("pods").is_empty() {
        assert!(
            Instant::now() < wd_deadline,
            "watchdog did not terminate pod at deadline"
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    assert_zero_live_pods(&pods);
    client_short.cancel().await.ok();

    // Part B: Huge task TTL (3_600_000ms) never extends the pod past the immediate plan deadline.
    let client_long = connect_tasks_client(
        &dir,
        &url,
        &bin,
        &fake,
        false,
        &[("OFFRIG_TEST_TASK_TTL_MS", "3600000".to_string())],
    )
    .await;
    let plan_long = plan_job(&client_long, 0.0005, 0).await;
    let resp_long = client_long
        .call_tool_once(
            CallToolRequestParams::new("offrig_launch").with_arguments(
                json!({"plan_id": plan_long})
                    .as_object()
                    .cloned()
                    .expect("obj"),
            ),
        )
        .await
        .expect("launch long");
    let CallToolResponse::Task(created_long) = resp_long else {
        panic!("expected task");
    };
    assert_eq!(created_long.task.ttl_ms, Some(3_600_000));

    let wait_long = Instant::now() + Duration::from_secs(20);
    loop {
        let s = Store::open(&db).expect("reopen");
        let closed = s
            .plan(plan_long)
            .expect("p")
            .is_some_and(|p| p.state == "closed");
        if closed && pods.lock().expect("pods").is_empty() {
            break;
        }
        assert!(
            Instant::now() < wait_long,
            "long TTL delayed deadline termination"
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    // Task record is still queryable (TTL not expired), showing plan_state == "closed".
    let gt_long = client_long
        .get_task(GetTaskParams::new(&created_long.task.task_id))
        .await
        .expect("task still present inside 1h TTL");
    let meta = gt_long.meta.expect("meta");
    assert_eq!(
        meta.get("plan_state").and_then(Value::as_str),
        Some("closed")
    );
    assert_zero_live_pods(&pods);

    client_long.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn client_disconnect_without_cancel_still_terminates_at_deadline() {
    let pods: Pods = Arc::default();
    let avail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (url, _hits): (_, Hits) = mock(stateful_runpod_with_create_status(
        Arc::clone(&pods),
        Arc::clone(&avail),
    ));
    let dir = temp("ts-disconnect");
    let db = init_project(&dir);
    let _ = Registry::at(dir.join("cfg"))
        .lane_for_project(&dir, &Config::default())
        .expect("lane");
    let (bin, fake) = install_fake_tools(&dir);
    setup_fake_ready_rules(&fake);

    let client = connect_tasks_client(&dir, &url, &bin, &fake, false, &[]).await;
    let plan_id = plan_job(&client, 0.0005, 0).await;
    let resp = client
        .call_tool_once(
            CallToolRequestParams::new("offrig_launch").with_arguments(
                json!({"plan_id": plan_id})
                    .as_object()
                    .cloned()
                    .expect("obj"),
            ),
        )
        .await
        .expect("launch");
    assert!(matches!(resp, CallToolResponse::Task(_)));

    let wait_pod = Instant::now() + Duration::from_secs(15);
    while pods.lock().expect("pods").is_empty() {
        assert!(Instant::now() < wait_pod);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Disconnect client WITHOUT sending tasks/cancel!
    client.cancel().await.ok();

    let wait_gone = Instant::now() + Duration::from_secs(20);
    loop {
        let s = Store::open(&db).expect("reopen");
        let closed = s
            .plan(plan_id)
            .expect("p")
            .is_some_and(|p| p.state == "closed");
        if closed && pods.lock().expect("pods").is_empty() {
            break;
        }
        assert!(
            Instant::now() < wait_gone,
            "detached watchdog did not terminate pod after client disconnect"
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    assert_zero_live_pods(&pods);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn client_without_tasks_capability_gets_synchronous_result_byte_for_byte() {
    let pods: Pods = Arc::default();
    let avail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (url, _hits): (_, Hits) = mock(stateful_runpod_with_create_status(
        Arc::clone(&pods),
        Arc::clone(&avail),
    ));
    let dir = temp("ts-sync");
    let _db = init_project(&dir);
    let (bin, fake) = install_fake_tools(&dir);
    setup_fake_ready_rules(&fake);

    // Non-declaring client receives synchronous Complete responses for both offrig_launch and offrig_job.
    let client = connect_plain_client(&dir, &url, &bin, &fake, true, &[]).await;
    let plan_id = plan_job(&client, 1.0, 0).await;
    let resp = client
        .call_tool_once(
            CallToolRequestParams::new("offrig_launch").with_arguments(
                json!({"plan_id": plan_id})
                    .as_object()
                    .cloned()
                    .expect("obj"),
            ),
        )
        .await
        .expect("sync launch");
    let CallToolResponse::Complete(launch_res) = resp else {
        panic!("non-declaring client must receive CallToolResponse::Complete, got {resp:?}");
    };
    assert_eq!(launch_res.is_error, Some(false));
    let lbody = body(&launch_res);
    let mut keys: Vec<&str> = lbody
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "account_notes",
            "budget",
            "capacity_wait_minutes",
            "deadline",
            "job_id",
            "lane",
            "next_action",
            "ok",
            "plan_id",
            "watchdog",
            "worst_case",
        ]
    );
    assert_eq!(
        launch_res.content[0].as_text().expect("text").text,
        lbody.to_string(),
        "text content must match serialized JSON byte-for-byte"
    );

    // Synchronous offrig_job polling still works with identical JSON key shape.
    let job_res = client
        .call_tool(
            CallToolRequestParams::new("offrig_job").with_arguments(
                json!({ "plan_id": plan_id })
                    .as_object()
                    .cloned()
                    .expect("obj"),
            ),
        )
        .await
        .expect("job");
    let jbody = body(&job_res);
    let mut jkeys: Vec<&str> = jbody
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    jkeys.sort_unstable();
    assert_eq!(
        jkeys,
        vec![
            "code",
            "error",
            "failure",
            "gpu_count",
            "gpu_mismatch",
            "gpu_mismatch_source",
            "gpus",
            "job_id",
            "measured_gpu_count",
            "minutes_left",
            "next_action",
            "ok",
            "plan_id",
            "plan_state",
            "planned_gpu_count",
            "pod_rented",
            "progress",
            "project",
            "rented",
            "retryable",
            "spent_so_far",
            "state",
            "warnings",
            "watchdog_alive",
            "watchdog_error",
        ]
    );
    assert_eq!(
        job_res.content[0].as_text().expect("text").text,
        jbody.to_string(),
        "offrig_job text content must match serialized JSON byte-for-byte"
    );

    // Calling tasks/get without tasks capability returns -32021 Missing Required Client Capability.
    let err = client
        .get_task(GetTaskParams::new("task-any"))
        .await
        .expect_err("tasks/get without capability must be rejected");
    assert!(
        err.to_string().contains("-32021")
            || err
                .to_string()
                .contains("Missing required client capability"),
        "expected -32021 error, got: {err}"
    );

    // Per-request tasks capability inside `_meta` switches the call onto the Task path on the same client.
    let mut meta = RequestMetaObject::new();
    meta.set_client_capabilities(ClientCapabilities::builder().enable_tasks().build());
    let mut per_req = CallToolRequestParams::new("offrig_launch").with_arguments(
        json!({"plan_id": plan_id})
            .as_object()
            .cloned()
            .expect("obj"),
    );
    per_req.set_meta(meta);
    let per_req_resp = client
        .call_tool_once(per_req)
        .await
        .expect("per-request task");
    assert!(
        matches!(per_req_resp, CallToolResponse::Task(_)),
        "per-request tasks capability should return CreateTaskResult: {per_req_resp:?}"
    );

    // Wait for synchronous offrig_job polling to reach "done", then shut down so 0 live pods remain.
    let done_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let jr = client
            .call_tool(
                CallToolRequestParams::new("offrig_job").with_arguments(
                    json!({ "plan_id": plan_id })
                        .as_object()
                        .cloned()
                        .expect("obj"),
                ),
            )
            .await
            .expect("poll job");
        if body(&jr)["state"] == "done" {
            break;
        }
        assert!(
            Instant::now() < done_deadline,
            "sync launch did not reach done: {:?}",
            body(&jr)
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
    }

    let shut = client
        .call_tool(
            CallToolRequestParams::new("offrig_shutdown").with_arguments(
                json!({"plan_id": plan_id})
                    .as_object()
                    .cloned()
                    .expect("obj"),
            ),
        )
        .await
        .expect("shutdown");
    assert_eq!(shut.is_error, Some(false));
    assert_zero_live_pods(&pods);

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn is_error_launch_result_reports_completed_not_failed() {
    let pods: Pods = Arc::default();
    let avail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (url, _hits): (_, Hits) = mock(stateful_runpod_with_create_status(
        Arc::clone(&pods),
        Arc::clone(&avail),
    ));
    let dir = temp("ts-is-error");
    let _db = init_project(&dir);
    let (bin, fake) = install_fake_tools(&dir);
    setup_fake_ready_rules(&fake);

    let client = connect_tasks_client(&dir, &url, &bin, &fake, true, &[]).await;
    let plan_id = plan_job(&client, 1.0, 0).await;
    avail.store(false, std::sync::atomic::Ordering::SeqCst);
    let resp = client
        .call_tool_once(
            CallToolRequestParams::new("offrig_launch").with_arguments(
                json!({"plan_id": plan_id})
                    .as_object()
                    .cloned()
                    .expect("obj"),
            ),
        )
        .await
        .expect("launch");
    let CallToolResponse::Task(created) = resp else {
        panic!("expected CreateTaskResult, got {resp:?}");
    };
    let task_id = created.task.task_id.clone();

    let deadline = Instant::now() + Duration::from_secs(15);
    let final_gt = loop {
        let gt = client
            .get_task(GetTaskParams::new(&task_id))
            .await
            .expect("get_task");
        if gt.task.status().is_terminal() {
            break gt;
        }
        assert!(Instant::now() < deadline, "task did not finish: {gt:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(
        final_gt.task.status(),
        TaskStatus::Completed,
        "tool failure (isError=true) MUST map to Completed, never Failed: {final_gt:?}"
    );
    let TaskPayload::Completed { result } = final_gt.task.payload else {
        panic!("expected Completed payload");
    };
    assert_eq!(result.get("isError").and_then(Value::as_bool), Some(true));
    let sc = result
        .get("structuredContent")
        .expect("structuredContent on Completed result");
    assert_eq!(sc["code"], "no_capacity");
    assert_eq!(sc["pod_rented"], false);
    assert!(sc.get("deadline").is_some(), "{sc}");
    assert!(sc.get("progress").is_some(), "{sc}");
    assert!(sc.get("warnings").is_some(), "{sc}");
    assert_zero_live_pods(&pods);

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn unknown_or_guessed_task_id_and_tasks_update_are_refused() {
    let pods: Pods = Arc::default();
    let avail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (url, _hits): (_, Hits) = mock(stateful_runpod_with_create_status(
        Arc::clone(&pods),
        Arc::clone(&avail),
    ));
    let dir = temp("ts-unknown-id");
    let _db = init_project(&dir);
    let (bin, fake) = install_fake_tools(&dir);
    setup_fake_ready_rules(&fake);

    let client = connect_tasks_client(&dir, &url, &bin, &fake, true, &[]).await;
    let plan_id = plan_job(&client, 1.0, 0).await;
    let resp = client
        .call_tool_once(
            CallToolRequestParams::new("offrig_launch").with_arguments(
                json!({"plan_id": plan_id})
                    .as_object()
                    .cloned()
                    .expect("obj"),
            ),
        )
        .await
        .expect("launch");
    let CallToolResponse::Task(created) = resp else {
        panic!("expected task");
    };
    let real_task_id = created.task.task_id.clone();

    // Bearer task_id must be non-guessable (>= 32 hex chars random entropy) and reject guessed IDs.
    assert!(
        real_task_id.len() >= 37 && real_task_id.starts_with("task-"),
        "expected opaque bearer task id, got {real_task_id}"
    );
    for guess in [
        "1",
        "plan-1",
        "job-1",
        "task-1",
        "task-00000000000000000000000000000000",
        "unknown-task-id",
    ] {
        assert_ne!(real_task_id, guess);
        let err = client
            .get_task(GetTaskParams::new(guess))
            .await
            .expect_err("guessed task id must be refused");
        assert!(
            err.to_string().contains("unknown task") || err.to_string().contains("-32602"),
            "{guess}: {err}"
        );
        let cerr = client
            .cancel_task(CancelTaskParams::new(guess))
            .await
            .expect_err("cancel on guessed task id must be refused");
        assert!(
            cerr.to_string().contains("unknown task") || cerr.to_string().contains("-32602"),
            "cancel {guess}: {cerr}"
        );
    }

    // tasks/update is unused and refused cleanly.
    let uerr = client
        .update_task(UpdateTaskParams::new(&real_task_id, Default::default()))
        .await
        .expect_err("tasks/update must be refused");
    assert!(
        uerr.to_string().contains("-32602") || uerr.to_string().contains("tasks/update"),
        "{uerr}"
    );

    // Wait for ready, shut down, leave zero live pods.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let gt = client
            .get_task(GetTaskParams::new(&real_task_id))
            .await
            .expect("get real task");
        if gt.task.status().is_terminal() {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    let _ = client
        .call_tool(
            CallToolRequestParams::new("offrig_shutdown").with_arguments(
                json!({"plan_id": plan_id})
                    .as_object()
                    .cloned()
                    .expect("obj"),
            ),
        )
        .await
        .expect("shutdown");
    assert_zero_live_pods(&pods);

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

fn check_skill_procedure_only(text: &str) -> Result<(), String> {
    let unix_home_pat = format!(r#"/{}/[^/ \t\r\n"'`]+/"#, "home");
    let macos_users_pat = format!(r#"/{}/[^/ \t\r\n"'`]+/"#, "Users");
    let patterns: &[(&str, &str)] = &[
        ("windows_drive_path", r"(?:^|[^A-Za-z0-9_])[A-Za-z]:[\\/]"),
        ("unix_home_path", unix_home_pat.as_str()),
        ("macos_users_path", macos_users_pat.as_str()),
        (
            "api_key_or_pem",
            r"(?:rpa|sk|ghp|gho|ghs|glpat)_[A-Za-z0-9_\-]{8,}|sk-[A-Za-z0-9_\-]{12,}|AKIA[0-9A-Z]{12,}|AIza[0-9A-Za-z_\-]{20,}|-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----",
        ),
        (
            "long_hex_secret",
            r"(?:^|[^0-9A-Fa-f])[0-9A-Fa-f]{32,}(?:$|[^0-9A-Fa-f])",
        ),
        (
            "long_base64_secret",
            r"(?:^|[^A-Za-z0-9+/=_\-])[A-Za-z0-9+/]{40,}={0,2}(?:$|[^A-Za-z0-9+/=_\-])",
        ),
        (
            "hardcoded_budget",
            r"\$[0-9]+(?:\.[0-9]+)?|(?:^|[^A-Za-z0-9_])(?:(?:budget|cap|spend_limit|max_price_hr)[ \t\r\n]*[:=][ \t\r\n]*\$?[0-9]+|offrig[ \t\r\n]+budget[^\r\n]*[^A-Za-z0-9_][0-9]+)",
        ),
        (
            "hardcoded_lane",
            r#"(?:^|[^A-Za-z0-9_])(?:lane|ssh_alias|OFFRIG_LANE)[ \t\r\n]*[:=][ \t\r\n]*['"`]?[A-Za-z0-9_\-]+"#,
        ),
    ];
    for &(label, pat) in patterns {
        let re = regex_automata::meta::Regex::new(pat)
            .unwrap_or_else(|e| panic!("invalid regex {label}: {e}"));
        if let Some(m) = re.find(text) {
            let excerpt = text[m.range()].trim();
            return Err(format!("matched {label} ({excerpt:?})"));
        }
    }
    Ok(())
}

#[tokio::test]
async fn skills_list_and_get_match_file_digest_and_contain_no_secrets_or_paths() {
    let pods: Pods = Arc::default();
    let avail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (url, _hits): (_, Hits) = mock(stateful_runpod_with_create_status(
        Arc::clone(&pods),
        Arc::clone(&avail),
    ));
    let dir = temp("ts-skills");
    let (bin, fake) = install_fake_tools(&dir);

    let client = connect_tasks_client(&dir, &url, &bin, &fake, true, &[]).await;
    let peer = client.peer_info().expect("peer info");
    assert!(
        peer.capabilities.supports_tasks(),
        "tasks capability advertised"
    );
    assert!(
        peer.capabilities.supports_skills(),
        "skills capability advertised"
    );
    assert!(
        !peer.capabilities.supports_directory_read(),
        "directoryRead unnecessary for single static file skill"
    );
    assert!(peer.capabilities.resources.is_some(), "resources enabled");

    let listed = client.list_skills(None).await.expect("skills/list");
    assert_eq!(listed.skills.len(), 1);
    let skill = &listed.skills[0];
    assert_eq!(skill.name(), "offrig-session");
    assert_eq!(skill.uri, "skill://offrig/offrig-session/SKILL.md");
    skill
        .validate()
        .expect("skill entry passes SEP-2640 validation");

    let fetched = client
        .get_skill(GetSkillRequestParams::new(&skill.uri))
        .await
        .expect("skills/get");
    assert_eq!(fetched.skill, *skill);

    let bad_skill = client
        .get_skill(GetSkillRequestParams::new(
            "skill://offrig/missing/SKILL.md",
        ))
        .await
        .expect_err("unknown skill URI must fail");
    assert!(
        bad_skill.to_string().contains("-32602") || bad_skill.to_string().contains("unknown skill"),
        "{bad_skill}"
    );

    let SkillResources::Files(files) = &skill.resources else {
        panic!("expected static SkillResources::Files manifest, not Dynamic");
    };
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].uri, skill.uri);

    let res_list = client.list_resources(None).await.expect("resources/list");
    assert_eq!(res_list.resources.len(), 1);
    assert_eq!(res_list.resources[0].uri, skill.uri);

    let read_res = client
        .read_resource(ReadResourceRequestParams::new(&skill.uri))
        .await
        .expect("resources/read");
    let ResourceContents::TextResourceContents { text, .. } = &read_res.contents[0] else {
        panic!("expected text resource contents");
    };

    // Assert manifest digest & size match the repository SKILL.md bytes on disk.
    let disk_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("skills")
        .join("offrig-session")
        .join("SKILL.md");
    let disk_bytes = std::fs::read(&disk_path).expect("read SKILL.md from repo");
    assert_eq!(text.as_bytes(), disk_bytes.as_slice());
    files[0]
        .verify(&disk_bytes)
        .expect("skill digest and size must match SKILL.md on disk");

    // Verify unknown resource fails with RESOURCE_NOT_FOUND / INVALID_PARAMS depending on version.
    let rerr = client
        .read_resource(ReadResourceRequestParams::new(
            "skill://offrig/nope/SKILL.md",
        ))
        .await
        .expect_err("unknown resource refused");
    assert!(matches!(
        rerr,
        rmcp::ServiceError::McpError(ref e)
            if e.code == ErrorCode::RESOURCE_NOT_FOUND || e.code == ErrorCode::INVALID_PARAMS
    ));

    // Assert skill content holds procedure only using generic patterns (no absolute paths,
    // no secret-shaped tokens, and no hard-coded budget/lane values).
    let samples = [
        format!("path {}:\\{}\\someone\\x here", "C", "Users"),
        format!("path /{}/someone/x here", "home"),
        format!("path /{}/someone/x here", "Users"),
        "key rpa_0123456789abcdef0123456789".to_string(),
        "hex 0123456789abcdef0123456789abcdef".to_string(),
        "b64 QWxhZGRpbjpvcGVuIHNlc2FtZQxhZGRpbjpvcGVuIHNlc2FtZQo=".to_string(),
        "cap: $25.00".to_string(),
        "lane: prod-gpu".to_string(),
    ];
    for sample in &samples {
        assert!(
            check_skill_procedure_only(sample).is_err(),
            "generic leak check must reject sample {sample:?}"
        );
    }
    check_skill_procedure_only(text)
        .unwrap_or_else(|e| panic!("SKILL.md failed procedure-only check: {e}"));
    assert!(text.contains("offrig_plan"));
    assert!(text.contains("offrig_launch"));
    assert!(text.contains("offrig_shutdown"));
    assert!(
        !dir.join(".offrig").exists(),
        "skills/resources calls alone must not create .offrig"
    );

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}
