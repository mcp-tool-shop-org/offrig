//! End to end over stdio: spawn the real offrig-mcp binary and drive it with rmcp's
//! client, the way Claude Code does. No network: RUNPOD_API_KEY is removed, so the
//! tools that need RunPod must fail with an actionable result, not a crash.

use offrig_core::store::Store;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

fn args(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().cloned().expect("an object")
}

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

#[tokio::test]
async fn the_sidecar_works_end_to_end_over_stdio() {
    let dir = std::env::temp_dir().join(format!("offrig-mcp-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    // The human sets the cap outside the tools.
    Store::open(&dir.join(".offrig").join("offrig.db"))
        .expect("store")
        .set_budget_cap(15.0)
        .expect("cap");

    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(&dir);
        c.env_remove("RUNPOD_API_KEY");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
    });
    let client = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");

    // A small, fixed tool set; no tool can raise the budget.
    let names: Vec<String> = client
        .list_all_tools()
        .await
        .expect("list")
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    for want in [
        "offrig_status",
        "offrig_offers",
        "offrig_plan",
        "offrig_memory_search",
        "offrig_memory_record",
        "offrig_handoffs",
    ] {
        assert!(
            names.contains(&want.to_string()),
            "missing {want}: {names:?}"
        );
    }
    assert!(
        names.iter().all(|n| !n.contains("budget")),
        "no budget-setting tool: {names:?}"
    );

    let call = |name: &'static str, a: Value| {
        let client = &client;
        async move {
            client
                .call_tool(CallToolRequestParams::new(name).with_arguments(args(a)))
                .await
                .expect("call")
        }
    };

    // Memory: record, supersede, search returns only the active record.
    let r = call(
        "offrig_memory_record",
        json!({"kind": "decision", "body": "combat uses a speed stat", "author": "test"}),
    )
    .await;
    let old = body(&r)["id"].as_i64().expect("id");
    let bad = call(
        "offrig_memory_record",
        json!({"kind": "decision", "body": "bands", "author": "test", "supersedes": old}),
    )
    .await;
    assert_eq!(
        bad.is_error,
        Some(true),
        "supersede without a reason is refused"
    );
    assert!(
        body(&bad)["next_action"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    let r = call(
        "offrig_memory_record",
        json!({"kind": "decision", "body": "combat uses initiative bands", "author": "test", "supersedes": old, "reason": "reads better", "source": "playtest 3"}),
    )
    .await;
    let new = body(&r)["id"].as_i64().expect("id");
    let s = call("offrig_memory_search", json!({"query": "combat"})).await;
    let ids: Vec<i64> = body(&s)["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|x| x["id"].as_i64())
        .collect();
    assert_eq!(ids, [new]);
    assert_eq!(body(&s)["results"][0]["source"], "playtest 3");

    // Handoffs: roles, preview, add (acceptance required), list.
    let roles = call("offrig_handoffs", json!({"action": "roles"})).await;
    assert!(
        body(&roles)["roles"]
            .as_array()
            .expect("roles")
            .iter()
            .any(|r| r == "game-designer")
    );
    let p = call(
        "offrig_handoffs",
        json!({"action": "preview", "role": "game-designer"}),
    )
    .await;
    let block = body(&p)["block"].as_str().expect("block").to_string();
    assert!(
        block.contains("## Role: Game Designer") && block.contains("Widen your range"),
        "{block}"
    );
    let no_check = call(
        "offrig_handoffs",
        json!({"action": "add", "role": "game-designer", "mission": "design the standoff"}),
    )
    .await;
    assert_eq!(no_check.is_error, Some(true));
    let bad_role = call(
        "offrig_handoffs",
        json!({"action": "add", "role": "wizard", "mission": "m", "acceptance": "a"}),
    )
    .await;
    assert_eq!(bad_role.is_error, Some(true));
    let added = call(
        "offrig_handoffs",
        json!({"action": "add", "role": "game-designer", "mission": "design the standoff duel", "acceptance": "spec lists verbs, feedback and failure states", "scope": ["docs/standoff.md"]}),
    )
    .await;
    assert_eq!(added.is_error, Some(false), "{:?}", body(&added));
    let list = call("offrig_handoffs", json!({"action": "list"})).await;
    assert_eq!(body(&list)["handoffs"][0]["state"], "pending");
    assert_eq!(body(&list)["handoffs"][0]["stale"], false);

    // Status works without RunPod and reports the budget the human set.
    let st = call("offrig_status", json!({})).await;
    assert_eq!(st.is_error, Some(false));
    assert_eq!(body(&st)["budget"]["cap"], 15.0);
    assert_eq!(body(&st)["handoffs"]["pending"], 1);
    assert!(
        body(&st)["runpod"]["note"]
            .as_str()
            .is_some_and(|n| n.contains("RUNPOD_API_KEY"))
    );

    // Plan needs live prices; without the key it fails with something to do next.
    let plan = call(
        "offrig_plan",
        json!({"profile": "frontier", "max_hours": 1.5}),
    )
    .await;
    assert_eq!(plan.is_error, Some(true));
    assert!(
        body(&plan)["error"]
            .as_str()
            .is_some_and(|e| e.contains("RUNPOD_API_KEY"))
    );
    let unknown = call("offrig_plan", json!({"profile": "huge", "max_hours": 1.0})).await;
    assert!(
        body(&unknown)["next_action"]
            .as_str()
            .is_some_and(|n| n.contains("frontier"))
    );

    client.cancel().await.expect("shutdown");

    // Starting the side-car in a folder and listing its tools (what a health check
    // does) must not create a database there.
    let bare = dir.join("untouched");
    std::fs::create_dir_all(&bare).expect("dir");
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(&bare);
        c.env_remove("RUNPOD_API_KEY");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
    });
    let probe = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");
    probe.list_all_tools().await.expect("list");
    probe.cancel().await.expect("shutdown");
    assert!(
        !bare.join(".offrig").exists(),
        "a health check must not leave .offrig/ behind"
    );
    // Nor may starting a side-car claim a lane: a lane is allocated by the first plan.
    assert!(
        !dir.join("cfg").join("lanes.toml").exists(),
        "starting a side-car must not allocate a lane"
    );
    // The memory outlives the server.
    let reopened = Store::open(&dir.join(".offrig").join("offrig.db")).expect("reopen");
    assert_eq!(reopened.handoffs().expect("list").len(), 1);
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
}
