//! Whole sessions through the real side-car: plan, launch to ready (ssh, tunnel, model
//! pulls), ask, the detached runner, a job pod's exec/put/get, and shutdown. RunPod and
//! the pod model are mocks and `ssh`/`scp` are the fake from offrig-core's examples, so
//! nothing leaves the machine and nothing outside a temp dir is touched.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

use common::{Hits, count, free_port, install_fake_tools, mock, path_with, temp};
use offrig_core::config::Config;
use offrig_core::store::Store;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

fn gpu_ids(profile: &str) -> Vec<String> {
    Config::default()
        .profile(profile)
        .expect("profile")
        .gpu_type_ids
        .clone()
}

fn offers_json() -> String {
    let mut ids = gpu_ids("small");
    ids.extend(gpu_ids("job"));
    let types: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({
                "id": id, "name": id, "memory": 24, "secure": true,
                "price": {"secure": 0.25}, "maxCount": {"secure": 8}, "availability": "HIGH"
            })
        })
        .collect();
    json!({"gpus": types}).to_string()
}

type Pods = Arc<Mutex<Vec<Value>>>;

/// A RunPod that keeps its pods: creates add one named as asked, deletes remove it.
fn runpod(pods: Pods, gpu: String) -> impl Fn(&str, &str, usize) -> (u16, String) + Send + 'static {
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
                let p = json!({
                    "id": "pod1", "name": want["name"], "desiredStatus": "RUNNING",
                    "costPerHr": 0.25, "publicIp": "203.0.113.9",
                    "portMappings": {"22": 40022}, "ports": ["22/tcp"],
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

/// The pod's model: the model list for the pull check, and chat replies that satisfy
/// the handoffs' checks.
fn model(models: Vec<String>) -> (String, Hits) {
    mock(move |route, body, _| match route {
        "GET /api/version" => (200, r#"{"version":"0.35.0"}"#.into()),
        "GET /api/tags" => (
            200,
            json!({"models": models.iter().map(|m| json!({"name": m})).collect::<Vec<_>>()})
                .to_string(),
        ),
        "POST /v1/chat/completions" => {
            let reply = if body.contains("Turn flow") {
                "## Duel flow\n1. Stare\n2. Draw"
            } else {
                "## Verbs\n- Draw\n- Feint\n- Hold"
            };
            (
                200,
                json!({
                    "choices": [{"message": {"content": reply}, "finish_reason": "stop"}],
                    "usage": {"completion_tokens": 12}
                })
                .to_string(),
            )
        }
        _ => (404, "{}".into()),
    })
}

struct Session {
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
}

impl Session {
    async fn call(&self, name: &'static str, a: Value) -> CallToolResult {
        self.client
            .call_tool(
                CallToolRequestParams::new(name)
                    .with_arguments(a.as_object().cloned().expect("object")),
            )
            .await
            .expect("call")
    }

    async fn ok(&self, name: &'static str, a: Value) -> Value {
        let r = self.call(name, a).await;
        assert_eq!(r.is_error, Some(false), "{name}: {:?}", body(&r));
        body(&r)
    }

    /// Poll `offrig_job` until the job leaves `running`.
    async fn finish(&self, job_id: i64) -> Value {
        let started = Instant::now();
        loop {
            let j = self.ok("offrig_job", json!({"job_id": job_id})).await;
            if j["state"] != "running" {
                return j;
            }
            assert!(
                started.elapsed() < Duration::from_secs(90),
                "job {job_id} never finished: {j:?}"
            );
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}

struct Rig {
    dir: std::path::PathBuf,
    session: Session,
    runpod_hits: Hits,
    fake: std::path::PathBuf,
}

impl Rig {
    async fn start(name: &str, gpu: String, models: &[String]) -> Rig {
        let dir = temp(name);
        let pods: Pods = Arc::default();
        let (url, runpod_hits) = mock(runpod(pods, gpu));
        let (ollama, _) = model(models.to_vec());
        let (bin, fake) = install_fake_tools(&dir);
        {
            let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
            s.set_budget_cap(15.0).expect("cap");
        }
        let upstream = ollama.trim_start_matches("http://").to_string();
        let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
            c.arg("--project").arg(&dir);
            c.env("RUNPOD_API_KEY", "test-key");
            c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
            c.env("OFFRIG_TEST_HOME", dir.join("home"));
            c.env("OFFRIG_TEST_RUNPOD_BASE", &url);
            c.env("OFFRIG_TEST_WATCHDOG_POLL_SECS", "1");
            c.env("OFFRIG_TEST_RUNNER_POLL_MS", "100");
            c.env("FAKE_SSH_DIR", &fake);
            c.env("FAKE_SSH_UPSTREAM", &upstream);
            c.env("PATH", path_with(&bin));
        });
        let client = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");
        Rig {
            dir,
            session: Session { client },
            runpod_hits,
            fake,
        }
    }

    fn rules(&self, rules: Value) {
        std::fs::write(self.fake.join("rules.json"), rules.to_string()).expect("rules");
    }

    fn fake_calls(&self) -> String {
        std::fs::read_to_string(self.fake.join("calls.log")).unwrap_or_default()
    }

    async fn finish(self) {
        self.session.client.cancel().await.expect("shutdown");
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn a_model_session_launches_asks_runs_the_queue_and_shuts_down() {
    let models: Vec<String> = Config::default()
        .profile("small")
        .expect("small")
        .models
        .iter()
        .map(|m| m.name.clone())
        .collect();
    let rig = Rig::start("model-session", gpu_ids("small")[0].clone(), &models).await;
    let s = &rig.session;
    rig.rules(json!([
        {"contains": "nvidia-smi", "stdout": "| NVIDIA-SMI 570.1  Driver Version: 570.1  CUDA Version: 12.8 |\n"}
    ]));

    // Queue the work: one handoff to ask about, two for the runner.
    let h1 = s
        .ok(
            "offrig_handoffs",
            json!({"action": "add", "role": "game-designer", "mission": "Core verbs",
                   "acceptance": "three verbs"}),
        )
        .await;
    assert_eq!(h1["id"], 1);
    s.ok(
        "offrig_handoffs",
        json!({"action": "add", "role": "game-designer", "mission": "Core verbs again",
               "acceptance": "three verbs and a heading",
               "checks": [{"check": "heading", "text": "Verbs"}, {"check": "items", "heading": "Verbs", "min": 3}],
               "accept_on_checks": true}),
    )
    .await;
    s.ok(
        "offrig_handoffs",
        json!({"action": "add", "role": "systems-designer", "mission": "Turn flow",
               "acceptance": "numbered flow", "depends_on": [2],
               "checks": [{"check": "heading", "text": "Duel flow"}], "accept_on_checks": true}),
    )
    .await;

    let plan = s
        .ok("offrig_plan", json!({"profile": "small", "max_hours": 1.0}))
        .await;
    let plan_id = plan["plan_id"].as_i64().expect("plan id");
    let launch = s.ok("offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(launch["watchdog"], "started", "{launch:?}");
    let job = s.finish(launch["job_id"].as_i64().expect("job")).await;
    assert_eq!(job["state"], "done", "{job:?}");
    assert_eq!(job["progress"]["step"], "ready");
    // The rental was filled when the pod appeared; once ssh was up, the nvidia-smi CUDA
    // measurement was added to it (it did not replace the GPU and price).
    assert_eq!(job["rented"]["cuda_version"], "12.8", "{job:?}");
    assert_eq!(job["rented"]["cuda_source"], "nvidia-smi", "{job:?}");
    assert!(job["rented"]["gpu"].is_string(), "{job:?}");
    assert!(job["rented"]["cost_per_hr"].is_number(), "{job:?}");
    assert!(
        job["failure"].is_null(),
        "a launch that worked has no failure"
    );
    assert_eq!(count(&rig.runpod_hits, "POST /pods"), 1);
    let calls = rig.fake_calls();
    assert!(calls.contains("ssh|echo ok"), "{calls}");
    assert!(calls.contains("tunnel|127.0.0.1:"), "{calls}");

    // A second launch of the same plan is the same job, not a second pod.
    let again = s.ok("offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(again["already_launched"], true);

    // One turn through the sidecar's own tunnel.
    let ask = s
        .ok(
            "offrig_ask",
            json!({"handoff_id": 1, "instruction": "draft the verbs"}),
        )
        .await;
    assert!(
        ask["reply"].as_str().is_some_and(|r| r.contains("Verbs")),
        "{ask:?}"
    );
    let listed = s.ok("offrig_handoffs", json!({"action": "list"})).await;
    assert_eq!(listed["handoffs"][0]["state"], "running");

    // The runner works the queue and, with it drained, shuts the pod down.
    s.ok(
        "offrig_handoffs",
        json!({"action": "complete", "handoff_id": 1}),
    )
    .await;
    let run = s.ok("offrig_run", json!({"plan_id": plan_id})).await;
    assert_eq!(run["workable"], 1, "{run:?}");
    let done = s.finish(run["job_id"].as_i64().expect("run job")).await;
    assert_eq!(done["state"], "done", "{done:?}");
    let listed = s.ok("offrig_handoffs", json!({"action": "list"})).await;
    for h in listed["handoffs"].as_array().expect("handoffs") {
        assert_eq!(h["state"], "complete", "{listed:?}");
    }
    let out = s
        .ok(
            "offrig_handoffs",
            json!({"action": "output", "handoff_id": 2}),
        )
        .await;
    assert!(
        out["body"].as_str().is_some_and(|b| b.contains("Verbs")),
        "{out:?}"
    );
    assert_eq!(
        count(&rig.runpod_hits, "DELETE /pods/pod1"),
        1,
        "the runner shut the pod down"
    );
    let status = s.ok("offrig_status", json!({})).await;
    assert_eq!(status["budget"]["committed"], 0.0, "{status:?}");
    rig.finish().await;
}

#[tokio::test]
async fn a_job_pod_runs_commands_and_moves_files_then_shuts_down() {
    let rig = Rig::start("job-session", gpu_ids("job")[0].clone(), &[]).await;
    let s = &rig.session;
    rig.rules(json!([
        {"contains": "nvidia-smi", "stdout": "CUDA Version: 12.8\n"},
        {"contains": "setsid nohup", "stdout": "started\n"},
        {"contains": "tail -n", "stdout": "exited 0\nstep 1\nstep 2\n"},
        {"contains": "kill -TERM", "stdout": "ok\n"},
        {"contains": "timeout -k", "stdout": "gpu0\n"}
    ]));

    // No job pod yet.
    let early = s
        .call("offrig_exec", json!({"action": "run", "command": "ls"}))
        .await;
    assert_eq!(early.is_error, Some(true));
    assert!(
        body(&early)["error"]
            .as_str()
            .is_some_and(|e| e.contains("no launched job pod")),
        "{:?}",
        body(&early)
    );

    let plan = s
        .ok("offrig_plan", json!({"profile": "job", "max_hours": 1.0}))
        .await;
    let plan_id = plan["plan_id"].as_i64().expect("plan id");
    let launch = s.ok("offrig_launch", json!({"plan_id": plan_id})).await;
    let job = s.finish(launch["job_id"].as_i64().expect("job")).await;
    assert_eq!(job["state"], "done", "{job:?}");
    assert!(
        job["next_action"]
            .as_str()
            .is_some_and(|n| n.contains("job pod is ready")),
        "{job:?}"
    );

    let ran = s
        .ok(
            "offrig_exec",
            json!({"action": "run", "command": "nvidia-smi -L", "timeout_secs": 10}),
        )
        .await;
    assert_eq!(ran["exit_code"], 0, "{ran:?}");
    assert_eq!(ran["stdout"], "gpu0\n");
    assert_eq!(ran["plan_id"], plan_id);
    let started = s
        .ok(
            "offrig_exec",
            json!({"action": "start", "name": "train", "command": "python train.py"}),
        )
        .await;
    assert_eq!(started["started"], true);
    let local_log = rig.dir.join("logs").join("train.log");
    let status = s
        .ok(
            "offrig_exec",
            json!({"action": "status", "name": "train", "tail": 5,
                   "save_log": local_log.to_str().expect("utf8")}),
        )
        .await;
    assert_eq!(status["status"]["state"], "exited", "{status:?}");
    assert_eq!(status["saved_log"]["bytes"], "fake download".len());
    assert!(local_log.is_file());
    s.ok("offrig_exec", json!({"action": "stop", "name": "train"}))
        .await;
    for bad in [
        json!({"action": "status"}),
        json!({"action": "start", "name": "x"}),
        json!({"action": "run"}),
        json!({"action": "dance"}),
    ] {
        let r = s.call("offrig_exec", bad).await;
        assert_eq!(r.is_error, Some(true), "{:?}", body(&r));
    }

    // Files up and down.
    let up_src = rig.dir.join("data.txt");
    std::fs::write(&up_src, "hello").expect("file");
    let put = s
        .ok(
            "offrig_put",
            json!({"local": up_src.to_str().expect("utf8"), "pod": "in/data.txt"}),
        )
        .await;
    assert_eq!(put["direction"], "up");
    let dest = rig.dir.join("results").join("out.bin");
    let got = s
        .ok(
            "offrig_get",
            json!({"local": dest.to_str().expect("utf8"), "pod": "out.bin", "plan_id": plan_id}),
        )
        .await;
    assert_eq!(got["direction"], "down");
    assert!(dest.is_file());
    let missing = s
        .call(
            "offrig_put",
            json!({"local": rig.dir.join("nope").to_str().expect("utf8"), "pod": "x"}),
        )
        .await;
    assert_eq!(missing.is_error, Some(true));

    let calls = rig.fake_calls();
    assert!(calls.contains("scp|"), "{calls}");
    assert!(calls.contains("mkdir -p"), "{calls}");

    let down = s.ok("offrig_shutdown", json!({"plan_id": plan_id})).await;
    assert!(
        down["outcome"].as_str().is_some_and(|o| o.contains("pod1")),
        "{down:?}"
    );
    assert_eq!(count(&rig.runpod_hits, "DELETE /pods/pod1"), 1);
    rig.finish().await;
    let _ = free_port();
}
