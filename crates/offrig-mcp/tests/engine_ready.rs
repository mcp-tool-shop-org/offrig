//! A recipe launch against fakes: one completion is ready, a second failure is
//! reported and the pod stays up, and a hung engine is reported with no delete.
//! RunPod, ssh and the engine are mocks. Nothing is rented and nothing is loaded.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Hits, count, install_fake_tools, mock, path_with, temp};
use offrig_core::config::Config;
use offrig_core::session::{ENGINE_HUNG_NEXT, ENGINE_UNRESPONSIVE_NEXT};
use offrig_core::store::Store;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

const CARD: &str = "\
| NVIDIA-SMI 570.124.06   Driver Version: 570.124.06   CUDA Version: 13.0     |
|   0  NVIDIA RTX PRO 6000 Blackwell Server Edition  Off |
";

const ENGINE_OUT: &str = "\
5000000000
running
1
Application startup complete
no scheduler line
weights resident
";

fn offers() -> String {
    let ids = Config::default()
        .profile("frontier-mini")
        .expect("frontier-mini")
        .gpu_type_ids
        .clone();
    let types: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({
                "id": id, "name": id, "memory": 96, "secure": true,
                "price": {"secure": 1.70}, "maxCount": {"secure": 8}, "availability": "HIGH"
            })
        })
        .collect();
    json!({"gpus": types}).to_string()
}

type Pods = Arc<Mutex<Vec<Value>>>;
type Bodies = Arc<Mutex<Vec<String>>>;

