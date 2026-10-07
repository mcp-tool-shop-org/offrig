//! Every tool failure is a structured result: `ok:false`, a stable `code`, the
//! message in `error`, the hint in `next_action` and whether a retry can help. Bad
//! input never takes the server down. Mock RunPod only; no network.

mod common;

use common::{mock, temp};
use offrig_core::store::Store;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

type Client = RunningService<RoleClient, ()>;

fn body(r: &CallToolResult) -> Value {
    r.structured_content
        .clone()
        .unwrap_or_else(|| panic!("no structured content: {r:?}"))
}

async fn start(dir: &std::path::Path, url: Option<&str>, key: Option<&str>) -> Client {
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(dir);
        match key {
            Some(k) => c.env("RUNPOD_API_KEY", k),
            None => c.env_remove("RUNPOD_API_KEY"),
        };
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        if let Some(u) = url {
            c.env("OFFRIG_TEST_RUNPOD_BASE", u);
        }
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

fn project(name: &str, cap: f64) -> std::path::PathBuf {
    let dir = temp(name);
    Store::open(&dir.join(".offrig").join("offrig.db"))
        .expect("store")
        .set_budget_cap(cap)
        .expect("cap");
    dir
}

/// The shape every failure shares.
fn assert_failure(r: &CallToolResult, code: &str, retryable: bool) {
    assert_eq!(r.is_error, Some(true), "{r:?}");
    let b = r
        .structured_content
        .clone()
        .unwrap_or_else(|| panic!("no structured content for {code}: {r:?}"));
    assert_eq!(b["ok"], false, "{b}");
    assert_eq!(b["code"], code, "{b}");
    assert_eq!(b["retryable"], retryable, "{b}");
    assert!(
        b["error"].as_str().is_some_and(|e| !e.is_empty()),
        "error message: {b}"
    );
    assert!(
        b["next_action"].as_str().is_some_and(|e| !e.is_empty()),
        "next_action: {b}"
    );
}

const GRAPHQL_GPUS: &str = r#"{"data":{"gpuTypes":[{"id":"NVIDIA RTX PRO 6000 Blackwell Server Edition","displayName":"RTX PRO 6000","memoryInGb":96,"secureCloud":true,"lowestPrice":{"uninterruptablePrice":2.09,"stockStatus":"High"}}]}}"#;

#[tokio::test]
async fn failures_carry_a_stable_code_and_retryability() {
    let (url, _hits) = mock(|route, _b, _| match route {
        "POST /graphql" => (200, GRAPHQL_GPUS.into()),
        _ => (404, "{}".into()),
    });
    let dir = project("error-codes", 5.0);
    let client = start(&dir, Some(&url), Some("test-key")).await;

    // Budget: the worst case of a long session exceeds the cap.
    let r = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 40.0}),
    )
    .await;
    assert_failure(&r, "budget_exceeded", false);

    // Unknown profile, unknown job, unknown plan: not_found.
    let r = call(
        &client,
        "offrig_plan",
        json!({"profile": "nope", "max_hours": 1.0}),
    )
    .await;
    assert_failure(&r, "not_found", false);
    let r = call(&client, "offrig_job", json!({"job_id": 999})).await;
    assert_failure(&r, "not_found", false);
    let r = call(&client, "offrig_launch", json!({"plan_id": 999})).await;
    assert_failure(&r, "not_found", false);

    // Out-of-range or empty input: invalid_input.
    let r = call(&client, "offrig_offers", json!({"gpu_count": 0})).await;
    assert_failure(&r, "invalid_input", false);
    let r = call(&client, "offrig_handoffs", json!({"action": "bogus"})).await;
    assert_failure(&r, "invalid_input", false);
    let r = call(
        &client,
        "offrig_ask",
        json!({"plan_id": 1, "handoff_id": 1, "instruction": ""}),
    )
    .await;
    assert_failure(&r, "invalid_input", false);

    // A memory record with a bad kind is refused by the store.
    let r = call(
        &client,
        "offrig_memory_record",
        json!({"kind": "gossip", "body": "x"}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(body(&r)["code"].is_string(), "{}", body(&r));

    // Success results carry no failure fields.
    let r = call(&client, "offrig_offers", json!({"gpu_count": 1})).await;
    assert_eq!(r.is_error, Some(false), "{:?}", r.structured_content);
    assert_eq!(body(&r)["ok"], true);
    assert!(body(&r).get("code").is_none());
    client.cancel().await.ok();
}

#[tokio::test]
async fn a_missing_key_and_a_flaky_runpod_have_their_own_codes() {
    // No RUNPOD_API_KEY: not retryable, the human must set it.
    let dir = project("error-codes-nokey", 5.0);
    let client = start(&dir, None, None).await;
    let r = call(&client, "offrig_offers", json!({"gpu_count": 1})).await;
    assert_failure(&r, "missing_api_key", false);
    client.cancel().await.ok();

    // RunPod answering 503: runpod_api, and worth retrying.
    let (url, _hits) = mock(|_, _, _| (503, "{}".into()));
    let dir = project("error-codes-503", 5.0);
    let client = start(&dir, Some(&url), Some("test-key")).await;
    let r = call(&client, "offrig_offers", json!({"gpu_count": 1})).await;
    assert_failure(&r, "runpod_api", true);
    client.cancel().await.ok();
}

#[tokio::test]
async fn bad_input_never_crashes_the_server() {
    let dir = project("error-codes-bad-input", 5.0);
    let client = start(&dir, None, Some("test-key")).await;

    // Wrong types, missing required fields, unknown tools: protocol errors or
    // structured failures, never a dead server.
    let cases: [(&'static str, Value); 6] = [
        ("offrig_offers", json!({"gpu_count": "four"})),
        ("offrig_offers", json!({})),
        ("offrig_plan", json!({"profile": 7})),
        ("offrig_handoffs", json!({"action": null})),
        ("offrig_job", json!({"job_id": "x"})),
        ("offrig_no_such_tool", json!({})),
    ];
    for (name, args) in cases {
        let r = client
            .call_tool(
                CallToolRequestParams::new(name)
                    .with_arguments(args.as_object().cloned().expect("obj")),
            )
            .await
            .unwrap_or_else(|e| panic!("{name}: a protocol error, not a result: {e}"));
        let code = if name == "offrig_no_such_tool" {
            "not_found"
        } else {
            "invalid_input"
        };
        assert_failure(&r, code, false);
    }

    // Still alive and answering.
    let r = call(&client, "offrig_job", json!({"job_id": 12345})).await;
    assert_failure(&r, "not_found", false);
    let r = call(&client, "offrig_status", json!({})).await;
    assert!(r.structured_content.is_some());
    client.cancel().await.ok();
}

#[test]
fn help_and_version_answer_without_serving() {
    for flag in ["--help", "--version"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp"))
            .arg(flag)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run offrig-mcp");
        assert_eq!(out.status.code(), Some(0), "{flag}");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.starts_with("offrig-mcp "), "{text}");
    }
}
