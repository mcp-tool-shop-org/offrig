//! Issue #6 and the container-disk need, end to end against a mock RunPod: `offrig_plan`
//! names the lane's ssh alias and the pod it will create, a plan's `container_disk_gb`
//! reaches the pod-create body as `disk` (the profile's size otherwise), and
//! a shutdown reports what it did to the lane's ssh block.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{mock, temp};
use offrig_core::store::Store;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

const RTX_S: &str = "NVIDIA RTX PRO 6000 Blackwell Server Edition";

fn graphql(body: &str) -> String {
    if body.contains("myself") {
        return r#"{"data":{"myself":{"clientBalance":50.0,"currentSpendPerHr":0.0,"spendLimit":80}}}"#
            .into();
    }
    format!(
        r#"{{"data":{{"gpuTypes":[{{"id":"{RTX_S}","displayName":"RTX PRO 6000","memoryInGb":96,"secureCloud":true,"lowestPrice":{{"uninterruptablePrice":2.09,"stockStatus":"High"}}}}]}}}}"#
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

#[tokio::test]
async fn offrig_plan_names_the_alias_and_the_pod_and_status_agrees() {
    let (url, _hits) = mock(|route, b, _| match route {
        "POST /graphql" => (200, graphql(b)),
        "GET /pods" => (200, "[]".into()),
        _ => (404, "{}".into()),
    });
    let dir = project("lane-names");
    let client = start(&dir, &url).await;
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "no_fallback": true}),
    )
    .await;
    assert_eq!(p.is_error, Some(false), "{:?}", body(&p));
    let b = body(&p);
    let tag = b["lane"].as_str().expect("lane tag").to_string();
    assert_eq!(b["ssh_alias"], format!("offrig-{tag}"));
    assert_eq!(b["pod_name"], format!("offrig-{tag}-job"));
    assert_eq!(b["container_disk_gb"], 60, "the job profile's own size");
    assert_eq!(b["container_disk_priced"], false);

    // A different profile names a different pod in the same lane, under the same alias.
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "jam", "max_hours": 1.0}),
    )
    .await;
    // (the mock lists only the RTX card, so jam may be refused for lack of a price)
    if p.is_error != Some(true) {
        assert_eq!(body(&p)["pod_name"], format!("offrig-{tag}-jam"));
        assert_eq!(body(&p)["ssh_alias"], format!("offrig-{tag}"));
    }

    // Status shows the same alias and pod name for an open plan.
    let plan_id = b["plan_id"].as_i64().expect("plan id");
    {
        let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
        s.commit_plan(plan_id).expect("commit");
    }
    let st = body(&call(&client, "offrig_status", json!({})).await);
    let open = st["open_plans"].as_array().expect("open plans");
    let mine = open
        .iter()
        .find(|x| x["plan_id"] == plan_id)
        .expect("the open plan");
    assert_eq!(mine["ssh_alias"], format!("offrig-{tag}"));
    assert_eq!(mine["pod_name"], format!("offrig-{tag}-job"));
    assert_eq!(mine["container_disk_gb"], 60);
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_plans_container_disk_reaches_the_create_body() {
    let created: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = Arc::clone(&created);
    // The pod's name is the lane's, which the test learns from the plan's reply.
    let pod_name: Arc<Mutex<String>> = Arc::default();
    let name = Arc::clone(&pod_name);
    let (url, _hits) = mock(move |route, b, _| {
        let pod = |id: &str| {
            format!(
                r#"{{"id":"{id}","name":"{}","desiredStatus":"RUNNING","costPerHr":2.09}}"#,
                name.lock().expect("name")
            )
        };
        match route {
            "POST /graphql" => (200, graphql(b)),
            "GET /pods" => (200, "[]".into()),
            "POST /pods" => {
                log.lock().expect("log").push(b.to_string());
                (200, pod("p1"))
            }
            "GET /pods/p1" | "DELETE /pods/p1" => (200, pod("p1")),
            _ => (404, "{}".into()),
        }
    });
    let dir = project("disk-override");
    let client = start(&dir, &url).await;

    // Out-of-range sizes are refused and write no plan.
    for bad in [0, 5000] {
        let p = call(
            &client,
            "offrig_plan",
            json!({"profile": "job", "max_hours": 1.0, "container_disk_gb": bad}),
        )
        .await;
        assert_eq!(p.is_error, Some(true), "{bad}");
        assert!(
            body(&p)["error"]
                .as_str()
                .is_some_and(|e| e.contains("container_disk_gb")),
            "{:?}",
            body(&p)
        );
    }
    let committed_before = Store::open(&dir.join(".offrig").join("offrig.db"))
        .expect("store")
        .budget()
        .expect("budget")
        .committed;
    assert_eq!(committed_before, 0.0);

    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "no_fallback": true, "container_disk_gb": 400}),
    )
    .await;
    assert_eq!(p.is_error, Some(false), "{:?}", body(&p));
    assert_eq!(body(&p)["container_disk_gb"], 400);
    let worst_with = body(&p)["worst_case"].clone();
    *pod_name.lock().expect("name") = body(&p)["pod_name"].as_str().expect("name").to_string();
    let plan_id = body(&p)["plan_id"].as_i64().expect("plan id");
    // The disk is not priced: the same plan without it has the same worst case.
    let q = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "no_fallback": true}),
    )
    .await;
    assert_eq!(body(&q)["worst_case"], worst_with);

    let l = call(&client, "offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(l.is_error, Some(false), "{:?}", body(&l));
    let started = Instant::now();
    let sent = loop {
        if let Some(s) = created.lock().expect("log").first().cloned() {
            break s;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "no pod create was sent"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let sent: Value = serde_json::from_str(&sent).expect("create body");
    assert_eq!(sent["disk"], 400, "{sent}");
    assert_eq!(
        sent["mounts"]["persistent"]["size"], 200,
        "the volume is the profile's own: {sent}"
    );
    assert!(sent.get("containerDiskInGb").is_none(), "{sent}");
    assert!(sent.get("volumeInGb").is_none(), "{sent}");

    // Shutdown reports the ssh block it removed (none here: the pod never had an address).
    let s = call(&client, "offrig_shutdown", json!({"plan_id": plan_id})).await;
    assert_eq!(s.is_error, Some(false), "{:?}", body(&s));
    assert_eq!(body(&s)["ssh_block_removed"], false);
    client.cancel().await.expect("shutdown");

    // A plan with no override sends the profile's size.
    let created2: Arc<Mutex<Vec<String>>> = Arc::default();
    let log2 = Arc::clone(&created2);
    let name2: Arc<Mutex<String>> = Arc::default();
    let name = Arc::clone(&name2);
    let (url2, _h) = mock(move |route, b, _| {
        let pod = |id: &str| {
            format!(
                r#"{{"id":"{id}","name":"{}","desiredStatus":"RUNNING","costPerHr":2.09}}"#,
                name.lock().expect("name")
            )
        };
        match route {
            "POST /graphql" => (200, graphql(b)),
            "GET /pods" => (200, "[]".into()),
            "POST /pods" => {
                log2.lock().expect("log").push(b.to_string());
                (200, pod("p2"))
            }
            "GET /pods/p2" | "DELETE /pods/p2" => (200, pod("p2")),
            _ => (404, "{}".into()),
        }
    });
    let dir2 = project("disk-default");
    let client = start(&dir2, &url2).await;
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "no_fallback": true}),
    )
    .await;
    let plan_id = body(&p)["plan_id"].as_i64().expect("plan id");
    *name2.lock().expect("name") = body(&p)["pod_name"].as_str().expect("name").to_string();
    let l = call(&client, "offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(l.is_error, Some(false), "{:?}", body(&l));
    let started = Instant::now();
    let sent = loop {
        if let Some(s) = created2.lock().expect("log").first().cloned() {
            break s;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "no pod create was sent"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let sent: Value = serde_json::from_str(&sent).expect("create body");
    assert_eq!(sent["disk"], 60, "{sent}");
    let _ = call(&client, "offrig_shutdown", json!({"plan_id": plan_id})).await;
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}