fn runpod(
    pods: Pods,
    creates: Bodies,
) -> impl Fn(&str, &str, usize) -> (u16, String) + Send + 'static {
    move |route, body, _| {
        let mut pods = pods.lock().expect("pods");
        match route {
            "POST /graphql" if body.contains("myself") => (
                200,
                r#"{"data":{"myself":{"clientBalance":40.0,"currentSpendPerHr":0.0,"spendLimit":80}}}"#
                    .into(),
            ),
            "GET /catalog/gpus" => (200, offers()),
            "GET /pods" => (200, Value::Array(pods.clone()).to_string()),
            "POST /pods" => {
                creates.lock().expect("creates").push(body.to_string());
                let want: Value = serde_json::from_str(body).unwrap_or_default();
                let count = want["gpu"]["count"].as_u64().unwrap_or(0);
                let gpu = want["gpu"]["id"].as_str().unwrap_or("");
                let p = json!({
                    "id": "pod1", "name": want["name"], "desiredStatus": "RUNNING",
                    "costPerHr": 1.70, "publicIp": "203.0.113.9",
                    "portMappings": {"22": 40022}, "ports": ["22/tcp"],
                    "gpuCount": count,
                    "machine": {"gpuTypeId": gpu, "dataCenterId": "EU-RO-1"}
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

/// `ready`: health 200 and one choice. `hung`: health stays 503. `unresponsive`:
/// health 200 and every completion fails.
fn engine(mode: &'static str) -> (String, Hits) {
    mock(move |route, _, nth| match (mode, route) {
        ("hung", "GET /health") => (503, "starting".into()),
        (_, "GET /health") => (200, "{}".into()),
        ("unresponsive", "POST /v1/chat/completions") => (500, "down".into()),
        (_, "POST /v1/chat/completions") if nth == 0 || mode == "ready" => (
            200,
            json!({"choices":[{"message":{"content":""},"finish_reason":"length"}]}).to_string(),
        ),
        _ => (404, "{}".into()),
    })
}

struct Session {
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
}

impl Session {
    async fn ok(&self, name: &'static str, a: Value) -> Value {
        let r = self
            .client
            .call_tool(
                CallToolRequestParams::new(name)
                    .with_arguments(a.as_object().cloned().expect("object")),
            )
            .await
            .expect("call");
        assert_eq!(r.is_error, Some(false), "{name}: {:?}", body(&r));
        body(&r)
    }

    /// Poll until `settled` is true, or the job leaves `running` (ready, failed,
    /// cancelled). A hung engine and a second smoke failure stay `running`, so
    /// those cases pass their own check.
    async fn wait(&self, job_id: i64, settled: impl Fn(&Value) -> bool) -> Value {
        let started = Instant::now();
        loop {
            let j = self.ok("offrig_job", json!({"job_id": job_id})).await;
            if settled(&j) || j["state"] != "running" {
                return j;
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "job {job_id} never finished: {j}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

struct Rig {
    dir: std::path::PathBuf,
    session: Session,
    runpod_hits: Hits,
    creates: Bodies,
}

/// A lane of this test's own, off the first port (`11500`). Each case has its own
/// config dir, so each would otherwise take `11500` and the three binds would
/// collide. The port stays inside the lane range (even steps from `11500`).
fn write_lane(cfg_dir: &std::path::Path, project: &std::path::Path, tag: &str, port: u16) {
    let key = offrig_core::lanes::project_key(project);
    let text = format!(
        "[[lane]]\nproject = {key:?}\ntag = {tag:?}\nssh_alias = \"offrig-{tag}\"\ntunnel_port = {port}\n"
    );
    std::fs::write(cfg_dir.join("lanes.toml"), text).expect("lanes");
}

impl Rig {
    async fn start(name: &str, mode: &'static str, pin: bool, port: u16) -> Rig {
        let dir = temp(name);
        let pods: Pods = Arc::default();
        let creates: Bodies = Arc::default();
        let (url, runpod_hits) = mock(runpod(Arc::clone(&pods), Arc::clone(&creates)));
        let (ollama, _) = engine(mode);
        let (bin, fake) = install_fake_tools(&dir);
        std::fs::write(
            fake.join("rules.json"),
            json!([
                {"contains": "nvidia-smi", "stdout": CARD},
                {"contains": "engine.log", "stdout": ENGINE_OUT}
            ])
            .to_string(),
        )
        .expect("rules");
        {
            let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
            s.set_budget_cap(15.0).expect("cap");
        }
        let cfg_dir = dir.join("cfg");
        std::fs::create_dir_all(&cfg_dir).expect("cfg dir");
        if pin {
            let mut cfg = Config::default();
            let mini = cfg
                .profiles
                .iter_mut()
                .find(|p| p.name == "frontier-mini")
                .expect("frontier-mini");
            mini.cuda_versions = Some(vec!["13.0".into()]);
            cfg.save_to(&cfg_dir.join("config.toml"))
                .expect("save config");
        }
        write_lane(&cfg_dir, &dir, name, port);
        let upstream = ollama.trim_start_matches("http://").to_string();
        let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
            c.arg("--project").arg(&dir);
            c.env("RUNPOD_API_KEY", "test-key");
            c.env("OFFRIG_CONFIG_DIR", &cfg_dir);
            c.env("OFFRIG_TEST_HOME", dir.join("home"));
            c.env("OFFRIG_TEST_RUNPOD_BASE", &url);
            c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
            c.env("OFFRIG_TEST_ENGINE_POLL_MS", "40");
            c.env("OFFRIG_TEST_ENGINE_LOOK_MS", "40");
            // Stands in for the real 3 minutes. The 180s boundary is a unit test.
            c.env("OFFRIG_TEST_ENGINE_HANG_MS", "1100");
            c.env("OFFRIG_TEST_SMOKE_TIMEOUT_MS", "800");
            c.env("OFFRIG_TEST_SMOKE_RETRY_MS", "80");
            c.env("FAKE_SSH_DIR", &fake);
            c.env("FAKE_SSH_UPSTREAM", &upstream);
            c.env("PATH", path_with(&bin));
        });
        let client = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");
        Rig {
            dir,
            session: Session { client },
            runpod_hits,
            creates,
        }
    }

    fn create_body(&self) -> Value {
        let raw = self
            .creates
            .lock()
            .expect("creates")
            .last()
            .expect("a create")
            .clone();
        serde_json::from_str(&raw).expect("create json")
    }

    async fn launch(&self) -> Value {
        let plan = self
            .session
            .ok(
                "offrig_plan",
                json!({"profile": "frontier-mini", "max_hours": 1.0}),
            )
            .await;
        let plan_id = plan["plan_id"].as_i64().expect("plan id");
        let launch = self
            .session
            .ok("offrig_launch", json!({"plan_id": plan_id}))
            .await;
        let mut job = self
            .session
            .wait(launch["job_id"].as_i64().expect("job"), |j| {
                j["progress"]["step"] == "engine_unresponsive"
            })
            .await;
        job["_plan"] = plan;
        job
    }

    async fn finish(self) {
        self.session.client.cancel().await.expect("shutdown");
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn a_hang_does_not_shut_the_pod_down() {
    let rig = Rig::start("engine-hung", "hung", false, 11540).await;
    let plan = rig
        .session
        .ok(
            "offrig_plan",
            json!({"profile": "frontier-mini", "max_hours": 1.0}),
        )
        .await;
    let plan_id = plan["plan_id"].as_i64().expect("plan id");
    let launch = rig
        .session
        .ok("offrig_launch", json!({"plan_id": plan_id}))
        .await;
    let job = rig
        .session
        .wait(launch["job_id"].as_i64().expect("job"), |j| {
            j["progress"]["step"] == "engine_hung"
        })
        .await;
    // `done` would mean the pod is ready to take work. A hang stays running.
    assert_eq!(job["state"], "running", "{job}");
    assert_eq!(job["progress"]["step"], "engine_hung", "{job}");
    assert_eq!(job["progress"]["next_action"], ENGINE_HUNG_NEXT, "{job}");
    assert_eq!(job["next_action"], ENGINE_HUNG_NEXT, "{job}");
    assert!(
        job["progress"]["since_startup_secs"].as_u64().unwrap_or(0) >= 1,
        "{job}"
    );
    let log = job["progress"]["log_tail"].as_array().expect("log");
    assert!(log.len() <= 40, "{log:?}");
    assert!(
        log.iter().any(|l| l == "Application startup complete"),
        "{log:?}"
    );
    assert_eq!(job["plan_state"], "committed", "the plan stays open: {job}");
    assert_eq!(
        count(&rig.runpod_hits, "DELETE /pods/pod1"),
        0,
        "a hang is reported, never shut down"
    );
    let sent = rig.create_body();
    assert!(
        sent["gpu"].get("allowedCudaVersions").is_none(),
        "no pin on the default profile: {sent}"
    );
    assert!(sent["gpu"].get("minCudaVersion").is_none(), "{sent}");
    rig.finish().await;
}

#[tokio::test]
async fn a_completion_choice_is_ready_and_the_create_sends_the_cuda_pin() {
    let rig = Rig::start("engine-ready", "ready", true, 11542).await;
    let job = rig.launch().await;
    let plan = &job["_plan"];
    assert_eq!(plan["cuda_versions"], json!(["13.0"]), "{plan}");
    assert!(
        plan["min_cuda"].is_null(),
        "a pin is not also a floor: {plan}"
    );
    let sent = rig.create_body();
    assert_eq!(
        sent["gpu"]["allowedCudaVersions"],
        json!(["13.0"]),
        "{sent}"
    );
    assert!(
        sent["gpu"].get("minCudaVersion").is_none(),
        "the pin is the whole CUDA constraint: {sent}"
    );
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["progress"]["step"], "ready", "{job}");
    assert_eq!(job["gpu_mismatch"], false, "{job}");
    assert_eq!(job["measured_gpu_count"], 1, "{job}");
    let next = job["next_action"].as_str().expect("next");
    assert!(
        next.starts_with("the pod is ready:"),
        "a matching count is not a warning: {next}"
    );
    assert_eq!(count(&rig.runpod_hits, "DELETE /pods/pod1"), 0);
    rig.finish().await;
}

#[tokio::test]
async fn a_second_smoke_failure_is_reported_and_does_not_shut_the_pod_down() {
    let rig = Rig::start("engine-unresponsive", "unresponsive", false, 11544).await;
    let job = rig.launch().await;
    assert_eq!(job["state"], "running", "{job}");
    assert_eq!(job["progress"]["step"], "engine_unresponsive", "{job}");
    assert!(
        job["progress"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("completion smoke") && e.contains("500")),
        "{job}"
    );
    assert!(
        job["progress"]["engine_log_tail"]
            .as_str()
            .is_some_and(|t| t.contains("Application startup complete")),
        "{job}"
    );
    assert_eq!(job["plan_state"], "committed", "the plan stays open: {job}");
    assert_eq!(
        count(&rig.runpod_hits, "DELETE /pods/pod1"),
        0,
        "a second smoke failure is reported, never shut down"
    );
    assert_eq!(
        job["progress"]["next_action"], ENGINE_UNRESPONSIVE_NEXT,
        "{job}"
    );
    assert_eq!(job["next_action"], ENGINE_UNRESPONSIVE_NEXT, "{job}");
    rig.finish().await;
}
