//! `offrig_complete` end to end against a mock OpenRouter: the project and model
//! allow-lists, the budget refusal, the ledger write, the charge of a failed stream,
//! the overrun warning, and that the key reaches no file. No real call is ever made.

mod common;

use common::{Hits, count, mock, temp};
use offrig_core::lanes::project_key;
use offrig_core::store::Store;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

const KEY: &str = "sk-or-v1-mock-0c7e55d1a9";
const CHAT: &str = "POST /chat/completions";

type Client = RunningService<RoleClient, ()>;

const ENDPOINTS: &str = r#"{"data":{"endpoints":[
    {"provider_name":"Relace","pricing":{"prompt":"0.00000067","completion":"0.000013"}},
    {"provider_name":"InferenceNet","pricing":{"prompt":"0.00000072","completion":"0.000015"}}]}}"#;

fn sse(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| format!("data: {p}\n\n"))
        .collect::<String>()
}

/// A good stream charging `cost`.
fn good(cost: f64) -> String {
    sse(&[
        r#"{"id":"gen-ok","provider":"InferenceNet","choices":[{"delta":{"reasoning":"Voice the alto."}}]}"#.into(),
        r#"{"id":"gen-ok","choices":[{"delta":{"content":"X:1\nT:Amazing Grace"},"finish_reason":"stop"}]}"#.into(),
        format!(
            r#"{{"id":"gen-ok","choices":[],"usage":{{"prompt_tokens":1700,"completion_tokens":19500,"completion_tokens_details":{{"reasoning_tokens":12000}},"cost":{cost}}}}}"#
        ),
        "[DONE]".into(),
    ])
}

fn openrouter(chat: String) -> (String, Hits) {
    mock(move |route, _, _| {
        match route {
        "GET /models/moonshotai/kimi-k3/endpoints" => (200, ENDPOINTS.into()),
        CHAT => (200, chat.clone()),
        "GET /generation" => (
            200,
            r#"{"data":{"total_cost":0.05,"tokens_prompt":1700,"tokens_completion":3000,"provider_name":"Relace"}}"#
                .into(),
        ),
        _ => (404, "{}".into()),
    }
    })
}

/// A project with a budget, registered as the ai-jam-sessions lane when `lane` is given.
fn project(name: &str, cap: f64, lane: Option<&str>) -> std::path::PathBuf {
    let dir = temp(name);
    let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
    s.set_budget_cap(cap).expect("cap");
    std::fs::create_dir_all(dir.join("cfg")).expect("cfg");
    if let Some(tag) = lane {
        std::fs::write(
            dir.join("cfg").join("lanes.toml"),
            format!(
                "[[lane]]\nproject = {:?}\ntag = {tag:?}\nssh_alias = \"offrig-{tag}\"\ntunnel_port = 11500\n",
                project_key(&dir)
            ),
        )
        .expect("lanes.toml");
    }
    std::fs::create_dir_all(dir.join("briefs")).expect("briefs");
    std::fs::write(
        dir.join("briefs").join("grace.md"),
        "Amazing Grace, SATB, G major.",
    )
    .expect("brief");
    dir
}

