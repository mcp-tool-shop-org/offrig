//! Every `offrig` command end to end against a mock RunPod, a mock pod model server and
//! a fake `ssh` on PATH (see offrig-core's `fake_ssh` example). Config, ssh config and
//! Zed settings all live in a temp dir; nothing reaches a real host or the user's files.

use std::sync::{Arc, Mutex};

use offrig_core::config::Config;
use serde_json::{Value, json};

mod common;
use common::{Rig, count, err, out};

fn small() -> Config {
    Config::default()
}

fn gpu_ids() -> Vec<String> {
    small()
        .profile("small")
        .expect("small")
        .gpu_type_ids
        .clone()
}

fn model_names() -> Vec<String> {
    small()
        .profile("small")
        .expect("small")
        .models
        .iter()
        .map(|m| m.name.clone())
        .collect()
}

fn pod(name: &str, id: &str) -> Value {
    json!({
        "id": id, "name": name, "desiredStatus": "RUNNING", "costPerHr": 0.25,
        "publicIp": "203.0.113.7", "portMappings": {"22": 40022},
        "ports": ["22/tcp"], "lastStartedAt": "2026-10-07T00:00:00Z",
        "machine": {"gpuTypeId": gpu_ids()[0], "dataCenterId": "EU-RO-1"}
    })
}

fn account(balance: f64) -> String {
    json!({"data": {"myself": {"clientBalance": balance, "currentSpendPerHr": 1.0, "spendLimit": 80}}})
        .to_string()
}

fn offers(price: Option<f64>) -> String {
    let types: Vec<Value> = gpu_ids()
        .iter()
        .map(|id| {
            json!({
                "id": id, "displayName": id, "memoryInGb": 16, "secureCloud": true,
                "lowestPrice": price.map(|p| json!({"uninterruptablePrice": p, "stockStatus": "High"}))
            })
        })
        .collect();
    json!({"data": {"gpuTypes": types}}).to_string()
}

type Pods = Arc<Mutex<Vec<Value>>>;

/// A RunPod that keeps its pod list: creates add to it, deletes remove from it.
fn runpod(pods: Pods, balance: f64) -> impl Fn(&str, &str) -> (u16, String) + Send + 'static {
    move |route, body| {
        let mut pods = pods.lock().expect("pods");
        match route {
            "POST /graphql" if body.contains("myself") => (200, account(balance)),
            "POST /graphql" => (200, offers(Some(1.0))),
            "GET /pods" => (200, Value::Array(pods.clone()).to_string()),
            "POST /pods" => {
                let want: Value = serde_json::from_str(body).unwrap_or_default();
                let p = pod(want["name"].as_str().unwrap_or("x"), "pod1");
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
            "GET /networkvolumes" => (200, "[]".into()),
            "POST /networkvolumes" => (
                200,
                json!({"id": "vol1", "name": "v", "size": 400, "dataCenterId": "EU-RO-1"})
                    .to_string(),
            ),
            r if r.starts_with("DELETE /networkvolumes/") => (204, String::new()),
            _ => (404, "{}".into()),
        }
    }
}

fn rig_with(name: &str, existing: Vec<Value>, balance: f64) -> (Rig, Pods) {
    let pods: Pods = Arc::new(Mutex::new(existing));
    let models = model_names();
    let names: Vec<&str> = models.iter().map(String::as_str).collect();
    let rig = Rig::new(
        name,
        |c| c.active_profile = "small".into(),
        &names,
        runpod(Arc::clone(&pods), balance),
    );
    (rig, pods)
}

fn lane_pod() -> Value {
    pod("offrig-small", "pod1")
}

