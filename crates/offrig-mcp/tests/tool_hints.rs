//! Every tool the side-car advertises names all four behaviour hints, and the
//! values stay the ones agreed for this server. A new tool with a missing hint
//! fails the first test. A change to any value fails the second.

use rmcp::ServiceExt;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};

/// read-only, destructive, idempotent, open-world. The order is the server's.
const HINTS: &[(&str, bool, bool, bool, bool)] = &[
    ("offrig_status", false, false, true, true),
    ("offrig_offers", true, false, true, true),
    ("offrig_plan", false, false, false, true),
    ("offrig_memory_search", true, false, true, false),
    ("offrig_memory_record", false, false, false, false),
    ("offrig_handoffs", false, true, false, false),
    ("offrig_launch", false, false, true, true),
    ("offrig_run", false, true, false, true),
    ("offrig_job", true, false, true, true),
    ("offrig_ask", false, false, false, true),
    ("offrig_exec", false, true, false, true),
    ("offrig_put", false, true, true, true),
    ("offrig_get", false, true, true, true),
    ("offrig_shutdown", false, true, true, true),
    ("offrig_complete", false, true, false, true),
];

async fn advertised() -> Vec<rmcp::model::Tool> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("offrig-mcp-hints-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(&dir);
        c.env_remove("RUNPOD_API_KEY");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
    });
    let client = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");
    let tools = client.list_all_tools().await.expect("list");
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
    tools
}

#[tokio::test]
async fn every_advertised_tool_has_four_boolean_hints() {
    let tools = advertised().await;
    assert!(!tools.is_empty(), "the server advertised no tools");
    for tool in &tools {
        let name = tool.name.to_string();
        let Some(ann) = tool.annotations.as_ref() else {
            panic!("{name} has no annotations");
        };
        assert!(
            ann.title.as_ref().is_some_and(|t| !t.is_empty()),
            "{name} is missing its title"
        );
        for (label, value) in [
            ("readOnlyHint", ann.read_only_hint),
            ("destructiveHint", ann.destructive_hint),
            ("idempotentHint", ann.idempotent_hint),
            ("openWorldHint", ann.open_world_hint),
        ] {
            assert!(
                value.is_some(),
                "{name} {label} is missing or not a boolean"
            );
        }
    }
}

#[tokio::test]
async fn tool_hints_match_the_pinned_table() {
    let tools = advertised().await;
    let mut names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    let mut want: Vec<String> = HINTS.iter().map(|(name, ..)| (*name).to_string()).collect();
    want.sort();
    assert_eq!(names, want, "the server's tool list");
    for (name, read_only, destructive, idempotent, open_world) in HINTS {
        let Some(tool) = tools.iter().find(|t| t.name.as_ref() == *name) else {
            panic!("missing {name}");
        };
        let Some(ann) = tool.annotations.as_ref() else {
            panic!("{name} has no annotations");
        };
        assert_eq!(
            (
                ann.read_only_hint,
                ann.destructive_hint,
                ann.idempotent_hint,
                ann.open_world_hint,
            ),
            (
                Some(*read_only),
                Some(*destructive),
                Some(*idempotent),
                Some(*open_world),
            ),
            "{name}"
        );
    }
}
