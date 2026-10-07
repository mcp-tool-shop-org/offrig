//! Issues #9 and #10 end to end against a mock RunPod: `offrig_plan` narrows the
//! profile's GPU list by `max_price_hr`, `no_fallback` and the profile's `min_vram_gb`;
//! the worst case follows; the plan stores the list; the launch rents only from it;
//! and `offrig_job` shouts when the rented host is older than the plan's CUDA floor.
//! The mock never reports an ssh endpoint, so no ssh config is touched.

mod common;

use std::sync::{Arc, Mutex};

use common::{mock, temp};
use offrig_core::config::Config;
use offrig_core::store::Store;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

const RTX_S: &str = "NVIDIA RTX PRO 6000 Blackwell Server Edition";
const RTX_W: &str = "NVIDIA RTX PRO 6000 Blackwell Workstation Edition";

/// The `job` profile's cards, as the issues describe the market: the RTX PRO 6000 at
/// $2.09 first, the 80 GB fallbacks, the H100 at $3.49 (the profile's top price).
fn graphql(body: &str) -> String {
    if body.contains("myself") {
        return r#"{"data":{"myself":{"clientBalance":50.0,"currentSpendPerHr":0.0,"spendLimit":80}}}"#
            .into();
    }
    let t = |id: &str, name: &str, gb: u32, price: f64| {
        format!(
            r#"{{"id":"{id}","displayName":"{name}","memoryInGb":{gb},"secureCloud":true,"lowestPrice":{{"uninterruptablePrice":{price},"stockStatus":"High"}}}}"#
        )
    };
    let all = [
        t(RTX_S, "RTX PRO 6000", 96, 2.09),
        t(RTX_W, "RTX PRO 6000 WK", 96, 1.99),
        t("NVIDIA A100-SXM4-80GB", "A100 SXM", 80, 1.89),
        t("NVIDIA A100 80GB PCIe", "A100 PCIe", 80, 1.64),
        t("NVIDIA H100 NVL", "H100 NVL", 94, 3.07),
        t("NVIDIA H100 80GB HBM3", "H100", 80, 3.49),
    ];
    format!(r#"{{"data":{{"gpuTypes":[{}]}}}}"#, all.join(","))
}

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

type Client = RunningService<RoleClient, ()>;

async fn start(dir: &std::path::Path, url: &str) -> Client {
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(dir);
        c.env("RUNPOD_API_KEY", "test-key");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        c.env("OFFRIG_TEST_RUNPOD_BASE", url);
        c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
    });
    ().serve(TokioChildProcess::new(cmd).expect("spawn"))
        .await
        .expect("connect")
}

async fn call(client: &Client, name: &'static str, a: Value) -> CallToolResult {
    client
        .call_tool(
            CallToolRequestParams::new(name).with_arguments(a.as_object().cloned().expect("obj")),
        )
        .await
        .expect("call")
}

fn project(name: &str) -> std::path::PathBuf {
    let dir = temp(name);
    let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
    s.set_budget_cap(100.0).expect("cap");
    dir
}

