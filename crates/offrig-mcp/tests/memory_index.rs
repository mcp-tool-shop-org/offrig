//! Memory search modes over stdio: keyword before an index exists, hybrid after, an
//! explicit hybrid with no index refused, and a dead embed server reported, not hidden.
//! The embed server is a mock; nothing here touches a real Ollama or a GPU.

mod common;

use offrig_core::index;
use offrig_core::ollama::Ollama;
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

fn ids(r: &CallToolResult) -> Vec<i64> {
    body(r)["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|x| x["id"].as_i64())
        .collect()
}

/// Axis 0 for "alpha" or "first letter"; the third axis keeps every vector non-zero.
fn embed(_: &str, body: &str, _: usize) -> (u16, String) {
    let v: Value = serde_json::from_str(body).expect("json");
    let rows: Vec<Vec<f64>> = v["input"]
        .as_array()
        .expect("input")
        .iter()
        .map(|t| {
            let t = t.as_str().unwrap_or("").to_lowercase();
            vec![
                f64::from(u8::from(t.contains("alpha") || t.contains("first letter"))),
                0.1,
            ]
        })
        .collect();
    (200, json!({ "embeddings": rows }).to_string())
}

#[tokio::test]
async fn memory_search_follows_the_index_and_never_hides_a_dead_embed_server() {
    let dir = common::temp("mcp-memory-index");
    let (embed_url, hits) = common::mock(embed);
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(&dir);
        c.env_remove("RUNPOD_API_KEY");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        c.env("OFFRIG_EMBED_URL", &embed_url);
    });
    let client = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");
    let call = |name: &'static str, a: Value| {
        let client = &client;
        async move {
            client
                .call_tool(CallToolRequestParams::new(name).with_arguments(args(a)))
                .await
                .expect("call")
        }
    };
    let record = |text: &'static str| {
        call(
            "offrig_memory_record",
            json!({"kind": "fact", "body": text, "author": "test"}),
        )
    };

    // No index: keywords, no embedding call, and hybrid is refused with the fix.
    let plan = body(&record("alpha is the plan").await);
    assert_eq!(plan["embedded"], false);
    let plan = plan["id"].as_i64().expect("id");
    let s = call("offrig_memory_search", json!({"query": "alpha"})).await;
    assert_eq!(body(&s)["mode"], "keyword");
    assert_eq!(ids(&s), [plan]);
    let refused = call(
        "offrig_memory_search",
        json!({"query": "alpha", "mode": "hybrid"}),
    )
    .await;
    assert_eq!(refused.is_error, Some(true));
    assert!(
        body(&refused).to_string().contains("offrig index"),
        "{}",
        body(&refused)
    );
    let bad_mode = call(
        "offrig_memory_search",
        json!({"query": "alpha", "mode": "magic"}),
    )
    .await;
    assert_eq!(bad_mode.is_error, Some(true));
    assert_eq!(common::count(&hits, "POST /api/embed"), 0);

    // `offrig index` (here, its library call) embeds what exists; new records follow.
    {
        let store = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
        let n = index::embed_pending(&store, &Ollama::new(&embed_url), "nomic-embed-text")
            .expect("embed");
        assert_eq!(n, 1);
    }
    let para = body(&record("the first letter of the Greek alphabet").await);
    assert_eq!(para["embedded"], true);
    let para = para["id"].as_i64().expect("id");
    let s = call("offrig_memory_search", json!({"query": "alpha"})).await;
    assert_eq!(body(&s)["mode"], "hybrid");
    let found = ids(&s);
    assert!(found.contains(&plan) && found.contains(&para), "{found:?}");
    // Keyword mode on request misses the paraphrase.
    let kw = call(
        "offrig_memory_search",
        json!({"query": "alpha", "mode": "keyword"}),
    )
    .await;
    assert_eq!(body(&kw)["mode"], "keyword");
    assert!(!ids(&kw).contains(&para));

    // Hybrid with the embed server gone is an error, not a quiet keyword search.
    client.cancel().await.expect("stop");
    let dead = common::mock(|_, _, _| (500, "down".into())).0;
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(&dir);
        c.env_remove("RUNPOD_API_KEY");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        c.env("OFFRIG_EMBED_URL", &dead);
    });
    let client = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");
    let res = client
        .call_tool(
            CallToolRequestParams::new("offrig_memory_search")
                .with_arguments(args(json!({"query": "alpha"}))),
        )
        .await
        .expect("call");
    assert_eq!(res.is_error, Some(true), "{}", body(&res));
    client.cancel().await.expect("stop");
    let _ = std::fs::remove_dir_all(&dir);
}
