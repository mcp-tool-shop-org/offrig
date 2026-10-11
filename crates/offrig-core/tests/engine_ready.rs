//! A recipe engine becomes ready only when one completion returns a choice, and a
//! startup line followed by a stretch of `/health` staying down is a hang.
//! The server is a local mock. Nothing calls RunPod or a real engine.
//! The production hang is three minutes ([`ENGINE_HANG_AFTER`]); these waits pass a
//! short pace so the test does not sleep it.

mod support;

use std::time::Duration;

use offrig_core::config::{Config, Profile};
use offrig_core::error::Error;
use offrig_core::ollama::Ollama;
use offrig_core::remote::EngineState;
use offrig_core::runpod::RunPod;
use offrig_core::session::{
    ENGINE_HUNG_NEXT, ENGINE_UNRESPONSIVE_NEXT, EnginePace, Event, STARTUP_COMPLETE, Session,
};
use serde_json::Value;
use serde_json::json;
use support::serve;

fn mini() -> Profile {
    Config::default()
        .profile("frontier-mini")
        .expect("frontier-mini")
        .clone()
}

fn pace(hang: Duration) -> EnginePace {
    EnginePace {
        poll: Duration::from_millis(15),
        look_every: Duration::from_millis(15),
        hang_after: hang,
        smoke_timeout: Duration::from_millis(400),
        smoke_retry: Duration::from_millis(40),
    }
}

fn session_for(url: &str) -> Session {
    let port: u16 = url
        .rsplit_once(':')
        .expect("host:port")
        .1
        .parse()
        .expect("port");
    let cfg = Config {
        tunnel_port: port,
        ..Config::default()
    };
    Session::with_client(
        cfg,
        RunPod::new(
            "test-key",
            "http://127.0.0.1:9",
            "http://127.0.0.1:9/graphql",
        ),
    )
}

fn choice() -> String {
    json!({"choices":[{"message":{"content":""},"finish_reason":"length"}]}).to_string()
}

fn running(log: &str) -> EngineState {
    EngineState {
        hf_bytes: 1_000,
        running: true,
        startup_complete: u64::from(log.contains(STARTUP_COMPLETE)),
        log_tail: log.to_string(),
    }
}

fn completion_body(raw: &str) -> Value {
    serde_json::from_str(raw).expect("completion json")
}

