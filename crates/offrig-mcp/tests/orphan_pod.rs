//! End-to-end tests for orphan-pod recovery, probe-before-create on relaunch, and
//! loud watchdog failure when RUNPOD_API_KEY is missing. Mock RunPod and fake ssh only.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

use common::{Hits, count, install_fake_tools, mock, path_with, temp};
use offrig_core::config::Config;
use offrig_core::lanes::Registry;
use offrig_core::store::{NewPlan, Store};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

type Pods = Arc<Mutex<Vec<Value>>>;

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

fn offers_json() -> String {
    let ids = Config::default()
        .profile("job")
        .expect("job")
        .gpu_type_ids
        .clone();
    let types: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({
                "id": id, "name": id, "memory": 80, "secure": true,
                "price": {"secure": 0.50}, "maxCount": {"secure": 8}, "availability": "HIGH"
            })
        })
        .collect();
    json!({"gpus": types}).to_string()
}

fn stateful_runpod(pods: Pods) -> impl Fn(&str, &str, usize) -> (u16, String) + Send + 'static {
    move |route, body, _| {
        let mut pods = pods.lock().expect("pods");
        match route {
            "POST /graphql" if body.contains("myself") => (
                200,
                r#"{"data":{"myself":{"clientBalance":40.0,"currentSpendPerHr":0.0,"spendLimit":80}}}"#
                    .into(),
            ),
            "GET /catalog/gpus" => (200, offers_json()),
            "GET /pods" => (200, Value::Array(pods.clone()).to_string()),
            "POST /pods" => {
                let want: Value = serde_json::from_str(body).unwrap_or_default();
                let id = format!("pod{}", pods.len() + 1);
                let p = json!({
                    "id": id,
                    "name": want["name"],
                    "status": "RUNNING",
                    "cost": 0.50,
                    "ports": ["22/tcp"],
                    "env": want["env"],
                    "ssh": {
                        "direct": {
                            "host": "203.0.113.9",
                            "port": 40022
                        }
                    },
                    "gpu": {
                        "id": "NVIDIA A100-SXM4-80GB",
                        "count": 1
                    },
                    "cudaVersion": "13.0"
                });
                pods.push(p.clone());
                (200, p.to_string())
            }
            r if r.starts_with("GET /pods/") => {
                let id = &r["GET /pods/".len()..];
                match pods.iter().find(|p| p["id"] == id) {
                    Some(p) => (200, p.to_string()),
                    None => (404, r#"{"error":"not found"}"#.into()),
                }
            }
            r if r.starts_with("DELETE /pods/") => {
                let id = &r["DELETE /pods/".len()..];
                pods.retain(|p| p["id"] != id);
                (204, String::new())
            }
            _ => (404, "{}".into()),
        }
    }
}

fn spawn_watchdog_and_wait(
    dir: &std::path::Path,
    plan_id: i64,
    url: &str,
    api_key: Option<&str>,
) -> std::process::ExitStatus {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp"));
    cmd.args(["--watchdog", &plan_id.to_string(), "--project"])
        .arg(dir)
        .env("OFFRIG_CONFIG_DIR", dir.join("cfg"))
        .env("OFFRIG_TEST_RUNPOD_BASE", url)
        .env("OFFRIG_TEST_WATCHDOG_POLL_SECS", "1");
    match api_key {
        Some(k) => cmd.env("RUNPOD_API_KEY", k),
        None => cmd.env_remove("RUNPOD_API_KEY"),
    };
    let mut child = cmd.spawn().expect("spawn watchdog");
    let started = Instant::now();
    loop {
        if let Some(st) = child.try_wait().expect("wait") {
            return st;
        }
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("watchdog did not finish");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

async fn connect_mcp(
    dir: &std::path::Path,
    url: &str,
    api_key: Option<&str>,
    bin: Option<&std::path::Path>,
    fake: Option<&std::path::Path>,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(dir);
        match api_key {
            Some(k) => c.env("RUNPOD_API_KEY", k),
            None => c.env_remove("RUNPOD_API_KEY"),
        };
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        c.env("OFFRIG_TEST_HOME", dir.join("home"));
        c.env("OFFRIG_TEST_RUNPOD_BASE", url);
        c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
        if let Some(f) = fake {
            c.env("FAKE_SSH_DIR", f);
        }
        if let Some(b) = bin {
            c.env("PATH", path_with(b));
        }
    });
    ().serve(TokioChildProcess::new(cmd).expect("spawn"))
        .await
        .expect("connect")
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &'static str,
    a: Value,
) -> CallToolResult {
    client
        .call_tool(
            CallToolRequestParams::new(name).with_arguments(a.as_object().cloned().expect("obj")),
        )
        .await
        .expect("call")
}

