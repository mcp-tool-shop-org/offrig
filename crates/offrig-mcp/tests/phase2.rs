//! Phase 2 end to end against a mock RunPod: launch, idempotent relaunch, job
//! progress, ask without a ready session, shutdown, and the watchdog process
//! terminating a pod at its plan's deadline. The binaries are debug builds, which
//! honour OFFRIG_TEST_RUNPOD_BASE; release builds ignore it.

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use offrig_core::store::{NewHandoff, NewPlan, Store};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

type Hits = Arc<Mutex<Vec<String>>>;

/// A small HTTP server standing in for RunPod. `handler(route, body, nth)`.
fn mock(handler: impl Fn(&str, &str, usize) -> (u16, String) + Send + 'static) -> (String, Hits) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let hits: Hits = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts
                .next()
                .unwrap_or("")
                .split('?')
                .next()
                .unwrap_or("")
                .to_string();
            let route = format!("{method} {path}");
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            let _ = reader.read_exact(&mut body);
            let body = String::from_utf8_lossy(&body).to_string();
            let nth = {
                let mut l = log.lock().expect("hits");
                let n = l.iter().filter(|h| **h == route).count();
                l.push(route.clone());
                n
            };
            let (status, text) = handler(&route, &body, nth);
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = reader.get_mut().write_all(resp.as_bytes());
        }
    });
    (url, hits)
}

fn count(h: &Hits, route: &str) -> usize {
    h.lock()
        .expect("hits")
        .iter()
        .filter(|x| *x == route)
        .count()
}

fn graphql(body: &str) -> String {
    if body.contains("myself") {
        return r#"{"data":{"myself":{"clientBalance":12.0,"currentSpendPerHr":0.0,"spendLimit":80}}}"#.into();
    }
    // Every small-tier GPU type, free at 0.25.
    r#"{"data":{"gpuTypes":[
        {"id":"NVIDIA RTX 2000 Ada Generation","displayName":"RTX 2000 Ada","memoryInGb":16,"secureCloud":true,
         "lowestPrice":{"uninterruptablePrice":0.25,"stockStatus":"High"}}]}}"#
        .into()
}

fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("offrig-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

#[tokio::test]
async fn launch_job_ask_and_shutdown_against_a_mock_runpod() {
    let deleted = Arc::new(AtomicBool::new(false));
    let gone = Arc::clone(&deleted);
    let pod = r#"{"id":"p1","name":"offrig-small","desiredStatus":"RUNNING","costPerHr":0.25,"ports":["22/tcp"]}"#;
    let (url, hits) = mock(move |route, body, _| match route {
        "POST /graphql" => (200, graphql(body)),
        "GET /pods" => (200, "[]".into()),
        "POST /pods" => (200, pod.into()),
        "GET /pods/p1" if gone.load(Ordering::SeqCst) => (404, r#"{"error":"not found"}"#.into()),
        "GET /pods/p1" => (200, pod.into()),
        "DELETE /pods/p1" => {
            gone.store(true, Ordering::SeqCst);
            (204, String::new())
        }
        _ => (404, "{}".into()),
    });

    let dir = temp("phase2");
    {
        let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
        s.set_budget_cap(15.0).expect("cap");
        s.add_handoff(NewHandoff {
            role_id: "game-designer".into(),
            mission: "design the standoff duel".into(),
            acceptance: "spec lists verbs and failure states".into(),
            ..Default::default()
        })
        .expect("handoff");
    }
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(&dir);
        c.env("RUNPOD_API_KEY", "test-key");
        c.env("OFFRIG_TEST_RUNPOD_BASE", &url);
        c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
    });
    let client = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");
    let call = |name: &'static str, a: Value| {
        let client = &client;
        async move {
            client
                .call_tool(
                    CallToolRequestParams::new(name)
                        .with_arguments(a.as_object().cloned().expect("object")),
                )
                .await
                .expect("call")
        }
    };

    let names: Vec<String> = client
        .list_all_tools()
        .await
        .expect("list")
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    for want in [
        "offrig_launch",
        "offrig_job",
        "offrig_ask",
        "offrig_shutdown",
    ] {
        assert!(names.contains(&want.to_string()), "missing {want}");
    }

    let plan = call("offrig_plan", json!({"profile": "small", "max_hours": 1.0})).await;
    assert_eq!(plan.is_error, Some(false), "{:?}", body(&plan));
    let plan_id = body(&plan)["plan_id"].as_i64().expect("plan id");
    assert_eq!(body(&plan)["worst_case"], 0.25);

    let launch = call("offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(launch.is_error, Some(false), "{:?}", body(&launch));
    let job_id = body(&launch)["job_id"].as_i64().expect("job id");
    assert_eq!(body(&launch)["budget"]["committed"], 0.25);

    let again = call("offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(body(&again)["already_launched"], true, "{:?}", body(&again));
    assert_eq!(body(&again)["job_id"].as_i64(), Some(job_id));

    // The job creates exactly one pod and reaches the wait for its address.
    let started = Instant::now();
    loop {
        let j = call("offrig_job", json!({"job_id": job_id})).await;
        if body(&j)["progress"]["pod_id"] == "p1" {
            assert_eq!(body(&j)["state"], "running");
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "pod never created: {:?}",
            body(&j)
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(count(&hits, "POST /pods"), 1);

    // No ssh endpoint yet: asking fails with something to do, and touches no handoff.
    let ask = call(
        "offrig_ask",
        json!({"handoff_id": 1, "instruction": "draft the spec"}),
    )
    .await;
    assert_eq!(ask.is_error, Some(true));
    let st = call("offrig_handoffs", json!({"action": "list"})).await;
    assert_eq!(body(&st)["handoffs"][0]["state"], "pending");

    // A failure outcome must say what failed; the hint says how.
    let bare = call(
        "offrig_handoffs",
        json!({"action": "fail", "handoff_id": 1}),
    )
    .await;
    assert_eq!(bare.is_error, Some(true));
    assert!(
        body(&bare)["next_action"]
            .as_str()
            .is_some_and(|n| n.contains("pass reason")),
        "{:?}",
        body(&bare)
    );

    // While the pod is up, status points at the live session, not at setup.
    let live = call("offrig_status", json!({})).await;
    assert!(
        body(&live)["next_action"]
            .as_str()
            .is_some_and(|n| n.contains("live on pod p1")),
        "{:?}",
        body(&live)
    );

    let down = call("offrig_shutdown", json!({"plan_id": plan_id})).await;
    assert_eq!(down.is_error, Some(false), "{:?}", body(&down));
    assert!(
        body(&down)["outcome"]
            .as_str()
            .is_some_and(|o| o.contains("p1"))
    );
    assert!(count(&hits, "DELETE /pods/p1") >= 1);
    let twice = call("offrig_shutdown", json!({"plan_id": plan_id})).await;
    assert_eq!(body(&twice)["already"], "closed", "shutdown is idempotent");

    let status = call("offrig_status", json!({})).await;
    let b = &body(&status)["budget"];
    assert_eq!(b["committed"], 0.0, "the commitment was released: {b:?}");
    assert!(b["remaining"].as_f64().is_some_and(|r| r > 14.9), "{b:?}");
    assert_eq!(count(&hits, "POST /pods"), 1, "never a second pod");

    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_watchdog_process_terminates_at_the_deadline() {
    let (url, hits) = mock(|route, _, _| match route {
        "GET /pods" => (
            200,
            r#"[{"id":"p9","name":"offrig-small","desiredStatus":"RUNNING","costPerHr":0.25}]"#
                .into(),
        ),
        "DELETE /pods/p9" => (204, String::new()),
        _ => (404, "{}".into()),
    });
    let dir = temp("watchdog");
    let db = dir.join(".offrig").join("offrig.db");
    let plan_id = {
        let s = Store::open(&db).expect("store");
        s.set_budget_cap(15.0).expect("cap");
        let p = s
            .create_plan(NewPlan {
                profile: "small".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 0.0005, // under two seconds
                max_price_hr: 0.25,
                note: None,
            })
            .expect("plan");
        s.commit_plan(p.id).expect("commit");
        s.attach_pod(p.id, "p9").expect("attach");
        p.id
    };
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp"))
        .args(["--watchdog", &plan_id.to_string(), "--project"])
        .arg(&dir)
        .env("RUNPOD_API_KEY", "test-key")
        .env("OFFRIG_TEST_RUNPOD_BASE", &url)
        .env("OFFRIG_TEST_WATCHDOG_POLL_SECS", "1")
        .spawn()
        .expect("spawn watchdog");
    let started = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().expect("wait") {
            break st;
        }
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("watchdog did not finish");
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(status.success());
    assert_eq!(
        count(&hits, "DELETE /pods/p9"),
        1,
        "terminated exactly once"
    );
    let s = Store::open(&db).expect("reopen");
    assert_eq!(
        s.plan(plan_id).expect("read").expect("exists").state,
        "closed"
    );
    assert!((s.budget().expect("b").committed).abs() < 1e-9);
    let log = std::fs::read_to_string(dir.join(".offrig").join(format!("watchdog-{plan_id}.log")))
        .expect("log");
    assert!(log.contains("Terminate"), "{log}");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
