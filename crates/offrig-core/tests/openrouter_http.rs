//! The OpenRouter client against a mock server on 127.0.0.1: the price read carries no
//! key, the chat call carries it and streams, and failures keep the generation id so the
//! charge can be asked for.

mod support;

use offrig_core::error::Error;
use offrig_core::openrouter::{OpenRouter, Price, Request};
use support::{dead_url, serve};

const KEY: &str = "sk-or-test-key-9f3a";

fn request() -> Request {
    Request {
        model: "moonshotai/kimi-k3".into(),
        system: "You arrange hymns.".into(),
        user: "Amazing Grace, SATB.".into(),
        max_tokens: 1_000,
        reasoning_effort: Some("high".into()),
        temperature: Some(0.0),
        price: Price {
            prompt: 0.72e-6,
            completion: 15e-6,
            request: 0.0,
            endpoints: 2,
        },
    }
}

const ENDPOINTS: &str = r#"{"data":{"id":"moonshotai/kimi-k3","endpoints":[
    {"provider_name":"Relace","pricing":{"prompt":"0.00000067","completion":"0.000013"}},
    {"provider_name":"Wafer","pricing":{"prompt":"0.00000068","completion":"0.000015"}}]}}"#;

fn stream(parts: &[&str]) -> String {
    let mut s = String::from(": OPENROUTER PROCESSING\n\n");
    for p in parts {
        s.push_str("data: ");
        s.push_str(p);
        s.push_str("\n\n");
    }
    s
}

#[test]
fn the_price_read_is_public_and_takes_the_dearest_provider() {
    let m = serve(|req, _| match req.route().as_str() {
        "GET /models/moonshotai/kimi-k3/endpoints" => (200, ENDPOINTS.to_string()),
        _ => (404, "{}".into()),
    });
    let or = OpenRouter::new(KEY, &m.url);
    let p = or.price("moonshotai/kimi-k3").expect("price");
    assert_eq!(p.prompt_per_m(), 0.68);
    assert_eq!(p.completion_per_m(), 15.0);
    assert_eq!(
        m.last().header("authorization"),
        None,
        "no key on the price read"
    );

    let missing = or.price("moonshotai/nope").expect_err("404");
    assert!(
        matches!(missing, Error::OpenRouter { status: 404, .. }),
        "{missing}"
    );
}

#[test]
fn a_completion_streams_with_the_key_and_reports_usage_and_cost() {
    let body = stream(&[
        r#"{"id":"gen-abc","provider":"Wafer","choices":[{"delta":{"reasoning":"Think. "}}]}"#,
        r#"{"id":"gen-abc","provider":"Wafer","choices":[{"delta":{"content":"X:1\n"}}]}"#,
        r#"{"id":"gen-abc","choices":[{"delta":{"content":"K:G"},"finish_reason":"stop"}]}"#,
        r#"{"id":"gen-abc","choices":[],"usage":{"prompt_tokens":1700,"completion_tokens":19500,"completion_tokens_details":{"reasoning_tokens":15000},"cost":0.3}}"#,
        "[DONE]",
    ]);
    let m = serve(move |req, _| match req.route().as_str() {
        "POST /chat/completions" => (200, body.clone()),
        _ => (404, "{}".into()),
    });
    let or = OpenRouter::new(KEY, &m.url);
    let mut seen = Vec::new();
    let done = or
        .complete(&request(), |g| seen.push(g.to_string()))
        .expect("complete");
    assert_eq!(
        seen,
        ["gen-abc"],
        "the generation id is reported once, first"
    );
    assert_eq!(done.content, "X:1\nK:G");
    assert_eq!(done.reasoning, "Think. ");
    assert_eq!(done.provider.as_deref(), Some("Wafer"));
    assert_eq!(done.finish_reason.as_deref(), Some("stop"));
    assert_eq!(done.usage.prompt_tokens, Some(1_700));
    assert_eq!(done.usage.completion_tokens, Some(19_500));
    assert_eq!(done.usage.reasoning_tokens, Some(15_000));
    assert_eq!(done.usage.cost, Some(0.3));

    let req = m.last();
    assert_eq!(req.header("authorization"), Some(&*format!("Bearer {KEY}")));
    let sent: serde_json::Value = serde_json::from_str(&req.body).expect("json");
    assert_eq!(sent["model"], "moonshotai/kimi-k3");
    assert_eq!(sent["max_tokens"], 1_000);
    assert_eq!(sent["provider"]["max_price"]["completion"], 15.0);
}