#[test]
fn status_lists_each_kind_of_pod_with_its_spend() {
    let (rig, _) = rig_with(
        "status",
        vec![
            lane_pod(),
            pod("offrig-proj-small", "pod2"),
            pod("somebody-elses", "pod3"),
        ],
        50.0,
    );
    let o = rig.run(&["status"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(
        text.contains("balance $50.00, spending $1.00/hr, runway 50.0 h"),
        "{text}"
    );
    assert!(
        text.contains("[offrig] offrig-small pod1 RUNNING $0.25/hr"),
        "{text}"
    );
    assert!(text.contains("[lane  ] offrig-proj-small pod2"), "{text}");
    assert!(text.contains("[other ] somebody-elses pod3"), "{text}");
    assert!(text.contains("this session"), "{text}");
    assert!(text.contains("ssh 203.0.113.7:40022"), "{text}");
}

#[test]
fn gpus_lists_offers_and_filters_by_vram() {
    let (rig, _) = rig_with("gpus", vec![], 50.0);
    let o = rig.run(&["gpus", "--count", "2"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("GPU (x2)"), "{text}");
    assert!(text.contains("32GB"), "two 16 GB cards: {text}");
    assert!(text.contains("1.00"), "{text}");
    let none = rig.run(&["gpus", "--count", "2", "--min-vram", "999"]);
    assert_eq!(
        out(&none).lines().count(),
        1,
        "only the header: {}",
        out(&none)
    );
}

#[test]
fn profiles_marks_the_active_one_and_shows_a_staged_volume() {
    let (rig, _) = rig_with("profiles", vec![], 50.0);
    let mut cfg = small();
    cfg.active_profile = "small".into();
    cfg.profile("small").expect("small");
    for p in &mut cfg.profiles {
        if p.name == "frontier" {
            p.network_volume_id = Some("vol9".into());
            p.data_center_id = Some("EU-RO-1".into());
        }
    }
    cfg.save_to(&rig.dir.join("cfg").join("config.toml"))
        .expect("save");
    let o = rig.run(&["profiles"]);
    let text = out(&o);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(text.contains("* small"), "{text}");
    assert!(text.contains("network volume vol9"), "{text}");
    assert!(text.contains("GB pod disk"), "{text}");
    assert!(text.contains("qwen3:4b"), "{text}");
}

#[test]
fn init_writes_the_default_config_once() {
    let (rig, _) = rig_with("init", vec![], 50.0);
    std::fs::remove_file(rig.dir.join("cfg").join("config.toml")).expect("remove");
    let first = rig.run(&["init"]);
    assert!(out(&first).contains("wrote "), "{}", out(&first));
    let again = rig.run(&["init"]);
    assert!(
        out(&again).contains("config already exists"),
        "{}",
        out(&again)
    );
}

#[test]
fn budget_shows_the_cap_for_a_project_directory() {
    let (rig, _) = rig_with("budget", vec![], 50.0);
    let proj = rig.dir.join("proj");
    std::fs::create_dir_all(&proj).expect("proj");
    let p = proj.to_str().expect("utf8");
    let set = rig.run(&["budget", "7.5", "--project", p]);
    assert_eq!(set.status.code(), Some(0), "{}", err(&set));
    let show = rig.run(&["budget", "--project", p]);
    assert!(out(&show).contains("cap $7.50"), "{}", out(&show));
}

#[test]
fn down_needs_yes_then_terminates_the_pod() {
    let (rig, pods) = rig_with("down", vec![lane_pod()], 50.0);
    let refused = rig.run(&["down"]);
    assert_eq!(refused.status.code(), Some(1), "{}", err(&refused));
    assert!(
        err(&refused).contains("Re-run with --yes"),
        "{}",
        err(&refused)
    );
    assert!(err(&refused).contains("deleted"), "{}", err(&refused));
    let done = rig.run(&["down", "--yes"]);
    assert_eq!(done.status.code(), Some(0), "{}", err(&done));
    assert!(
        out(&done).contains("terminated offrig-small (pod1)"),
        "{}",
        out(&done)
    );
    assert!(pods.lock().expect("pods").is_empty());
    let none = rig.run(&["down", "--yes"]);
    assert!(
        out(&none).contains("no pod for profile small"),
        "{}",
        out(&none)
    );
}

#[test]
fn down_says_models_on_a_network_volume_are_kept() {
    let (rig, _) = rig_with(
        "down-vol",
        vec![{
            let mut p = lane_pod();
            p["networkVolumeId"] = json!("vol1");
            p
        }],
        50.0,
    );
    let refused = rig.run(&["down"]);
    assert!(err(&refused).contains("kept"), "{}", err(&refused));
}

#[test]
fn stage_refuses_before_it_creates_or_costs() {
    let (rig, _) = rig_with("stage-refuse", vec![], 50.0);
    // An Ollama profile is not staged.
    let o = rig.run(&["stage", "small", "--dc", "EU-RO-1", "--yes"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("no recipe"), "{}", err(&o));
    // A recipe profile needs a data center.
    let o = rig.run(&["stage", "frontier"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("--dc"), "{}", err(&o));
    // And a yes, with the charge spelled out.
    let o = rig.run(&["stage", "frontier", "--dc", "EU-RO-1"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        err(&o).contains("/month") && err(&o).contains("--yes"),
        "{}",
        err(&o)
    );
    assert_eq!(count(&rig.runpod_hits, "POST /networkvolumes"), 0);
    // Removing something never staged.
    let o = rig.run(&["stage", "frontier", "--remove", "--yes"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("no staged volume"), "{}", err(&o));
}

#[test]
fn stage_records_the_volume_before_the_fill_and_remove_deletes_it() {
    let inner = runpod(Arc::new(Mutex::new(vec![])), 50.0);
    let models = model_names();
    let names: Vec<&str> = models.iter().map(String::as_str).collect();
    let rig = Rig::new(
        "stage-run",
        |c| c.active_profile = "small".into(),
        &names,
        move |route, body| {
            if route == "POST /pods" {
                return (500, r#"{"error":"create pod: boom"}"#.into());
            }
            inner(route, body)
        },
    );
    // The mock cannot create the staging pod, so the fill fails: the volume is kept
    // and recorded in the config, and the error says how to resume or remove it.
    let o = rig.run(&["stage", "frontier", "--dc", "EU-RO-1", "--yes"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        err(&o).contains("staging failed") && err(&o).contains("vol1"),
        "{}",
        err(&o)
    );
    assert_eq!(count(&rig.runpod_hits, "POST /networkvolumes"), 1);
    let listed = rig.run(&["profiles"]);
    assert!(
        out(&listed).contains("network volume vol1"),
        "{}",
        out(&listed)
    );

    let no_yes = rig.run(&["stage", "frontier", "--remove"]);
    assert_eq!(no_yes.status.code(), Some(1), "{}", err(&no_yes));
    assert!(
        err(&no_yes).contains("deletes network volume vol1"),
        "{}",
        err(&no_yes)
    );
    let removed = rig.run(&["stage", "frontier", "--remove", "--yes"]);
    assert_eq!(removed.status.code(), Some(0), "{}", err(&removed));
    assert!(
        out(&removed).contains("deleted volume vol1"),
        "{}",
        out(&removed)
    );
    assert_eq!(count(&rig.runpod_hits, "DELETE /networkvolumes/vol1"), 1);
    assert!(!out(&rig.run(&["profiles"])).contains("network volume vol1"));
}

#[test]
fn up_refuses_a_job_profile_and_a_short_runway() {
    let (rig, _) = rig_with("up-refuse", vec![], 0.5);
    let o = rig.run(&["up", "job"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let o = rig.run(&["up", "small"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("under an hour of runway"), "{}", err(&o));
    assert!(out(&o).contains("cheapest free match"), "{}", out(&o));
    assert_eq!(count(&rig.runpod_hits, "POST /pods"), 0);
}

#[test]
fn up_survives_a_dead_price_api_and_reports_no_capacity() {
    let create_hits = Arc::new(Mutex::new(0u32));
    let seen = Arc::clone(&create_hits);
    let models = model_names();
    let names: Vec<&str> = models.iter().map(String::as_str).collect();
    let rig = Rig::new(
        "up-nocap",
        |c| c.active_profile = "small".into(),
        &names,
        move |route, _| match route {
            "POST /graphql" => (500, "{}".into()),
            "GET /pods" => (200, "[]".into()),
            "POST /pods" => {
                *seen.lock().expect("n") += 1;
                (
                    500,
                    r#"{"error":"create pod: There are no instances currently available"}"#.into(),
                )
            }
            _ => (404, "{}".into()),
        },
    );
    let o = rig.run(&["up", "--wait", "0", "--no-zed"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("could not read GPU prices"), "{text}");
    assert!(text.contains("could not read the balance"), "{text}");
    assert!(err(&o).contains("no capacity"), "{}", err(&o));
    assert_eq!(*create_hits.lock().expect("n"), 1);
}

#[test]
fn up_reports_a_pod_that_never_got_an_address_as_rented_and_not_ready() {
    let models = model_names();
    let names: Vec<&str> = models.iter().map(String::as_str).collect();
    let bare = r#"{"id":"pod1","name":"offrig-small","desiredStatus":"RUNNING","costPerHr":0.25}"#;
    let rig = Rig::new(
        "up-notready",
        |c| c.active_profile = "small".into(),
        &names,
        move |route, body| match route {
            "POST /graphql" if body.contains("myself") => (200, account(50.0)),
            "POST /graphql" => (200, offers(Some(1.0))),
            "GET /pods" => (200, "[]".into()),
            "POST /pods" | "GET /pods/pod1" => (200, bare.into()),
            _ => (404, "{}".into()),
        },
    );
    let mut cmd = rig.command(&["up", "--detach", "--no-zed"]);
    cmd.env("OFFRIG_TEST_POD_READY_MS", "300");
    let o = cmd.output().expect("run offrig");
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    let e = err(&o);
    assert!(e.contains("pod pod1 was rented and billed"), "{e}");
}

#[test]
fn up_stops_when_the_gpus_are_not_free() {
    let models = model_names();
    let names: Vec<&str> = models.iter().map(String::as_str).collect();
    let rig = Rig::new(
        "up-full",
        |c| c.active_profile = "small".into(),
        &names,
        |route, body| match route {
            "POST /graphql" if body.contains("myself") => (200, account(50.0)),
            "POST /graphql" => (200, offers(None)),
            "GET /pods" => (200, "[]".into()),
            _ => (404, "{}".into()),
        },
    );
    let o = rig.run(&["up", "--wait", "0"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        out(&o).contains("free on secure cloud right now"),
        "{}",
        out(&o)
    );
    assert_eq!(
        count(&rig.runpod_hits, "POST /pods"),
        0,
        "no create call when none is free"
    );
}

#[test]
fn up_launches_a_pod_wires_zed_and_detaches() {
    let (rig, pods) = rig_with("up-ok", vec![], 50.0);
    let model = model_names()[0].clone();
    let o = rig.run(&["up", "--detach", "--default-model", &model]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(
        text.contains("pod pod1 created") || text.contains("created at"),
        "{text}"
    );
    assert!(text.contains("tunnel up on 127.0.0.1:"), "{text}");
    assert!(text.contains("all profile models are on the pod"), "{text}");
    assert!(text.contains("Zed provider covtest written to"), "{text}");
    assert!(
        text.contains(&format!("Zed's default agent model is now {model}")),
        "{text}"
    );
    assert!(text.contains("guard:"), "{text}");
    assert!(text.contains("ready: offrig-small (pod1)"), "{text}");
    assert!(text.contains("tunnel closed (--detach)"), "{text}");
    assert_eq!(pods.lock().expect("pods").len(), 1);
    assert_eq!(count(&rig.runpod_hits, "POST /pods"), 1);
    let calls = rig.fake_calls();
    assert!(calls.contains("ssh|echo ok"), "{calls}");
    assert!(
        calls.contains(&format!(
            "tunnel|127.0.0.1:{}:127.0.0.1:11434",
            rig.cfg.tunnel_port
        )),
        "{calls}"
    );
    let settings = std::fs::read_to_string(rig.settings()).expect("zed settings");
    assert!(
        settings.contains("covtest") && settings.contains(&model),
        "{settings}"
    );
    let ssh_config = std::fs::read_to_string(rig.dir.join("home").join(".ssh").join("config"))
        .expect("ssh config");
    assert!(
        ssh_config.contains("203.0.113.7") && ssh_config.contains("40022"),
        "{ssh_config}"
    );
}

#[test]
fn up_reuses_the_pod_that_is_already_there() {
    let (rig, _) = rig_with("up-reuse", vec![lane_pod()], 50.0);
    let o = rig.run(&["up", "--detach", "--no-zed", "--yes"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("offrig-small is already up (pod1)"),
        "{}",
        out(&o)
    );
    assert_eq!(count(&rig.runpod_hits, "POST /pods"), 0);
    assert!(!rig.settings().exists(), "--no-zed leaves Zed alone");
}

#[test]
fn up_pulls_a_missing_model_through_the_pod() {
    let (rig, _) = rig_with("up-pull", vec![lane_pod()], 50.0);
    // Add a model the pod's Ollama does not list yet.
    let mut cfg = small();
    cfg.active_profile = "small".into();
    cfg.tunnel_port = rig.cfg.tunnel_port;
    cfg.zed_provider = "covtest".into();
    cfg.ssh_alias = "covtest-pod".into();
    cfg.role_os_dir = None;
    for p in &mut cfg.profiles {
        if p.name == "small" {
            let mut extra = p.models[0].clone();
            extra.name = "extra:1b".into();
            p.models.push(extra);
        }
    }
    cfg.save_to(&rig.dir.join("cfg").join("config.toml"))
        .expect("save");
    rig.rules(json!([
        {"contains": "NOFILE", "stdout": "NOFILE\n", "limit": 1},
        {"contains": "nohup curl", "stdout": "started\n"},
        {"contains": "tail -c", "stdout": "DEAD\n{\"status\":\"success\"}\n"}
    ]));
    let o = rig.run(&["up", "--detach", "--no-zed", "--yes"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("extra:1b: pull started on the pod"), "{text}");
    assert!(text.contains("extra:1b: pulled"), "{text}");
}

#[test]
fn pull_follows_a_pull_on_the_pod_to_the_end() {
    let (rig, _) = rig_with("pull", vec![lane_pod()], 50.0);
    rig.rules(json!([
        {"contains": "nohup curl", "stdout": "started\n"},
        {"contains": "tail -c", "stdout": "ALIVE\n{\"status\":\"pulling a\",\"total\":2000000000,\"completed\":1000000000}\n", "limit": 1},
        {"contains": "tail -c", "stdout": "DEAD\n{\"status\":\"success\"}\n"}
    ]));
    let o = rig.run(&["pull", "qwen3:8b"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(
        text.contains("pull of qwen3:8b started on the pod"),
        "{text}"
    );
    assert!(text.contains("50% of 2.0 GB"), "{text}");
    assert!(text.contains("qwen3:8b pulled"), "{text}");
}

#[test]
fn pull_reports_a_failed_or_vanished_pull_as_a_runtime_error() {
    let (rig, _) = rig_with("pull-fail", vec![lane_pod()], 50.0);
    rig.rules(json!([
        {"contains": "nohup curl", "stdout": "started\n"},
        {"contains": "tail -c", "stdout": "DEAD\n{\"error\":\"disk full\"}\n"}
    ]));
    let o = rig.run(&["pull", "qwen3:8b"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        err(&o).contains("pull of qwen3:8b failed: disk full"),
        "{}",
        err(&o)
    );
    rig.rules(json!([
        {"contains": "nohup curl", "stdout": "started\n"},
        {"contains": "tail -c", "stdout": "NOFILE\n"}
    ]));
    let o = rig.run(&["pull", "qwen3:8b"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("vanished"), "{}", err(&o));
}

#[test]
fn pull_and_models_need_a_running_pod() {
    let (rig, _) = rig_with("nopod", vec![], 50.0);
    for args in [
        &["pull", "x:1"][..],
        &["models"],
        &["tunnel"],
        &["zed"],
        &["check"],
        &["guard"],
        &["connect"],
    ] {
        let o = rig.run(args);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}", err(&o));
        assert!(
            err(&o).contains("no running pod for profile small"),
            "{args:?}: {}",
            err(&o)
        );
    }
}

#[test]
fn models_lists_what_the_pod_serves() {
    let (rig, _) = rig_with("models", vec![lane_pod()], 50.0);
    rig.rules(json!([
        {"contains": "/v1/models", "stdout": "{\"data\":[{\"id\":\"alpha:1\"},{\"id\":\"beta:2\"}]}"}
    ]));
    let o = rig.run(&["models"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("  alpha:1") && out(&o).contains("  beta:2"),
        "{}",
        out(&o)
    );
}

#[test]
fn zed_writes_and_zed_remove_takes_the_provider_out() {
    let (rig, _) = rig_with("zed", vec![lane_pod()], 50.0);
    let o = rig.run(&["zed"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("Zed provider covtest written to"),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("is set; restart Zed"), "{}", out(&o));
    let settings = std::fs::read_to_string(rig.settings()).expect("settings");
    assert!(
        settings.contains("covtest") && settings.contains("32768"),
        "{settings}"
    );
    let o = rig.run(&["zed-remove"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("removed provider covtest"), "{}", out(&o));
    let settings = std::fs::read_to_string(rig.settings()).expect("settings");
    assert!(!settings.contains("covtest"), "{settings}");
}

#[test]
fn guard_passes_when_the_pod_and_the_tunnel_agree_and_fails_when_they_do_not() {
    let (rig, _) = rig_with("guard", vec![lane_pod()], 50.0);
    assert_eq!(rig.run(&["zed"]).status.code(), Some(0));
    let model = model_names()[0].clone();
    rig.rules(json!([
        {"contains": "/v1/models", "stdout": json!({"data": [{"id": model}]}).to_string()}
    ]));
    let ok = rig.run(&["guard"]);
    assert!(out(&ok).contains("guard:"), "{}\n{}", out(&ok), err(&ok));
    assert!(
        out(&ok).contains("[ok] The tunnel ends at the pod"),
        "{}",
        out(&ok)
    );
    rig.rules(json!([
        {"contains": "/v1/models", "stdout": "{\"data\":[{\"id\":\"some-other-model\"}]}"}
    ]));
    let bad = rig.run(&["guard"]);
    assert_eq!(bad.status.code(), Some(1), "{}\n{}", out(&bad), err(&bad));
    assert!(
        out(&bad).contains("[FAIL] The tunnel ends at the pod"),
        "{}",
        out(&bad)
    );
    assert!(err(&bad).contains("guard checks failed"), "{}", err(&bad));
}

#[test]
fn check_streams_a_chat_with_a_tool_through_the_tunnel() {
    let (rig, _) = rig_with("check", vec![lane_pod()], 50.0);
    let o = rig.run(&["check"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("qwen3:4b: 2 chunks"), "{text}");
    assert!(text.contains("tool call: read_file"), "{text}");
    assert!(text.contains("\"hello\""), "{text}");
    assert!(text.contains("loaded: qwen3:4b (2.0 GB in VRAM)"), "{text}");
    let named = rig.run(&["check", "other:7b", "--profile", "small"]);
    assert!(
        out(&named).contains("other:7b: 2 chunks"),
        "{}",
        out(&named)
    );
}

#[test]
fn connect_opens_the_pod_in_zed() {
    let (rig, _) = rig_with("connect", vec![lane_pod()], 50.0);
    let o = rig.run(&["connect", "/workspace/game"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("opened ssh://covtest-pod/workspace/game in Zed"),
        "{}",
        out(&o)
    );
    // The fake `zed` ran, not a real one.
    let mut waited = 0;
    while !rig
        .fake_calls()
        .contains("ssh://covtest-pod/workspace/game")
        && waited < 50
    {
        std::thread::sleep(std::time::Duration::from_millis(100));
        waited += 1;
    }
    assert!(
        rig.fake_calls()
            .contains("ssh|ssh://covtest-pod/workspace/game"),
        "{}",
        rig.fake_calls()
    );
}

#[test]
fn tunnel_ends_the_pod_after_the_idle_window() {
    let (rig, pods) = rig_with("idle", vec![lane_pod()], 50.0);
    // A zero-minute window: the first idle reading stops the pod.
    let mut cfg = small();
    cfg.active_profile = "small".into();
    cfg.tunnel_port = rig.cfg.tunnel_port;
    cfg.zed_provider = "covtest".into();
    cfg.ssh_alias = "covtest-pod".into();
    cfg.role_os_dir = None;
    cfg.auto_stop_idle_minutes = Some(0);
    cfg.save_to(&rig.dir.join("cfg").join("config.toml"))
        .expect("save");
    rig.rules(json!([
        {"contains": "nvidia-smi", "stdout": "0, Fake GPU, 0, 100, 16000\n"}
    ]));
    let o = rig.run(&["tunnel"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("holding the tunnel on 127.0.0.1:"), "{text}");
    assert!(
        text.contains("every GPU idle for 0 min; terminating offrig-small"),
        "{text}"
    );
    assert!(text.contains("terminated offrig-small (pod1)"), "{text}");
    assert!(pods.lock().expect("pods").is_empty());
}

#[test]
fn tunnel_gives_up_when_the_connection_drops_and_the_pod_is_gone() {
    let pods: Pods = Arc::new(Mutex::new(vec![lane_pod()]));
    let lists = Arc::new(Mutex::new(0u32));
    let n = Arc::clone(&lists);
    let inner = runpod(Arc::clone(&pods), 50.0);
    let models = model_names();
    let names: Vec<&str> = models.iter().map(String::as_str).collect();
    let rig = Rig::new(
        "dropped",
        |c| {
            c.active_profile = "small".into();
            c.auto_stop_idle_minutes = None;
        },
        &names,
        move |route, body| {
            if route == "GET /pods" {
                let mut n = n.lock().expect("n");
                *n += 1;
                // The first listing finds the pod; by the next one it is gone.
                if *n > 1 {
                    return (200, "[]".into());
                }
            }
            inner(route, body)
        },
    );
    let o = rig
        .command(&["tunnel"])
        .env("FAKE_SSH_TUNNEL_LIFETIME_MS", "1500")
        .output()
        .expect("run");
    assert_eq!(o.status.code(), Some(2), "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("tunnel dropped"), "{}", out(&o));
    assert!(err(&o).contains("the pod is gone"), "{}", err(&o));
}

#[test]
fn a_failing_tunnel_is_reported_with_ssh_s_own_words() {
    let (rig, _) = rig_with("tunnel-fail", vec![lane_pod()], 50.0);
    std::fs::write(rig.fake.join("tunnel_fail"), "").expect("flag");
    let o = rig.run(&["zed"]);
    assert_eq!(o.status.code(), Some(2), "{}\n{}", out(&o), err(&o));
    assert!(
        err(&o).contains("tunnel exited") && err(&o).contains("connect failed"),
        "{}",
        err(&o)
    );
}