async fn start(dir: &std::path::Path, url: &str) -> Client {
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(dir);
        c.env("OPENROUTER_API_KEY", KEY);
        c.env("OFFRIG_TEST_OPENROUTER_BASE", url);
        c.env("OFFRIG_TEST_OPENROUTER_POLL_MS", "10");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        c.env_remove("RUNPOD_API_KEY");
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

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

fn ask(extra: Value) -> Value {
    let mut a = json!({
        "model": "moonshotai/kimi-k3",
        "system": "You arrange hymns for piano.",
        "user_files": ["briefs/grace.md"],
        "max_tokens": 200000,
        "reasoning_effort": "high",
        "temperature": 0.0,
    });
    if let (Some(m), Value::Object(e)) = (a.as_object_mut(), extra) {
        m.extend(e);
    }
    a
}

fn budget(dir: &std::path::Path) -> offrig_core::store::Budget {
    Store::open(&dir.join(".offrig").join("offrig.db"))
        .expect("store")
        .budget()
        .expect("budget")
}

#[tokio::test]
async fn a_completion_writes_its_answer_records_the_charge_and_leaks_no_key() {
    let dir = project("or-ok", 4.50, Some("ai-jam-sessions"));
    let (url, hits) = openrouter(good(0.30));
    let client = start(&dir, &url).await;

    let r = call(
        &client,
        "offrig_complete",
        ask(json!({"out": "arrangements/grace.md"})),
    )
    .await;
    let v = body(&r);
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["path"], "arrangements/grace.md");
    assert_eq!(v["reasoning_path"], "arrangements/grace.md.reasoning.md");
    assert_eq!(v["cost"], 0.3);
    assert_eq!(v["cost_source"], "usage");
    assert_eq!(v["provider"], "InferenceNet");
    assert_eq!(
        v["worst_case"], 3.01,
        "200k out at the dearest $15/M, plus input"
    );
    assert_eq!(v["usage"]["reasoning_tokens"], 12000);
    assert_eq!(v["budget"]["remaining"], 4.2);
    assert_eq!(count(&hits, CHAT), 1);

    let answer =
        std::fs::read_to_string(dir.join("arrangements").join("grace.md")).expect("answer");
    assert_eq!(answer, "X:1\nT:Amazing Grace");
    let reasoning =
        std::fs::read_to_string(dir.join("arrangements").join("grace.md.reasoning.md")).expect("r");
    assert_eq!(reasoning, "Voice the alto.");

    let b = budget(&dir);
    assert_eq!(b.committed, 0.0, "the commitment is released");
    assert!(
        (b.spent - 0.30).abs() < 1e-9,
        "the real charge is the actual"
    );

    let status = body(&call(&client, "offrig_status", json!({})).await);
    let recent = &status["openrouter_completions"]["recent"][0];
    assert_eq!(recent["state"], "done");
    assert_eq!(recent["path"], "arrangements/grace.md");
    assert_eq!(status["unfinished_side_effects"], 0);
    client.cancel().await.expect("stop");

    // The key is in no file the call wrote, not in the store and not in the reply.
    let db = std::fs::read(dir.join(".offrig").join("offrig.db")).expect("db");
    let mut wal = dir.join(".offrig").join("offrig.db").into_os_string();
    wal.push("-wal");
    let wal = std::fs::read(wal).unwrap_or_default();
    let key = KEY.as_bytes();
    for (what, bytes) in [
        ("answer", answer.as_bytes()),
        ("reasoning", reasoning.as_bytes()),
        ("store", db.as_slice()),
        ("store wal", wal.as_slice()),
    ] {
        assert!(
            !bytes.windows(key.len()).any(|w| w == key),
            "key in the {what}"
        );
    }
    assert!(!v.to_string().contains(KEY) && !status.to_string().contains(KEY));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_worst_case_over_the_budget_is_refused_before_any_call() {
    let dir = project("or-budget", 1.00, Some("ai-jam-sessions"));
    let (url, hits) = openrouter(good(0.30));
    let client = start(&dir, &url).await;
    let v = body(&call(&client, "offrig_complete", ask(json!({}))).await);
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], "budget_exceeded", "{v}");
    assert!(
        v["error"].as_str().is_some_and(|e| e.contains("$3.01")),
        "{v}"
    );
    assert_eq!(count(&hits, CHAT), 0, "no money moved");
    let b = budget(&dir);
    assert_eq!((b.committed, b.spent), (0.0, 0.0));

    // A smaller cap on generated tokens fits.
    let v = body(
        &call(
            &client,
            "offrig_complete",
            ask(json!({"max_tokens": 20000})),
        )
        .await,
    );
    assert_eq!(v["ok"], true, "{v}");
    client.cancel().await.expect("stop");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn only_approved_models_projects_and_project_files_are_accepted() {
    let dir = project("or-allow", 10.0, Some("ai-jam-sessions"));
    let (url, hits) = openrouter(good(0.30));
    let client = start(&dir, &url).await;
    // A real file beside the project: readable, but not the project's.
    let outside = dir
        .parent()
        .expect("parent")
        .join(format!("offrig-or-outside-{}.md", std::process::id()));
    std::fs::write(&outside, "secret").expect("outside file");
    let outside = outside.to_string_lossy().into_owned();
    for (extra, needle) in [
        (json!({"model": "moonshotai/kimi-k2"}), "not approved"),
        (json!({"model": "kimi-k3:cloud"}), "not approved"),
        (json!({"user_files": [outside]}), "outside the project"),
        (
            json!({"system": null, "system_file": outside}),
            "outside the project",
        ),
        (json!({"out": "../escape.md"}), "inside the project"),
        (json!({"reasoning_effort": "max"}), "reasoning_effort"),
    ] {
        let v = body(&call(&client, "offrig_complete", ask(extra.clone())).await);
        assert_eq!(v["ok"], false, "{extra}");
        assert_eq!(v["code"], "refused", "{extra}: {v}");
        assert!(
            v["error"].as_str().is_some_and(|e| e.contains(needle)),
            "{extra}: {v}"
        );
    }
    assert!(
        hits.lock().expect("hits").is_empty(),
        "nothing was sent anywhere"
    );
    client.cancel().await.expect("stop");
    let _ = std::fs::remove_file(&outside);
    let _ = std::fs::remove_dir_all(&dir);

    // Another project's lane, and a project with no lane at all, are refused; the
    // refusal allocates no lane.
    for (name, lane) in [("or-other", Some("aspire-si")), ("or-nolane", None)] {
        let dir = project(name, 10.0, lane);
        let client = start(&dir, &url).await;
        let v = body(&call(&client, "offrig_complete", ask(json!({}))).await);
        assert_eq!(v["code"], "refused", "{name}: {v}");
        assert!(
            v["error"]
                .as_str()
                .is_some_and(|e| e.contains("ai-jam-sessions")),
            "{v}"
        );
        client.cancel().await.expect("stop");
        assert_eq!(dir.join("cfg").join("lanes.toml").exists(), lane.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(hits.lock().expect("hits").is_empty());
}

#[tokio::test]
async fn a_failed_stream_records_what_openrouter_charged() {
    let dir = project("or-fail", 4.50, Some("ai-jam-sessions"));
    let broken = sse(&[
        r#"{"id":"gen-bad","provider":"Relace","choices":[{"delta":{"content":"X:1"}}]}"#.into(),
        r#"{"id":"gen-bad","error":{"code":502,"message":"upstream died"},"choices":[]}"#.into(),
    ]);
    let (url, hits) = openrouter(broken);
    let client = start(&dir, &url).await;
    let v = body(&call(&client, "offrig_complete", ask(json!({}))).await);
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["failed"], true);
    assert_eq!(v["generation_id"], "gen-bad");
    assert_eq!(v["cost"], 0.05);
    assert_eq!(v["cost_source"], "generation");
    assert!(
        v["charge"].as_str().is_some_and(|c| c.contains("$0.0500")),
        "{v}"
    );
    assert_eq!(count(&hits, "GET /generation"), 1);
    let b = budget(&dir);
    assert_eq!(b.committed, 0.0);
    assert!((b.spent - 0.05).abs() < 1e-9);
    client.cancel().await.expect("stop");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_charge_over_the_worst_case_warns_and_is_recorded_as_charged() {
    let dir = project("or-over", 10.0, Some("ai-jam-sessions"));
    let (url, _) = openrouter(good(5.0));
    let client = start(&dir, &url).await;
    let v = body(&call(&client, "offrig_complete", ask(json!({}))).await);
    assert_eq!(v["ok"], true, "{v}");
    assert!(
        v["warnings"][0]
            .as_str()
            .is_some_and(|w| w.starts_with("WARNING") && w.contains("$5.0000")),
        "{v}"
    );
    assert!(
        (budget(&dir).spent - 5.0).abs() < 1e-9,
        "never rounded down to the worst case"
    );
    client.cancel().await.expect("stop");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn without_the_key_nothing_is_committed() {
    let dir = project("or-nokey", 10.0, Some("ai-jam-sessions"));
    let (url, hits) = openrouter(good(0.30));
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(&dir);
        c.env_remove("OPENROUTER_API_KEY");
        c.env("OFFRIG_TEST_OPENROUTER_BASE", &url);
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
    });
    let client = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");
    let v = body(&call(&client, "offrig_complete", ask(json!({}))).await);
    assert_eq!(v["code"], "missing_api_key", "{v}");
    assert!(hits.lock().expect("hits").is_empty());
    assert_eq!(budget(&dir).committed, 0.0);
    client.cancel().await.expect("stop");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_unread_charge_is_held_then_settled_and_a_refused_call_costs_nothing() {
    let dir = project("or-held", 10.0, Some("ai-jam-sessions"));
    // The chat call answers 402 first (nothing generated), then a stream that breaks;
    // the generation lookup does not know the charge for its first five asks.
    let broken = sse(&[
        r#"{"id":"gen-held","choices":[{"delta":{"content":"X"}}]}"#.into(),
        r#"{"id":"gen-held","error":{"code":500,"message":"gone"},"choices":[]}"#.into(),
    ]);
    let (url, hits) = mock(move |route, _, nth| match (route, nth) {
        ("GET /models/moonshotai/kimi-k3/endpoints", _) => (200, ENDPOINTS.into()),
        (CHAT, 0) => (
            402,
            r#"{"error":{"message":"insufficient credits"}}"#.into(),
        ),
        (CHAT, 1) => (200, broken.clone()),
        (CHAT, _) => (200, good(0.30)),
        ("GET /generation", n) if n < 5 => (404, "{}".into()),
        ("GET /generation", _) => (
            200,
            r#"{"data":{"total_cost":0.07,"tokens_prompt":10,"tokens_completion":20}}"#.into(),
        ),
        _ => (404, "{}".into()),
    });
    let client = start(&dir, &url).await;

    let v = body(&call(&client, "offrig_complete", ask(json!({}))).await);
    assert_eq!(v["cost_source"], "none", "{v}");
    assert_eq!(v["cost"], 0.0);
    assert_eq!(v["code"], "openrouter_api");
    assert_eq!(
        count(&hits, "GET /generation"),
        0,
        "no generation to ask about"
    );

    let v = body(&call(&client, "offrig_complete", ask(json!({}))).await);
    assert_eq!(v["cost_source"], "unread", "{v}");
    assert!(v["cost"].is_null());
    assert!(
        v["charge"]
            .as_str()
            .is_some_and(|c| c.contains("stays committed"))
    );
    let b = budget(&dir);
    assert!((b.committed - 3.01).abs() < 1e-9, "held: {b:?}");
    let status = body(&call(&client, "offrig_status", json!({})).await);
    assert_eq!(
        status["openrouter_completions"]["held"][0]["generation_id"],
        "gen-held"
    );

    // Once the held commitment is an hour old, the next call settles it first.
    rusqlite_open(&dir)
        .execute_batch(
            "UPDATE completions SET created_at = created_at - 7200 WHERE generation_id = 'gen-held';",
        )
        .expect("age it");
    let v = body(&call(&client, "offrig_complete", ask(json!({}))).await);
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["held"], json!([]), "settled before the new call");
    let b = budget(&dir);
    assert_eq!(b.committed, 0.0);
    assert!((b.spent - 0.37).abs() < 1e-9, "0.07 settled + 0.30: {b:?}");
    client.cancel().await.expect("stop");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_charge_missing_from_the_stream_is_asked_for_and_a_cut_answer_warns() {
    let dir = project("or-nocost", 10.0, Some("ai-jam-sessions"));
    let stream = sse(&[
        r#"{"id":"gen-len","provider":"Wafer","choices":[{"delta":{"reasoning":"long thought"},"finish_reason":"length"}]}"#.into(),
        r#"{"id":"gen-len","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":100}}"#.into(),
        "[DONE]".into(),
    ]);
    let (url, hits) = openrouter(stream);
    let client = start(&dir, &url).await;
    let v = body(
        &call(
            &client,
            "offrig_complete",
            ask(
                json!({"user": "Also: G major.", "system": null, "system_file": "briefs/grace.md"}),
            ),
        )
        .await,
    );
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["cost_source"], "generation");
    assert_eq!(v["cost"], 0.05);
    assert_eq!(count(&hits, "GET /generation"), 1);
    let w = v["warnings"].to_string();
    assert!(w.contains("cut short") && w.contains("empty"), "{w}");
    assert_eq!(
        v["path"],
        format!(".offrig/out/completion-{}.md", v["completion_id"])
    );
    client.cancel().await.expect("stop");
    let _ = std::fs::remove_dir_all(&dir);
}

fn rusqlite_open(dir: &std::path::Path) -> rusqlite::Connection {
    rusqlite::Connection::open(dir.join(".offrig").join("offrig.db")).expect("open db")
}
