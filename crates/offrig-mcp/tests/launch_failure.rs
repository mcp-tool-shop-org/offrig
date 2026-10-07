//! Issue #25 end to end against a mock RunPod: a launch that stops says why in
//! `offrig_job` (the error's `code` and `retryable`, whether a pod was rented and what
//! it cost), a pod rented but never ready is `pod_not_ready` and plainly billed while a
//! launch that rented nothing stays `no_capacity`, `rented` is filled as soon as the pod
//! exists, and a `job_id` that is really a plan id is answered with the plan's job.
//!
//! No ssh endpoint is ever handed out, so no ssh config is touched; the home dir is a
//! temp dir anyway.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::{count, mock, temp};
use offrig_core::store::{NewPlan, Store};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

const RTX_S: &str = "NVIDIA RTX PRO 6000 Blackwell Server Edition";

type Client = RunningService<RoleClient, ()>;

fn graphql(body: &str, free: bool) -> String {
    if body.contains("myself") {
        return r#"{"data":{"myself":{"clientBalance":50.0,"currentSpendPerHr":0.0,"spendLimit":80}}}"#
            .into();
    }
    let price = if free { "2.09" } else { "null" };
    format!(
        r#"{{"data":{{"gpuTypes":[{{"id":"{RTX_S}","displayName":"RTX PRO 6000","memoryInGb":96,"secureCloud":true,"lowestPrice":{{"uninterruptablePrice":{price},"stockStatus":null}}}}]}}}}"#
    )
}

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

