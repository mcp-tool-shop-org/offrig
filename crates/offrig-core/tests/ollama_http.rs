//! The Ollama client against a mock model server: the lists, `show`, delete, warm,
//! plain and streamed chat, and every way a reply can be wrong.

mod support;

use offrig_core::ollama::{Ollama, SseChat, strip_thinking};
use serde_json::json;
use support::serve;

fn chat_ok(text: &str) -> String {
    json!({
        "choices": [{"message": {"content": text}, "finish_reason": "stop"}],
        "usage": {"completion_tokens": 7}
    })
    .to_string()
}

#[test]
fn lists_versions_and_what_is_loaded() {
    let m = serve(|r, _| {
        match r.route().as_str() {
        "GET /api/version" => (200, r#"{"version":"0.35.0"}"#.into()),
        "GET /api/tags" => (
            200,
            r#"{"models":[{"name":"a:1","size":5,"details":{"parameter_size":"4B","quantization_level":"Q4"}},{"name":"b:2"}]}"#.into(),
        ),
        "GET /api/ps" => (
            200,
            r#"{"models":[{"name":"a:1","size_vram":4000000000,"context_length":8192}]}"#.into(),
        ),
        "GET /v1/models" => (200, r#"{"data":[{"id":"a:1"},{"id":"b:2"}]}"#.into()),
        "GET /health" => (200, "{}".into()),
        _ => (404, "{}".into()),
    }
    });
    let o = Ollama::new(&format!("{}/", m.url));
    assert_eq!(o.version().expect("version"), "0.35.0");
    let tags = o.tags().expect("tags");
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0].details.parameter_size, "4B");
    assert_eq!(tags[1].size, 0);
    let loaded = o.loaded().expect("ps");
    assert_eq!(loaded[0].size_vram, 4_000_000_000);
    assert_eq!(loaded[0].context_length, Some(8192));
    assert_eq!(o.openai_models().expect("models"), ["a:1", "b:2"]);
    assert!(o.answers("/health"));
    assert!(!o.answers("/nope"));
}