#[test]
fn smoke_passes_when_the_completion_returns_a_choice() {
    let m = serve(|r, _| match r.route().as_str() {
        "GET /health" => (200, "{}".into()),
        // Answering here used to be treated as readiness. It is not.
        "GET /v1/models" => (404, r#"{"error":"not readiness"}"#.into()),
        "POST /v1/chat/completions" => (200, choice()),
        other => panic!("unexpected {other}"),
    });
    let session = session_for(&m.url);
    let mut steps = Vec::new();
    session
        .wait_engine_with(
            &mini(),
            Duration::from_secs(5),
            &mut |e| {
                if let Event::Step(s) = e {
                    steps.push(s);
                }
            },
            pace(Duration::from_secs(180)),
            &mut || Ok(running(STARTUP_COMPLETE)),
        )
        .expect("a choice is ready");
    assert_eq!(steps, ["ready"]);
    assert_eq!(
        m.count("GET /v1/models"),
        0,
        "the model list is not the gate"
    );
    assert_eq!(m.count("POST /v1/chat/completions"), 1);
    let body = completion_body(
        &m.requests()
            .into_iter()
            .find(|r| r.route() == "POST /v1/chat/completions")
            .expect("post")
            .body,
    );
    assert_eq!(body["model"], "qwen3-coder-30b");
    assert_eq!(body["max_tokens"], 1);
    assert_eq!(body["temperature"], 0);
    assert_eq!(body["stream"], false);
    assert_eq!(body["messages"][0]["content"], "ok");
}

#[test]
fn smoke_fails_once_then_passes() {
    let m = serve(|r, nth| match r.route().as_str() {
        "GET /health" => (200, "{}".into()),
        "POST /v1/chat/completions" if nth == 0 => (200, r#"{"choices":[]}"#.into()),
        "POST /v1/chat/completions" => (200, choice()),
        other => panic!("unexpected {other}"),
    });
    let session = session_for(&m.url);
    let mut steps = Vec::new();
    session
        .wait_engine_with(
            &mini(),
            Duration::from_secs(5),
            &mut |e| {
                if let Event::Step(s) = e {
                    steps.push(s);
                }
            },
            pace(Duration::from_secs(180)),
            &mut || Ok(running(STARTUP_COMPLETE)),
        )
        .expect("the retry passes");
    assert_eq!(m.count("POST /v1/chat/completions"), 2);
    assert!(
        steps
            .iter()
            .any(|s| s.contains("no choice") && s.contains("retrying once")),
        "{steps:?}"
    );
    assert_eq!(steps.last().map(String::as_str), Some("ready"));
}

#[test]
fn smoke_fails_twice_and_the_step_is_engine_unresponsive() {
    let m = serve(|r, _| match r.route().as_str() {
        "GET /health" => (200, "{}".into()),
        "POST /v1/chat/completions" => (500, "down".into()),
        other => panic!("unexpected {other}"),
    });
    let session = session_for(&m.url);
    let mut steps = Vec::new();
    let err = session
        .wait_engine_with(
            &mini(),
            Duration::from_secs(5),
            &mut |e| {
                if let Event::Step(s) = e {
                    steps.push(s);
                }
            },
            pace(Duration::from_secs(180)),
            &mut || Ok(running("Application startup complete\nno scheduler")),
        )
        .expect_err("two failures");
    assert_eq!(m.count("POST /v1/chat/completions"), 2);
    assert_eq!(
        steps.last().map(String::as_str),
        Some("engine_unresponsive")
    );
    match err {
        Error::EngineUnresponsive {
            message,
            log_tail,
            next_action,
        } => {
            assert!(message.contains("http 500"), "{message}");
            assert!(log_tail.contains(STARTUP_COMPLETE), "{log_tail}");
            assert_eq!(next_action, ENGINE_UNRESPONSIVE_NEXT);
        }
        other => panic!("expected engine_unresponsive, got {other}"),
    }
}

#[test]
fn startup_complete_and_three_minutes_of_503_is_engine_hung() {
    // `hang_after` stands in for [`ENGINE_HANG_AFTER`]. The 179s / 180s boundary
    // is `three_minutes_is_the_hang_threshold`. This is the same wait, sped up.
    let m = serve(|r, _| match r.route().as_str() {
        "GET /health" => (503, "starting".into()),
        other => panic!("unexpected {other}"),
    });
    let session = session_for(&m.url);
    let mut steps = Vec::new();
    let log = "Application startup complete\nno scheduler line\nweights resident";
    let err = session
        .wait_engine_with(
            &mini(),
            Duration::from_secs(5),
            &mut |e| {
                if let Event::Step(s) = e {
                    steps.push(s);
                }
            },
            pace(Duration::from_millis(1_100)),
            &mut || Ok(running(log)),
        )
        .expect_err("hung");
    assert_eq!(
        m.count("POST /v1/chat/completions"),
        0,
        "no smoke before health"
    );
    assert_eq!(steps.last().map(String::as_str), Some("engine_hung"));
    match err {
        Error::EngineHung {
            log_lines,
            secs_since_startup,
            next_action,
        } => {
            assert_eq!(next_action, ENGINE_HUNG_NEXT);
            assert!(secs_since_startup >= 1, "{secs_since_startup}");
            assert!(log_lines.len() <= 40);
            assert!(
                log_lines.iter().any(|l| l == STARTUP_COMPLETE),
                "{log_lines:?}"
            );
            assert!(log_lines.iter().any(|l| l.contains("no scheduler")));
        }
        other => panic!("expected engine_hung, got {other}"),
    }
}

#[test]
fn a_startup_line_younger_than_three_minutes_is_not_a_hang() {
    let m = serve(|r, _| match r.route().as_str() {
        "GET /health" => (503, "starting".into()),
        other => panic!("unexpected {other}"),
    });
    let session = session_for(&m.url);
    let err = session
        .wait_engine_with(
            &mini(),
            Duration::from_millis(250),
            &mut |_| {},
            pace(Duration::from_secs(180)),
            &mut || Ok(running(STARTUP_COMPLETE)),
        )
        .expect_err("still starting");
    assert!(
        matches!(err, Error::Timeout(_)),
        "a young startup line waits, it does not hang: {err}"
    );
}

#[test]
fn smoke_choice_times_out() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        if let Ok(stream) = listener.accept() {
            std::thread::sleep(Duration::from_secs(3));
            drop(stream);
        }
    });
    let err = Ollama::new(&format!("http://{addr}"))
        .smoke_choice("qwen3-coder-30b", Duration::from_millis(200))
        .expect_err("the completion is bounded");
    assert!(
        err.to_string().contains("completion smoke: timed out"),
        "{err}"
    );
}