async fn start(dir: &std::path::Path, url: &str) -> Client {
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(dir);
        c.env("RUNPOD_API_KEY", "test-key");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        c.env("OFFRIG_TEST_HOME", dir.join("home"));
        c.env("OFFRIG_TEST_RUNPOD_BASE", url);
        c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
        c.env("OFFRIG_TEST_CAPACITY_POLL_MS", "100");
        // A pod that never gets an address is given up on after this long.
        c.env("OFFRIG_TEST_POD_READY_MS", "400");
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

fn pod_with_machine() -> String {
    format!(
        r#"{{"id":"p1","name":"offrig-x","desiredStatus":"RUNNING","costPerHr":2.09,"machine":{{"gpuTypeId":"{RTX_S}"}}}}"#
    )
}

const POD_BARE: &str =
    r#"{"id":"p1","name":"offrig-x","desiredStatus":"RUNNING","costPerHr":2.09}"#;

async fn until_job_state(client: &Client, plan_id: i64, states: &[&str]) -> Value {
    let started = Instant::now();
    loop {
        let j = body(&call(client, "offrig_job", json!({"plan_id": plan_id})).await);
        if states.contains(&j["state"].as_str().unwrap_or("")) {
            return j;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the job never reached {states:?}: {j}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn a_pod_that_never_got_ready_is_pod_not_ready_rented_billed_and_terminated() {
    let (url, hits) = mock(|route, b, _| match route {
        "POST /graphql" => (200, graphql(b, true)),
        "GET /pods" => (200, "[]".into()),
        // The create reply names the GPU and price; the pod never gets an ssh address.
        "POST /pods" | "GET /pods/p1" | "DELETE /pods/p1" => (200, pod_with_machine()),
        _ => (404, "{}".into()),
    });
    let dir = project("fail-not-ready");
    let client = start(&dir, &url).await;
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "no_fallback": true}),
    )
    .await;
    assert_eq!(p.is_error, Some(false), "{:?}", body(&p));
    let plan_id = body(&p)["plan_id"].as_i64().expect("plan id");
    let l = body(&call(&client, "offrig_launch", json!({"plan_id": plan_id})).await);
    let job_id = l["job_id"]
        .as_i64()
        .expect("launch replies with its job_id");
    let next = l["next_action"].as_str().expect("next_action");
    assert!(
        next.contains(&format!("\"job_id\": {job_id}")) && next.contains("not the plan's"),
        "{next}"
    );

    let j = until_job_state(&client, plan_id, &["failed"]).await;
    assert_eq!(j["job_id"], job_id);
    assert_eq!(j["code"], "pod_not_ready", "{j}");
    assert_eq!(j["retryable"], true);
    assert_eq!(j["pod_rented"], true);
    let f = &j["failure"];
    assert_eq!(f["code"], "pod_not_ready");
    assert_eq!(f["pod_rented"], true);
    assert_eq!(f["billed"], true);
    assert_eq!(f["pod_id"], "p1");
    assert_eq!(f["cost_per_hr"], 2.09);
    assert!(f["spent"].is_number(), "what was spent is recorded: {f}");
    assert_eq!(j["spent_so_far"], f["spent"], "{j}");
    assert!(
        f["pod_terminated"]
            .as_str()
            .is_some_and(|t| t.contains("terminated")),
        "{f}"
    );
    let err = j["error"].as_str().expect("error");
    assert!(err.contains("rented and billed"), "{err}");
    // What was rented survives the failure, filled in before ssh could be up.
    assert_eq!(j["rented"]["gpu"], RTX_S, "{j}");
    assert_eq!(j["rented"]["cost_per_hr"], 2.09);
    assert_eq!(j["progress"]["pod_id"], "p1");
    assert_eq!(j["progress"]["step"], "stopped");
    assert!(
        j["next_action"]
            .as_str()
            .is_some_and(|n| n.contains("rented and billed")),
        "{j}"
    );
    assert_eq!(j["plan_state"], "closed", "the pod was terminated: {j}");
    assert_eq!(count(&hits, "DELETE /pods/p1"), 1);
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_launch_that_rented_nothing_stays_no_capacity_with_nothing_spent() {
    let (url, hits) = mock(|route, b, _| match route {
        "POST /graphql" => (200, graphql(b, false)),
        "GET /pods" => (200, "[]".into()),
        _ => (404, "{}".into()),
    });
    let dir = project("fail-no-capacity");
    let db = dir.join(".offrig").join("offrig.db");
    let plan_id = {
        let s = Store::open(&db).expect("store");
        let plan = s
            .create_plan(NewPlan {
                profile: "job".into(),
                gpu_count: 1,
                gpu_types: vec![RTX_S.into()],
                max_hours: 1.0,
                max_price_hr: 2.09,
                note: None,
            })
            .expect("plan");
        s.set_plan_wait_minutes(plan.id, 0).expect("wait");
        plan.id
    };
    let client = start(&dir, &url).await;
    let l = body(&call(&client, "offrig_launch", json!({"plan_id": plan_id})).await);
    assert!(l["job_id"].is_i64(), "{l}");
    let j = until_job_state(&client, plan_id, &["failed"]).await;
    assert_eq!(j["code"], "no_capacity", "{j}");
    assert_eq!(j["retryable"], true);
    assert_eq!(j["pod_rented"], false);
    assert_eq!(j["failure"]["billed"], false);
    assert_eq!(j["failure"]["spent"], 0.0);
    assert!(j["failure"]["pod_id"].is_null());
    assert_eq!(j["spent_so_far"], 0.0);
    assert!(
        j["next_action"]
            .as_str()
            .is_some_and(|n| n.contains("nothing was rented")),
        "{j}"
    );
    assert_eq!(count(&hits, "POST /pods"), 0);
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rented_is_filled_when_the_pod_exists_even_if_only_a_later_read_names_the_gpu() {
    // The create reply names no GPU; a read of the pod does. The launch asks once.
    let named = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&named);
    let (url, hits) = mock(move |route, b, _| match route {
        "POST /graphql" => (200, graphql(b, true)),
        "GET /pods" => (200, "[]".into()),
        "POST /pods" => (200, POD_BARE.into()),
        "GET /pods/p1" => {
            seen.store(true, Ordering::SeqCst);
            (200, pod_with_machine())
        }
        "DELETE /pods/p1" => (200, POD_BARE.into()),
        _ => (404, "{}".into()),
    });
    let dir = project("fail-rented-early");
    let client = start(&dir, &url).await;
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "no_fallback": true}),
    )
    .await;
    let plan_id = body(&p)["plan_id"].as_i64().expect("plan id");
    call(&client, "offrig_launch", json!({"plan_id": plan_id})).await;
    // While the pod boots (running, no ssh yet) the rental is already on the job.
    let started = Instant::now();
    let j = loop {
        let j = body(&call(&client, "offrig_job", json!({"plan_id": plan_id})).await);
        if j["rented"]["gpu"].is_string() || j["state"] != "running" {
            break j;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "{j}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(j["rented"]["gpu"], RTX_S, "{j}");
    assert_eq!(j["rented"]["cost_per_hr"], 2.09);
    assert!(
        j["rented"]["cuda_version"].is_null(),
        "the CUDA measurement waits for ssh: {j}"
    );
    assert!(named.load(Ordering::SeqCst));
    let _ = until_job_state(&client, plan_id, &["failed"]).await;
    assert_eq!(count(&hits, "POST /pods"), 1);
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_job_id_that_is_really_a_plan_id_is_answered_with_the_plans_job() {
    let (url, _hits) = mock(|route, b, _| match route {
        "POST /graphql" => (200, graphql(b, true)),
        "GET /pods" => (200, "[]".into()),
        _ => (404, "{}".into()),
    });
    let dir = project("fail-job-id");
    let db = dir.join(".offrig").join("offrig.db");
    // Plans 1 and 2 exist; only plan 2 has a launch job, and it is job 1: so job_id 2
    // does not exist, while plan 2's job is job 1 (the shape of issue #25).
    let new_plan = |s: &Store| {
        s.create_plan(NewPlan {
            profile: "job".into(),
            gpu_count: 1,
            gpu_types: vec![RTX_S.into()],
            max_hours: 1.0,
            max_price_hr: 2.09,
            note: None,
        })
        .expect("plan")
        .id
    };
    let (unlaunched, launched, job) = {
        let s = Store::open(&db).expect("store");
        let a = new_plan(&s);
        let b = new_plan(&s);
        let j = s.create_job(b, "launch").expect("job");
        (a, b, j)
    };
    assert_eq!((unlaunched, launched, job), (1, 2, 1));
    let client = start(&dir, &url).await;

    // job_id 2 is plan 2: name its job and the call that works.
    let r = call(&client, "offrig_job", json!({"job_id": launched})).await;
    assert_eq!(r.is_error, Some(true));
    let b = body(&r);
    assert_eq!(b["code"], "not_found");
    let (err, next) = (
        b["error"].as_str().expect("error text"),
        b["next_action"].as_str().expect("next action"),
    );
    assert!(
        err.starts_with("no such job 2") && err.contains("not plan_id") && err.contains("job 1"),
        "{err}"
    );
    assert!(
        next.contains(&format!("{{\"plan_id\": {launched}}}")) && next.contains("\"job_id\": 1"),
        "{next}"
    );
    // And the suggested call works.
    let ok = call(&client, "offrig_job", json!({"plan_id": launched})).await;
    assert_eq!(ok.is_error, Some(false), "{:?}", body(&ok));
    assert_eq!(body(&ok)["job_id"], 1);

    // No job 3 and no plan 3 either: the plain answer.
    let r = call(&client, "offrig_job", json!({"job_id": 3})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(
        body(&r)["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("no such job 3")),
        "no plan 3 either: {:?}",
        body(&r)
    );
    // A plan that exists but was never launched is named as such.
    let third = new_plan(&Store::open(&db).expect("store"));
    assert_eq!(third, 3);
    let r = call(&client, "offrig_job", json!({"job_id": third})).await;
    assert!(
        body(&r)["error"]
            .as_str()
            .is_some_and(|e| e.contains("has not been launched")),
        "{:?}",
        body(&r)
    );
    assert!(
        body(&r)["next_action"]
            .as_str()
            .is_some_and(|n| n.contains("offrig_launch")),
        "{:?}",
        body(&r)
    );

    // A plan with no launch job, and a call with neither id.
    let r = call(&client, "offrig_job", json!({"plan_id": unlaunched})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(
        body(&r)["error"]
            .as_str()
            .is_some_and(|e| e.contains("no launch job for plan 1")),
        "{:?}",
        body(&r)
    );
    let r = call(&client, "offrig_job", json!({})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(
        body(&r)["error"]
            .as_str()
            .is_some_and(|e| e.contains("pass a job_id or a plan_id")),
        "{:?}",
        body(&r)
    );
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}