#[test]
fn a_server_that_is_down_or_unwell_is_an_error() {
    let m = serve(|_, _| (503, "overloaded".into()));
    let o = Ollama::new(&m.url);
    let e = o.version().expect_err("503").to_string();
    assert!(e.contains("503") && e.contains("overloaded"), "{e}");
    assert!(o.tags().is_err() && o.loaded().is_err() && o.openai_models().is_err());
    assert!(o.info("x").is_err());
    assert!(o.warm("x").is_err());
    assert!(!o.answers("/health"));
    // Nothing listens here at all.
    let dead = Ollama::new("http://127.0.0.1:1");
    assert!(dead.version().is_err());
    assert!(dead.chat_check("x").is_err());
    // A 200 that is not JSON.
    let junk = serve(|_, _| (200, "not json".into()));
    let e = Ollama::new(&junk.url).version().expect_err("junk");
    assert!(
        e.to_string().to_lowercase().contains("ollama version"),
        "{e}"
    );
    let bad_tags = serve(|_, _| (200, r#"{"models":"nope"}"#.into()));
    assert!(Ollama::new(&bad_tags.url).tags().is_err());
    assert!(Ollama::new(&bad_tags.url).loaded().is_err());
}

#[test]
fn info_reads_capabilities_and_the_context_window() {
    let m = serve(|r, _| {
        match r.route().as_str() {
        "POST /api/show" => (
            200,
            r#"{"capabilities":["completion","tools","vision","thinking"],"model_info":{"qwen3.context_length":40960}}"#.into(),
        ),
        _ => (404, "{}".into()),
    }
    });
    let info = Ollama::new(&m.url).info("qwen3:4b").expect("info");
    assert!(info.tools && info.images && info.thinking);
    assert_eq!(info.context_length, Some(40960));
    assert!(m.last().body.contains("qwen3:4b"));
    let none = offrig_core::ollama::parse_show(&json!({}));
    assert!(!none.tools && none.context_length.is_none());
}

#[test]
fn delete_and_warm_send_the_right_requests() {
    let m = serve(|r, _| match r.route().as_str() {
        "DELETE /api/delete" if r.body.contains("gone:1") => (200, "{}".into()),
        "DELETE /api/delete" => (404, "no such model".into()),
        "POST /api/generate" => (200, "{}".into()),
        _ => (404, "{}".into()),
    });
    let o = Ollama::new(&m.url);
    o.delete("gone:1").expect("deleted");
    let e = o.delete("other:1").expect_err("missing").to_string();
    assert!(e.contains("404") && e.contains("no such model"), "{e}");
    o.warm("a:1").expect("warm");
    let sent: serde_json::Value = serde_json::from_str(&m.last().body).expect("json");
    assert_eq!(sent["keep_alive"], -1);
    assert_eq!(sent["model"], "a:1");
}

#[test]
fn chat_returns_the_answer_without_the_thinking() {
    let m = serve(|r, n| {
        match (r.route().as_str(), n) {
        ("POST /v1/chat/completions", 0) => (200, chat_ok("<think>hm</think> The answer. ")),
        ("POST /v1/chat/completions", 1) => (200, chat_ok("")),
        ("POST /v1/chat/completions", 2) => (200, r#"{"error":"model not found"}"#.into()),
        ("POST /v1/chat/completions", 3) => (200, r#"{"choices":[{"message":{}}]}"#.into()),
        ("POST /v1/chat/completions", 4) => (
            200,
            json!({"choices":[{"message":{"content":"<think>x</think>"},"finish_reason":"length"}]})
                .to_string(),
        ),
        _ => (500, "boom".into()),
    }
    });
    let o = Ollama::new(&m.url);
    let reply = o.chat_reply("a:1", "hi", Some(64)).expect("reply");
    assert_eq!(reply.text, "The answer.");
    assert_eq!(reply.tokens, Some(7));
    let sent: serde_json::Value = serde_json::from_str(&m.last().body).expect("json");
    assert_eq!(sent["max_tokens"], 64);
    assert_eq!(sent["stream"], false);
    // An empty answer that finished normally is empty text, not an error.
    assert_eq!(o.chat("a:1", "hi", None).expect("empty"), "");
    let e = o
        .chat("a:1", "hi", None)
        .expect_err("error body")
        .to_string();
    assert!(e.contains("model not found"), "{e}");
    let e = o
        .chat("a:1", "hi", None)
        .expect_err("no content")
        .to_string();
    assert!(e.contains("no message content"), "{e}");
    let e = o.chat("a:1", "hi", None).expect_err("length").to_string();
    assert!(e.contains("token limit"), "{e}");
    assert!(o.chat("a:1", "hi", None).is_err(), "a 500 is an error");
}

#[test]
fn a_streamed_chat_check_counts_chunks_and_sees_the_tool_call() {
    let sse = [
        r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#,
        "",
        r#"data: {"choices":[{"delta":{"content":"lo","tool_calls":[{"function":{"name":"read_file"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"function":{"name":"ignored_second"}}]}}]}"#,
        "data: [DONE]",
        r#"data: {"choices":[{"delta":{"content":"after the end"}}]}"#,
    ]
    .join("\n");
    let m = serve(move |_, _| (200, sse.clone()));
    let c = Ollama::new(&m.url).chat_check("a:1").expect("check");
    assert_eq!(c.model, "a:1");
    assert_eq!(c.reply, "Hello");
    assert_eq!(c.streamed_chunks, 3);
    assert_eq!(c.tool_call.as_deref(), Some("read_file"));
    let sent: serde_json::Value = serde_json::from_str(&m.last().body).expect("json");
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["tools"][0]["function"]["name"], "read_file");
}

#[test]
fn a_chat_check_names_what_went_wrong() {
    let empty = serve(|_, _| (200, "data: [DONE]\n".into()));
    let e = Ollama::new(&empty.url)
        .chat_check("a:1")
        .expect_err("empty");
    assert!(e.to_string().contains("no chunks"), "{e}");
    let refused = serve(|_, _| (400, "bad request".into()));
    let e = Ollama::new(&refused.url)
        .chat_check("a:1")
        .expect_err("400");
    assert!(
        e.to_string().contains("http 400") && e.to_string().contains("bad request"),
        "{e}"
    );
    let broken = serve(|_, _| (200, "data: {nope".into()));
    assert!(Ollama::new(&broken.url).chat_check("a:1").is_err());
    let midstream = serve(|_, _| (200, r#"data: {"error":"oom"}"#.into()));
    let e = Ollama::new(&midstream.url)
        .chat_check("a:1")
        .expect_err("error chunk");
    assert!(e.to_string().contains("oom"), "{e}");
}

#[test]
fn the_sse_parser_and_thinking_stripper_handle_odd_lines() {
    let mut s = SseChat::default();
    assert!(!s.feed(": comment").expect("ignored"));
    assert!(!s.feed("event: ping").expect("ignored"));
    assert!(
        !s.feed(r#"data: {"choices":[{"delta":{}}]}"#)
            .expect("empty delta")
    );
    assert_eq!(s.chunks, 1);
    assert!(s.feed("data:[DONE]").expect("done"));
    assert_eq!(strip_thinking("a</think>b</think>  c "), "c");
    assert_eq!(strip_thinking("  plain "), "plain");
}