#[tokio::test]
async fn plan_limits_narrow_the_gpu_list_and_the_worst_case() {
    let (url, _hits) = mock(|route, b, _| match route {
        "POST /graphql" => (200, graphql(b)),
        _ => (404, "{}".into()),
    });
    let dir = project("plan-limits");
    let client = start(&dir, &url).await;

    // No limits: as before, the worst case is the profile's top listed price.
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 3.5}),
    )
    .await;
    assert_eq!(p.is_error, Some(false), "{:?}", body(&p));
    assert_eq!(body(&p)["max_price_hr"], 3.49);
    assert_eq!(body(&p)["worst_case"], 12.22, "the issue's number");
    assert_eq!(body(&p)["min_cuda"], "13.0", "the job profile's floor");

    // max_price_hr: the H100s are left out and the worst case follows the cap's side.
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 3.5, "max_price_hr": 2.2}),
    )
    .await;
    assert_eq!(p.is_error, Some(false), "{:?}", body(&p));
    let b = body(&p);
    let gpus = b["gpus"].as_str().expect("gpus");
    assert!(
        gpus.contains("RTX PRO 6000") && gpus.contains("A100"),
        "{gpus}"
    );
    assert!(!gpus.contains("H100"), "{gpus}");
    assert_eq!(b["max_price_hr"], 2.09);
    assert_eq!(b["worst_case"], 7.32);
    assert!(
        b["left_out"]
            .as_array()
            .is_some_and(|l| l.iter().any(|x| x["gpu"] == "NVIDIA H100 80GB HBM3")),
        "{b:?}"
    );

    // no_fallback: the two RTX PRO 6000 editions only.
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "no_fallback": true}),
    )
    .await;
    assert_eq!(p.is_error, Some(false), "{:?}", body(&p));
    let gpus = body(&p)["gpus"].as_str().expect("gpus").to_string();
    assert_eq!(gpus, format!("1x {RTX_S} | {RTX_W}"));
    assert_eq!(body(&p)["max_price_hr"], 2.09);

    // Nothing remains: refused, naming why, and no plan was written.
    let before = Store::open(&dir.join(".offrig").join("offrig.db"))
        .expect("store")
        .budget()
        .expect("budget")
        .committed;
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "max_price_hr": 0.5}),
    )
    .await;
    assert_eq!(p.is_error, Some(true));
    let err = body(&p)["error"].as_str().expect("error").to_string();
    assert!(err.contains("above max_price_hr"), "{err}");
    let bad = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "max_price_hr": -2.0}),
    )
    .await;
    assert_eq!(bad.is_error, Some(true));
    assert_eq!(before, 0.0);

    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_profiles_min_vram_gb_drops_the_smaller_cards() {
    let (url, _hits) = mock(|route, b, _| match route {
        "POST /graphql" => (200, graphql(b)),
        _ => (404, "{}".into()),
    });
    let dir = project("plan-vram");
    let mut cfg = Config::default();
    for p in &mut cfg.profiles {
        if p.name == "job" {
            p.min_vram_gb = Some(90);
        }
    }
    std::fs::create_dir_all(dir.join("cfg")).expect("cfg dir");
    cfg.save_to(&dir.join("cfg").join("config.toml"))
        .expect("save config");
    let client = start(&dir, &url).await;
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0}),
    )
    .await;
    assert_eq!(p.is_error, Some(false), "{:?}", body(&p));
    let gpus = body(&p)["gpus"].as_str().expect("gpus").to_string();
    assert!(gpus.contains("H100 NVL"), "{gpus}");
    assert!(!gpus.contains("A100") && !gpus.contains("HBM3"), "{gpus}");
    assert_eq!(body(&p)["max_price_hr"], 3.07, "the dearest 90 GB+ card");
    assert_eq!(body(&p)["min_vram_gb"], 90);

    // A floor nothing meets refuses the plan.
    for p in &mut cfg.profiles {
        if p.name == "job" {
            p.min_vram_gb = Some(500);
        }
    }
    client.cancel().await.expect("shutdown");
    cfg.save_to(&dir.join("cfg").join("config.toml"))
        .expect("save config");
    let client = start(&dir, &url).await;
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0}),
    )
    .await;
    assert_eq!(p.is_error, Some(true));
    assert!(
        body(&p)["error"]
            .as_str()
            .is_some_and(|e| e.contains("min_vram_gb")),
        "{:?}",
        body(&p)
    );
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_launch_rents_only_from_the_plans_stored_gpu_list_and_cuda_floor() {
    let created: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = Arc::clone(&created);
    let (url, _hits) = mock(move |route, b, _| match route {
        "POST /graphql" => (200, graphql(b)),
        "GET /pods" => (200, "[]".into()),
        "POST /pods" => {
            log.lock().expect("log").push(b.to_string());
            // No ssh endpoint: the launch waits for an address, touching no ssh config.
            (
                200,
                r#"{"id":"p1","name":"offrig-x","desiredStatus":"RUNNING","costPerHr":2.09}"#
                    .into(),
            )
        }
        "GET /pods/p1" | "DELETE /pods/p1" => (
            200,
            r#"{"id":"p1","name":"offrig-x","desiredStatus":"RUNNING","costPerHr":2.09}"#.into(),
        ),
        _ => (404, "{}".into()),
    });
    let dir = project("plan-launch");
    let client = start(&dir, &url).await;
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.0, "max_price_hr": 2.2, "no_fallback": true}),
    )
    .await;
    assert_eq!(p.is_error, Some(false), "{:?}", body(&p));
    let plan_id = body(&p)["plan_id"].as_i64().expect("plan id");

    // The plan holds the narrowed list and the floor, not the profile's six cards.
    {
        let s = Store::open(&dir.join(".offrig").join("offrig.db")).expect("store");
        let plan = s.plan(plan_id).expect("read").expect("plan");
        assert_eq!(plan.gpu_types, [RTX_S, RTX_W]);
        assert_eq!(plan.max_price_hr, 2.09);
        assert_eq!(s.plan_min_cuda(plan_id).expect("read"), Some("13.0".into()));
    }

    let l = call(&client, "offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(l.is_error, Some(false), "{:?}", body(&l));
    let started = std::time::Instant::now();
    let sent = loop {
        if let Some(s) = created.lock().expect("log").first().cloned() {
            break s;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "no pod create was sent"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    let sent: Value = serde_json::from_str(&sent).expect("create body");
    assert_eq!(sent["gpuTypeIds"], json!([RTX_S, RTX_W]), "{sent}");
    assert_eq!(sent["allowedCudaVersions"], json!(["13.0"]), "{sent}");

    let _ = call(&client, "offrig_shutdown", json!({"plan_id": plan_id})).await;
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn offrig_job_repeats_a_rental_warning_at_the_top_and_in_next_action() {
    let (url, _hits) = mock(|_, _, _| (404, "{}".into()));
    let dir = project("plan-warn");
    let db = dir.join(".offrig").join("offrig.db");
    let (plan_id, job_id) = {
        let s = Store::open(&db).expect("store");
        let plan = s
            .create_plan(offrig_core::store::NewPlan {
                profile: "job".into(),
                gpu_count: 1,
                gpu_types: vec![RTX_S.into()],
                max_hours: 1.0,
                max_price_hr: 2.09,
                note: None,
            })
            .expect("plan");
        let job = s.create_job(plan.id, "launch").expect("job");
        let rented = offrig_core::planning::audit_rental(
            &serde_json::from_value(json!({
                "id": "p1",
                "costPerHr": 1.89,
                "machine": {"gpuTypeId": "NVIDIA A100-SXM4-80GB", "cudaVersion": "12.8"},
            }))
            .expect("pod"),
            &[RTX_S.to_string()],
            2.09,
            Some("13.0"),
        );
        s.update_job(
            job,
            "running",
            &json!({"step": "waiting", "rented": rented.to_json()}),
            None,
        )
        .expect("progress");
        (plan.id, job)
    };
    let client = start(&dir, &url).await;
    let j = call(&client, "offrig_job", json!({"job_id": job_id})).await;
    assert_eq!(j.is_error, Some(false), "{:?}", body(&j));
    let b = body(&j);
    assert_eq!(b["plan_id"], plan_id);
    assert_eq!(b["rented"]["gpu"], "NVIDIA A100-SXM4-80GB");
    assert_eq!(b["rented"]["cuda_version"], "12.8");
    let warnings = b["warnings"].as_array().expect("warnings");
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .is_some_and(|w| w.starts_with("HOST CUDA TOO OLD"))),
        "{b:?}"
    );
    assert!(
        b["next_action"]
            .as_str()
            .is_some_and(|n| n.starts_with("WARNING")),
        "{b:?}"
    );
    assert_eq!(
        b["plan_state"], "planned",
        "nothing was terminated or closed"
    );
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}