#[test]
fn a_mid_stream_error_keeps_the_generation_id_and_redacts_the_key() {
    let body = stream(&[
        r#"{"id":"gen-mid","provider":"Relace","choices":[{"delta":{"content":"X:1"}}]}"#,
        &format!(
            r#"{{"id":"gen-mid","error":{{"code":502,"message":"provider died; key {KEY}"}},"choices":[]}}"#
        ),
    ]);
    let m = serve(move |_, _| (200, body.clone()));
    let or = OpenRouter::new(KEY, &m.url);
    let failed = or
        .complete(&request(), |_| {})
        .expect_err("mid-stream error");
    assert_eq!(failed.generation_id.as_deref(), Some("gen-mid"));
    assert_eq!(failed.provider.as_deref(), Some("Relace"));
    assert_eq!(failed.partial, "X:1");
    let msg = failed.error.to_string();
    assert!(msg.contains("502") && !msg.contains(KEY), "{msg}");
    assert!(failed.error.retryable());
}

#[test]
fn a_stream_that_stops_early_or_an_http_error_is_a_failure() {
    let cut = stream(&[r#"{"id":"gen-cut","choices":[{"delta":{"content":"X"}}]}"#]);
    let m = serve(move |_, _| (200, cut.clone()));
    let failed = OpenRouter::new(KEY, &m.url)
        .complete(&request(), |_| {})
        .expect_err("cut short");
    assert_eq!(failed.generation_id.as_deref(), Some("gen-cut"));

    let m = serve(|_, _| {
        (
            402,
            r#"{"error":{"message":"insufficient credits"}}"#.into(),
        )
    });
    let failed = OpenRouter::new(KEY, &m.url)
        .complete(&request(), |_| {})
        .expect_err("402");
    assert_eq!(failed.generation_id, None, "no generation started");
    assert!(matches!(
        failed.error,
        Error::OpenRouter { status: 402, .. }
    ));

    let failed = OpenRouter::new(KEY, &dead_url())
        .complete(&request(), |_| {})
        .expect_err("nothing listening");
    assert_eq!(failed.error.code(), "network");
}

#[test]
fn a_generation_charge_is_looked_up_with_the_key() {
    let m = serve(|req, nth| {
        match (req.route().as_str(), nth) {
        ("GET /generation", 0) => (404, r#"{"error":"not yet"}"#.into()),
        ("GET /generation", _) => (
            200,
            r#"{"data":{"id":"gen-x","total_cost":0.0123,"tokens_prompt":1700,"tokens_completion":800,"provider_name":"Wafer"}}"#
                .into(),
        ),
        _ => (404, "{}".into()),
    }
    });
    let or = OpenRouter::new(KEY, &m.url);
    assert_eq!(
        or.generation_cost("gen-x").expect("first"),
        None,
        "404: not yet"
    );
    let c = or.generation_cost("gen-x").expect("second").expect("known");
    assert_eq!(c.cost, 0.0123);
    assert_eq!((c.tokens_in, c.tokens_out), (Some(1_700), Some(800)));
    assert_eq!(c.provider.as_deref(), Some("Wafer"));
    assert_eq!(m.last().target, "/generation?id=gen-x");
    assert_eq!(
        m.last().header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );

    let m = serve(|_, _| (500, "{}".into()));
    assert!(
        OpenRouter::new(KEY, &m.url)
            .generation_cost("gen-x")
            .is_err()
    );
}

#[test]
fn the_key_comes_from_the_environment_only() {
    if std::env::var_os("OFFRIG_CHILD").is_some() {
        assert!(matches!(
            OpenRouter::from_env(),
            Err(Error::MissingOpenRouterKey)
        ));
        return;
    }
    support::reexec(
        "the_key_comes_from_the_environment_only",
        &[],
        &["OPENROUTER_API_KEY"],
    );
}