#[test]
fn watchdog_finds_and_terminates_orphan_pod_created_before_attach() {
    let pods: Pods = Arc::default();
    let (url, hits) = mock(stateful_runpod(Arc::clone(&pods)));
    let dir = temp("orphan-wd");
    let cfg_dir = dir.join("cfg");
    let lane = Registry::at(&cfg_dir)
        .lane_for_project(&dir, &Config::default())
        .expect("lane");
    let tag = lane.tag.expect("tag");
    let db = dir.join(".offrig").join("offrig.db");

    let plan_id = {
        let s = Store::open(&db).expect("store");
        s.set_budget_cap(50.0).expect("cap");
        let p = s
            .create_plan(NewPlan {
                profile: "job".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 0.0005, // ~2 seconds
                max_price_hr: 0.50,
                note: None,
            })
            .expect("plan");
        s.set_plan_lane(p.id, &tag).expect("lane");
        s.commit_plan(p.id).expect("commit");
        // Simulate process crash right after RunPod created the pod: created in the fake
        // RunPod and journaled, but store.attach_pod never ran.
        let rp = offrig_core::runpod::RunPod::new("test-key", &url, &format!("{url}/graphql"));
        let body = offrig_core::spec::pod_create(
            &Config::default()
                .in_lane(&offrig_core::lanes::Lane {
                    tag: Some(tag.clone()),
                    ssh_alias: format!("offrig-{tag}"),
                    tunnel_port: 11500,
                })
                .expect("lane cfg"),
            Config::default().profile("job").expect("job"),
        );
        let created = rp.create_pod(&body).expect("create pod in fake");
        let j = s
            .journal("create_pod", Some(p.id), &json!({"profile": "job"}))
            .expect("journal");
        s.journal_outcome(j, &format!("created pod {}", created.id))
            .expect("outcome");
        p.id
    };
    assert_eq!(pods.lock().expect("pods").len(), 1);

    let status = spawn_watchdog_and_wait(&dir, plan_id, &url, Some("test-key"));
    assert!(status.success(), "watchdog exited with {status}");
    assert!(
        pods.lock().expect("pods").is_empty(),
        "no pod left in the fake RunPod"
    );
    assert_eq!(count(&hits, "DELETE /pods/pod1"), 1);

    let s = Store::open(&db).expect("reopen");
    let closed = s.plan(plan_id).expect("read").expect("plan");
    assert_eq!(closed.state, "closed");
    assert_eq!(closed.pod_id.as_deref(), Some("pod1"));
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn relaunch_after_crash_attaches_existing_pod_instead_of_creating_a_second() {
    let pods: Pods = Arc::default();
    let (url, hits): (_, Hits) = mock(stateful_runpod(Arc::clone(&pods)));
    let dir = temp("orphan-relaunch");
    let cfg_dir = dir.join("cfg");
    let lane = Registry::at(&cfg_dir)
        .lane_for_project(&dir, &Config::default())
        .expect("lane");
    let tag = lane.tag.clone().expect("tag");
    let db = dir.join(".offrig").join("offrig.db");
    let (bin, fake) = install_fake_tools(&dir);

    let plan_id = {
        let s = Store::open(&db).expect("store");
        s.set_budget_cap(50.0).expect("cap");
        let p = s
            .create_plan(NewPlan {
                profile: "job".into(),
                gpu_count: 1,
                gpu_types: vec!["NVIDIA A100-SXM4-80GB".into()],
                max_hours: 1.0,
                max_price_hr: 0.50,
                note: None,
            })
            .expect("plan");
        s.set_plan_lane(p.id, &tag).expect("lane");
        s.commit_plan(p.id).expect("commit");
        let _old_job = s.create_job(p.id, "launch").expect("stale running job");

        // Simulate crash between create_when_free and attach_pod.
        let rp = offrig_core::runpod::RunPod::new("test-key", &url, &format!("{url}/graphql"));
        let lane_cfg = Config::default().in_lane(&lane).expect("lane cfg");
        let mut spec =
            offrig_core::spec::pod_create(&lane_cfg, lane_cfg.profile("job").expect("job"));
        let committed = s.plan(p.id).expect("read").expect("plan");
        offrig_core::spec::mark_plan(&mut spec, &committed);
        let created = rp.create_pod(&spec).expect("create pod in fake");
        let j = s
            .journal("create_pod", Some(p.id), &json!({"profile": "job"}))
            .expect("journal");
        s.journal_outcome(j, &format!("created pod {}", created.id))
            .expect("outcome");
        p.id
    };
    assert_eq!(count(&hits, "POST /pods"), 1);

    let client = connect_mcp(&dir, &url, Some("test-key"), Some(&bin), Some(&fake)).await;
    let launch = call(&client, "offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(launch.is_error, Some(false), "{:?}", body(&launch));
    let job_id = body(&launch)["job_id"].as_i64().expect("job id");

    let started = Instant::now();
    loop {
        let j = body(&call(&client, "offrig_job", json!({"job_id": job_id})).await);
        if j["state"] == "done" {
            assert_eq!(j["progress"]["pod_id"], "pod1");
            break;
        }
        assert!(
            j["state"] == "running",
            "unexpected job state during relaunch: {j}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "relaunch never finished: {j}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    assert_eq!(
        count(&hits, "POST /pods"),
        1,
        "probe-before-create must attach the existing pod and never create a second"
    );
    assert_eq!(pods.lock().expect("pods").len(), 1);

    let s = Store::open(&db).expect("reopen");
    assert_eq!(
        s.plan(plan_id)
            .expect("read")
            .expect("plan")
            .pod_id
            .as_deref(),
        Some("pod1")
    );
    let journal = s.journal_for_plan(plan_id).expect("journal");
    assert!(
        journal.iter().any(|(a, _, o)| a == "attach_existing_pod"
            && o.as_deref().is_some_and(|t| t.contains("pod1"))),
        "{journal:?}"
    );
    drop(s);

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn without_a_key_watchdog_fails_loudly_in_status_and_refuses_launch() {
    let pods: Pods = Arc::default();
    let (url, hits) = mock(stateful_runpod(Arc::clone(&pods)));
    let dir = temp("nokey-wd");
    let db = dir.join(".offrig").join("offrig.db");

    let (open_id, planned_id) = {
        let s = Store::open(&db).expect("store");
        s.set_budget_cap(50.0).expect("cap");
        let p1 = s
            .create_plan(NewPlan {
                profile: "small".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 1.0,
                max_price_hr: 0.25,
                note: None,
            })
            .expect("plan1");
        s.commit_plan(p1.id).expect("commit");
        s.attach_pod(p1.id, "p1").expect("attach");
        let p2 = s
            .create_plan(NewPlan {
                profile: "small".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 1.0,
                max_price_hr: 0.25,
                note: None,
            })
            .expect("plan2");
        (p1.id, p2.id)
    };

    // 1. Watchdog process without RUNPOD_API_KEY must exit non-zero and record failure.
    let status = spawn_watchdog_and_wait(&dir, open_id, &url, None);
    assert!(
        !status.success(),
        "watchdog must fail loudly when RUNPOD_API_KEY is missing"
    );
    let log = std::fs::read_to_string(dir.join(".offrig").join(format!("watchdog-{open_id}.log")))
        .expect("watchdog log");
    assert!(log.contains("FATAL"), "{log}");

    // 2. offrig_status surfaces the failed watchdog clearly and offrig_launch refuses.
    let client = connect_mcp(&dir, &url, None, None, None).await;
    let st = body(&call(&client, "offrig_status", json!({})).await);
    assert_eq!(st["open_plans"][0]["watchdog_alive"], false, "{st}");
    assert_eq!(st["open_plans"][0]["watchdog_guarding"], false, "{st}");
    assert!(
        st["open_plans"][0]["watchdog_error"]
            .as_str()
            .is_some_and(|e| e.contains("RUNPOD_API_KEY")),
        "{st}"
    );
    assert!(
        st["watchdog_warnings"]
            .as_array()
            .is_some_and(|w| !w.is_empty()),
        "{st}"
    );
    assert!(
        st["next_action"]
            .as_str()
            .is_some_and(|n| n.starts_with("WARNING, watchdog is not guarding:")),
        "{st}"
    );

    let launch = call(&client, "offrig_launch", json!({"plan_id": planned_id})).await;
    assert_eq!(launch.is_error, Some(true), "{:?}", body(&launch));
    assert_eq!(
        body(&launch)["code"],
        "missing_api_key",
        "{:?}",
        body(&launch)
    );
    assert_eq!(count(&hits, "POST /pods"), 0);

    let s = Store::open(&db).expect("reopen");
    assert_eq!(
        s.plan(planned_id).expect("read").expect("plan").state,
        "planned",
        "refused launch must not commit the plan"
    );
    drop(s);

    client.cancel().await.ok();

    // 3. Even if RUNPOD_API_KEY is later set, another open plan whose watchdog is not
    //    guarding blocks launching a new paid pod until resolved.
    let client_with_key = connect_mcp(&dir, &url, Some("test-key"), None, None).await;
    let blocked = call(
        &client_with_key,
        "offrig_launch",
        json!({"plan_id": planned_id}),
    )
    .await;
    assert_eq!(blocked.is_error, Some(true), "{:?}", body(&blocked));
    assert!(
        body(&blocked)["error"]
            .as_str()
            .is_some_and(|e| e.contains("has a watchdog that is not guarding")),
        "{:?}",
        body(&blocked)
    );
    assert_eq!(count(&hits, "POST /pods"), 0);
    client_with_key.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}
