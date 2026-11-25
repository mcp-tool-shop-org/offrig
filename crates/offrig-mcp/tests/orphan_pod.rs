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

async fn connect_mcp_with_env(
    dir: &std::path::Path,
    url: &str,
    api_key: Option<&str>,
    bin: Option<&std::path::Path>,
    fake: Option<&std::path::Path>,
    poll_ms: Option<&str>,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.kill_on_drop(true);
        c.arg("--project").arg(dir);
        match api_key {
            Some(k) => c.env("RUNPOD_API_KEY", k),
            None => c.env_remove("RUNPOD_API_KEY"),
        };
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        c.env("OFFRIG_TEST_HOME", dir.join("home"));
        c.env("OFFRIG_TEST_RUNPOD_BASE", url);
        c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
        if let Some(ms) = poll_ms {
            c.env("OFFRIG_TEST_CAPACITY_POLL_MS", ms);
        }
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

async fn connect_mcp(
    dir: &std::path::Path,
    url: &str,
    api_key: Option<&str>,
    bin: Option<&std::path::Path>,
    fake: Option<&std::path::Path>,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    connect_mcp_with_env(dir, url, api_key, bin, fake, None).await
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
            .is_some_and(|n| n.starts_with("WARNING, watchdog is not guarding:")
                && n.contains("shut down")
                && !n.contains("relaunch")),
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

#[tokio::test]
async fn two_sidecars_racing_during_capacity_wait_refuse_relaunch_and_create_one_pod() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let pods: Pods = Arc::default();
    let capacity_open = Arc::new(AtomicBool::new(true));
    let cap_srv = Arc::clone(&capacity_open);
    let pods_srv = Arc::clone(&pods);
    let (url, hits): (_, Hits) = mock(move |route, body, _| {
        if route == "GET /catalog/gpus" && !cap_srv.load(Ordering::SeqCst) {
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
                        "price": {"secure": 0.50}, "maxCount": {"secure": 0}, "availability": "NONE"
                    })
                })
                .collect();
            return (200, json!({"gpus": types}).to_string());
        }
        stateful_runpod(Arc::clone(&pods_srv))(route, body, 0)
    });
    let dir = temp("two-sidecars-race");
    let db = dir.join(".offrig").join("offrig.db");
    {
        let s = Store::open(&db).expect("store");
        s.set_budget_cap(50.0).expect("cap");
    }
    let (bin, fake) = install_fake_tools(&dir);

    let sidecar_a = connect_mcp_with_env(
        &dir,
        &url,
        Some("test-key"),
        Some(&bin),
        Some(&fake),
        Some("120"),
    )
    .await;
    let plan_resp = call(
        &sidecar_a,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "wait_minutes": 5}),
    )
    .await;
    assert_eq!(plan_resp.is_error, Some(false), "{:?}", body(&plan_resp));
    let plan_id = body(&plan_resp)["plan_id"].as_i64().expect("plan_id");

    // Close capacity before A launches so A writes `create_pod` and stays inside `create_when_free`.
    capacity_open.store(false, Ordering::SeqCst);
    let launch_a = call(&sidecar_a, "offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(launch_a.is_error, Some(false), "{:?}", body(&launch_a));
    let job_id = body(&launch_a)["job_id"].as_i64().expect("job_id");

    let wait_started = Instant::now();
    loop {
        let j = body(&call(&sidecar_a, "offrig_job", json!({"job_id": job_id})).await);
        let unfinished = Store::open(&db)
            .expect("reopen")
            .unfinished_create_pod(plan_id)
            .expect("read");
        if j["progress"]["capacity_wait"]["checks"]
            .as_u64()
            .unwrap_or(0)
            >= 1
            && unfinished.is_some()
        {
            break;
        }
        assert!(
            wait_started.elapsed() < Duration::from_secs(15),
            "side-car A never entered capacity wait with unfinished create_pod: {j}"
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
    }

    // Side-car B opens the same project and attempts a relaunch while A is waiting.
    let sidecar_b = connect_mcp_with_env(
        &dir,
        &url,
        Some("test-key"),
        Some(&bin),
        Some(&fake),
        Some("120"),
    )
    .await;
    let launch_b = call(&sidecar_b, "offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(
        launch_b.is_error,
        Some(true),
        "second side-car must be refused while create_pod is in flight: {:?}",
        body(&launch_b)
    );
    assert_eq!(body(&launch_b)["code"], "refused", "{:?}", body(&launch_b));

    // Open capacity and verify A finishes with exactly one created pod.
    capacity_open.store(true, Ordering::SeqCst);
    let started = Instant::now();
    loop {
        let j = body(&call(&sidecar_a, "offrig_job", json!({"job_id": job_id})).await);
        if j["state"] == "done" {
            assert_eq!(j["progress"]["pod_id"], "pod1");
            break;
        }
        assert_eq!(j["state"], "running", "unexpected job state: {j}");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "never done: {j}"
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    assert_eq!(count(&hits, "POST /pods"), 1, "exactly one POST /pods");
    assert_eq!(pods.lock().expect("pods").len(), 1);
    let s = Store::open(&db).expect("reopen");
    assert_eq!(
        s.plan(plan_id).expect("r").expect("p").pod_id.as_deref(),
        Some("pod1")
    );
    drop(s);

    sidecar_b.cancel().await.ok();
    sidecar_a.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn create_returning_after_another_pod_attached_terminates_duplicate_pod() {
    let pods: Pods = Arc::default();
    let dir = temp("dup-create-race");
    let db = dir.join(".offrig").join("offrig.db");
    let target_plan_id = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let plan_ref = Arc::clone(&target_plan_id);
    let db_hook = db.clone();
    let pods_srv = Arc::clone(&pods);
    let (url, hits): (_, Hits) = mock(move |route, body, n| {
        if route == "POST /pods" {
            let pid = plan_ref.load(std::sync::atomic::Ordering::SeqCst);
            if pid > 0 {
                // Simulate a concurrent winner attaching pod-winner before create_when_free returns.
                let s = Store::open(&db_hook).expect("open db from hook");
                s.attach_pod(pid, "pod-winner").expect("attach winner");
            }
        }
        stateful_runpod(Arc::clone(&pods_srv))(route, body, n)
    });
    {
        let s = Store::open(&db).expect("store");
        s.set_budget_cap(50.0).expect("cap");
    }
    let (bin, fake) = install_fake_tools(&dir);
    let client = connect_mcp(&dir, &url, Some("test-key"), Some(&bin), Some(&fake)).await;

    let plan_resp = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0}),
    )
    .await;
    let plan_id = body(&plan_resp)["plan_id"].as_i64().expect("plan_id");
    target_plan_id.store(plan_id, std::sync::atomic::Ordering::SeqCst);

    let launch = call(&client, "offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(launch.is_error, Some(false), "{:?}", body(&launch));
    let job_id = body(&launch)["job_id"].as_i64().expect("job_id");

    let started = Instant::now();
    loop {
        let j = body(&call(&client, "offrig_job", json!({"job_id": job_id})).await);
        if j["state"] == "failed" {
            assert!(
                j["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("pod-winner") && e.contains("pod1")),
                "{j}"
            );
            break;
        }
        assert_ne!(
            j["state"], "done",
            "duplicate pod launch must fail rather than attach and become done: {j}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "never finished: {j}"
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    assert_eq!(count(&hits, "POST /pods"), 1);
    assert_eq!(
        count(&hits, "DELETE /pods/pod1"),
        1,
        "duplicate newly-created pod must be terminated at once"
    );
    assert_eq!(count(&hits, "DELETE /pods/pod-winner"), 0);
    assert!(pods.lock().expect("pods").is_empty());

    let s = Store::open(&db).expect("reopen");
    let p = s.plan(plan_id).expect("r").expect("plan");
    assert_eq!(p.pod_id.as_deref(), Some("pod-winner"));
    assert_eq!(p.state, "committed", "winning pod keeps plan committed");
    let journal = s.journal_for_plan(plan_id).expect("journal");
    assert!(
        journal
            .iter()
            .any(|(a, _, o)| a == "terminate_duplicate_pod"
                && o.as_deref()
                    .is_some_and(|t| t.contains("pod1") && t.contains("pod-winner"))),
        "{journal:?}"
    );
    drop(s);

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn probe_does_not_reuse_exited_pod_terminates_it_and_creates_fresh_pod() {
    let pods: Pods = Arc::default();
    let (url, hits): (_, Hits) = mock(stateful_runpod(Arc::clone(&pods)));
    let dir = temp("exited-probe");
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
        pods.lock().expect("pods").push(json!({
            "id": "pod-exited",
            "name": format!("offrig-{tag}-job"),
            "status": "EXITED",
            "cost": 0.50,
            "ports": ["22/tcp"],
            "env": {
                "OFFRIG_PLAN": p.id.to_string(),
                "OFFRIG_LANE": tag
            }
        }));
        p.id
    };

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
        assert_eq!(j["state"], "running", "unexpected job state: {j}");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "never done: {j}"
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    assert_eq!(
        count(&hits, "DELETE /pods/pod-exited"),
        1,
        "EXITED pod must be terminated instead of reused"
    );
    assert_eq!(count(&hits, "POST /pods"), 1, "fresh pod must be created");
    let s = Store::open(&db).expect("reopen");
    assert_eq!(
        s.plan(plan_id).expect("r").expect("p").pod_id.as_deref(),
        Some("pod1")
    );
    let journal = s.journal_for_plan(plan_id).expect("journal");
    assert!(
        journal.iter().any(|(a, _, o)| a == "terminate_pod"
            && o.as_deref()
                .is_some_and(|t| t.contains("terminated EXITED pod pod-exited"))),
        "{journal:?}"
    );
    drop(s);

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn plain_lane_launch_never_adopts_unmarked_pod_by_name() {
    let pods: Pods = Arc::default();
    let (url, hits): (_, Hits) = mock(stateful_runpod(Arc::clone(&pods)));
    let dir = temp("plain-unmarked");
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
        // Committed without a lane (plain lane).
        s.commit_plan(p.id).expect("commit");
        pods.lock().expect("pods").push(json!({
            "id": "pod-unmarked",
            "name": "offrig-job",
            "status": "RUNNING",
            "cost": 0.50,
            "ports": ["22/tcp"]
        }));
        p.id
    };

    let client = connect_mcp(&dir, &url, Some("test-key"), Some(&bin), Some(&fake)).await;
    let launch = call(&client, "offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(
        launch.is_error,
        Some(true),
        "plain-lane plan must refuse rather than adopt an unmarked pod: {:?}",
        body(&launch)
    );
    assert_eq!(count(&hits, "POST /pods"), 0);
    let s = Store::open(&db).expect("reopen");
    assert_eq!(s.plan(plan_id).expect("r").expect("p").pod_id, None);
    drop(s);

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn self_stop_armed_and_unavailable_lines_reach_offrig_job_and_status() {
    for (name, ssh_line, expect_unavailable) in [
        (
            "self-stop-armed",
            "offrig self-stop: armed fire_at=1900000120\n",
            false,
        ),
        (
            "self-stop-unavailable",
            "offrig self-stop: UNAVAILABLE (runpodctl, RUNPOD_POD_ID)\n",
            true,
        ),
    ] {
        let pods: Pods = Arc::default();
        let (url, _hits): (_, Hits) = mock(stateful_runpod(Arc::clone(&pods)));
        let dir = temp(name);
        {
            let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
            s.set_budget_cap(50.0).expect("cap");
        }
        let (bin, fake) = install_fake_tools(&dir);
        std::fs::write(
            fake.join("rules.json"),
            json!([
                {
                    "contains": "nvidia-smi",
                    "stdout": "| NVIDIA-SMI 570.1 Driver Version: 570.1 CUDA Version: 13.0 |\n|   0  NVIDIA A100-SXM4-80GB  Off |\n"
                },
                {
                    "contains": "self-stop.status",
                    "stdout": ssh_line
                }
            ])
            .to_string(),
        )
        .expect("rules");

        let client = connect_mcp(&dir, &url, Some("test-key"), Some(&bin), Some(&fake)).await;
        let plan_resp = call(
            &client,
            "offrig_plan",
            json!({"profile": "job", "max_hours": 1.0}),
        )
        .await;
        let plan_id = body(&plan_resp)["plan_id"].as_i64().expect("plan_id");
        let launch = call(&client, "offrig_launch", json!({"plan_id": plan_id})).await;
        assert_eq!(launch.is_error, Some(false), "{name}: {:?}", body(&launch));
        let job_id = body(&launch)["job_id"].as_i64().expect("job_id");

        let started = Instant::now();
        let job = loop {
            let j = body(&call(&client, "offrig_job", json!({"job_id": job_id})).await);
            if j["state"] == "done" {
                break j;
            }
            assert_eq!(j["state"], "running", "{name}: unexpected {j}");
            assert!(started.elapsed() < Duration::from_secs(30), "{name}: {j}");
            tokio::time::sleep(Duration::from_millis(120)).await;
        };

        let expected = ssh_line.trim();
        assert_eq!(job["progress"]["self_stop"], expected, "{name}: {job}");
        let st = body(&call(&client, "offrig_status", json!({})).await);
        assert_eq!(st["open_plans"][0]["self_stop"], expected, "{name}: {st}");

        if expect_unavailable {
            assert!(
                job["warnings"].as_array().is_some_and(|w| w
                    .iter()
                    .any(|v| v.as_str().is_some_and(|s| s.contains("UNAVAILABLE")))),
                "{name}: {job}"
            );
            assert!(
                job["next_action"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("WARNING,") && s.contains("UNAVAILABLE")),
                "{name}: {job}"
            );
            assert!(
                st["self_stop_warnings"]
                    .as_array()
                    .is_some_and(|w| !w.is_empty()),
                "{name}: {st}"
            );
            assert!(
                st["next_action"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("WARNING, on-pod self-stop is unavailable:")),
                "{name}: {st}"
            );
        } else {
            assert_eq!(job["warnings"], json!([]), "{name}: {job}");
            assert_eq!(st["self_stop_warnings"], json!([]), "{name}: {st}");
        }

        client.cancel().await.ok();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
