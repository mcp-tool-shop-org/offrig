//! Issue #26, end to end over stdio against a mock RunPod: `offrig_status` names other
//! lanes' pods (lane, project, pod, gpu, price, status) with the open plan read from that
//! project's own store, read-only; pods offrig did not create stay a count and names; a
//! store that cannot be read degrades to a note; and a pod a plan creates carries
//! `OFFRIG_LANE`, `OFFRIG_PLAN` and `OFFRIG_DEADLINE` beside the profile's own env.
//!
//! Nothing here reaches RunPod, ssh or the real config dir: every lane registry, store
//! and project is in a temp dir.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

use common::{mock, temp};
use offrig_core::config::Config;
use offrig_core::lanes::Registry;
use offrig_core::store::{NewPlan, Store};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

const RTX_S: &str = "NVIDIA RTX PRO 6000 Blackwell Server Edition";

type Client = RunningService<RoleClient, ()>;

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

async fn start(project: &Path, cfg: &Path, url: &str) -> Client {
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(project);
        c.env("RUNPOD_API_KEY", "test-key");
        c.env("OFFRIG_CONFIG_DIR", cfg);
        c.env("OFFRIG_TEST_RUNPOD_BASE", url);
        c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
        c.env_remove("OPENROUTER_API_KEY");
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

fn graphql(body: &str) -> String {
    if body.contains("myself") {
        return r#"{"data":{"myself":{"clientBalance":50.0,"currentSpendPerHr":0.0,"spendLimit":80}}}"#
            .into();
    }
    format!(
        r#"{{"gpus":[{{"id":"{RTX_S}","name":"RTX PRO 6000","memory":96,"secure":true,"price":{{"secure":2.09}},"maxCount":{{"secure":8}},"availability":"HIGH"}}]}}"#
    )
}

fn db(project: &Path) -> PathBuf {
    project.join(".offrig").join("offrig.db")
}

/// A project folder with its lane allocated in the shared registry. Returns its tag.
fn lane(cfg: &Path, project: &Path) -> String {
    Registry::at(cfg)
        .lane_for_project(project, &Config::default())
        .expect("lane")
        .tag
        .expect("tag")
}

/// A sibling project: lane, funded store, and one committed plan with a note.
fn sibling_with_plan(cfg: &Path, name: &str, note: &str) -> (PathBuf, String, i64) {
    let dir = temp(name);
    let tag = lane(cfg, &dir);
    let s = Store::open(&db(&dir)).expect("store");
    s.set_budget_cap(500.0).expect("cap");
    let plan = s
        .create_plan(NewPlan {
            profile: "job".into(),
            gpu_count: 1,
            gpu_types: vec![RTX_S.into()],
            max_hours: 2.0,
            max_price_hr: 2.09,
            note: Some(note.into()),
        })
        .expect("plan");
    let plan = s.commit_plan(plan.id).expect("commit");
    s.attach_pod(plan.id, "sib-pod").expect("attach");
    (dir, tag, plan.id)
}

fn pod(id: &str, name: &str, price: f64) -> String {
    format!(
        r#"{{"id":"{id}","name":"{name}","desiredStatus":"RUNNING","costPerHr":{price},"machine":{{"gpuTypeId":"{RTX_S}"}}}}"#
    )
}

/// Every file in the store's folder with its bytes, to show nothing was touched.
fn snapshot(project: &Path) -> Vec<(String, u64, Vec<u8>)> {
    let mut out: Vec<(String, u64, Vec<u8>)> = std::fs::read_dir(project.join(".offrig"))
        .expect("read dir")
        .filter(|e| {
            // Reading through SQLite's normal locking may add -wal and -shm files.
            let n = e.as_ref().expect("entry").file_name();
            let n = n.to_string_lossy();
            !(n.ends_with("-wal") || n.ends_with("-shm"))
        })
        .map(|e| {
            let e = e.expect("entry");
            let bytes = std::fs::read(e.path()).unwrap_or_default();
            (
                e.file_name().to_string_lossy().into_owned(),
                bytes.len() as u64,
                bytes,
            )
        })
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn a_sibling_lanes_pod_appears_with_its_plan_and_foreign_pods_are_only_counted() {
    let root = temp("sibling-status");
    let cfg = root.join("cfg");
    let (sib, sib_tag, sib_plan) =
        sibling_with_plan(&cfg, "sibling-a", "cut 40 clips for the trailer");
    let me = root.join("me");
    std::fs::create_dir_all(&me).expect("me");
    let pods = format!(
        "[{},{},{}]",
        pod("sib-pod", &format!("offrig-{sib_tag}-job"), 2.09),
        pod("plain-pod", "offrig-small", 0.25),
        pod("not-ours", "somebody-elses", 1.5),
    );
    let (url, _h) = mock(move |route, b, _| match route {
        "GET /catalog/gpus" => (200, graphql("")),
        "POST /graphql" => (200, graphql(b)),
        "GET /pods" => (200, pods.clone()),
        _ => (404, "{}".into()),
    });
    let before = snapshot(&sib);
    let client = start(&me, &cfg, &url).await;
    let st = body(&call(&client, "offrig_status", json!({})).await);
    client.cancel().await.expect("shutdown");

    let run = &st["runpod"];
    let sibs = run["sibling_pods"].as_array().expect("sibling pods");
    assert_eq!(
        sibs.len(),
        2,
        "the sibling lane's and the plain lane's: {run}"
    );
    let s = sibs.iter().find(|x| x["lane"] == sib_tag).expect("sibling");
    assert_eq!(s["pod"], format!("offrig-{sib_tag}-job"));
    assert_eq!(s["pod_id"], "sib-pod");
    assert_eq!(s["gpu"], RTX_S);
    assert_eq!(s["cost_per_hr"], 2.09);
    assert_eq!(s["status"], "RUNNING");
    assert_eq!(
        s["project"].as_str().map(str::to_lowercase),
        Some(offrig_core::lanes::project_key(&sib).to_lowercase())
    );
    let plan = &s["plan"];
    assert_eq!(plan["plan_id"], sib_plan);
    assert_eq!(plan["note"], "cut 40 clips for the trailer");
    assert_eq!(plan["committed_worst_case"], 4.18);
    let deadline = plan["deadline"].as_str().expect("deadline");
    assert!(
        deadline.len() == 20 && deadline.ends_with('Z') && deadline.as_bytes()[10] == b'T',
        "UTC ISO: {deadline}"
    );
    assert!(s["plan_note"].is_null());

    // The plain lane's pod is listed as lane "plain", which has no project to read.
    let plain = sibs.iter().find(|x| x["lane"] == "plain").expect("plain");
    assert_eq!(plain["pod"], "offrig-small");
    assert!(plain["project"].is_null());
    assert!(plain["plan"].is_null());
    assert!(
        plain["plan_note"]
            .as_str()
            .is_some_and(|n| n.contains("no one project"))
    );

    // A pod offrig did not create is only counted, with its name.
    assert_eq!(run["other_pods"], 1);
    assert_eq!(run["other_pod_names"], json!(["somebody-elses"]));
    assert!(
        run["offrig_pods"].as_array().expect("ours").is_empty(),
        "nothing here is this project's"
    );

    // Reading the sibling changed nothing in its store folder: every file is
    // byte-identical, and no side file but SQLite's own -wal and -shm appeared.
    let names = |v: &[(String, u64, Vec<u8>)]| {
        v.iter()
            .map(|(n, len, _)| format!("{n}:{len}"))
            .collect::<Vec<_>>()
    };
    let after = snapshot(&sib);
    assert_eq!(names(&after), names(&before), "files and sizes");
    assert!(after == before, "the sibling's store is untouched");
    let _ = std::fs::remove_dir_all(&sib);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_unreadable_sibling_store_degrades_to_a_note_and_status_still_answers() {
    let root = temp("sibling-degrade");
    let cfg = root.join("cfg");
    // No store at all.
    let none = temp("sibling-nostore");
    let none_tag = lane(&cfg, &none);
    // A file that is not a database.
    let junk = temp("sibling-junk");
    let junk_tag = lane(&cfg, &junk);
    let junk_bytes: &[u8] =
        b"this is not an sqlite database, not even close, so it is long enough to be a page header";
    std::fs::create_dir_all(junk.join(".offrig")).expect("dir");
    std::fs::write(db(&junk), junk_bytes).expect("junk");
    // A real store with no open plan.
    let idle = temp("sibling-idle");
    let idle_tag = lane(&cfg, &idle);
    Store::open(&db(&idle)).expect("store");
    let me = root.join("me");
    std::fs::create_dir_all(&me).expect("me");

    let pods = format!(
        "[{},{},{}]",
        pod("p1", &format!("offrig-{none_tag}-job"), 1.0),
        pod("p2", &format!("offrig-{junk_tag}-job"), 1.0),
        pod("p3", &format!("offrig-{idle_tag}-job"), 1.0),
    );
    let (url, _h) = mock(move |route, b, _| match route {
        "GET /catalog/gpus" => (200, graphql("")),
        "POST /graphql" => (200, graphql(b)),
        "GET /pods" => (200, pods.clone()),
        _ => (404, "{}".into()),
    });
    let client = start(&me, &cfg, &url).await;
    let r = call(&client, "offrig_status", json!({})).await;
    assert_eq!(r.is_error, Some(false), "{:?}", body(&r));
    let st = body(&r);
    client.cancel().await.expect("shutdown");
    let sibs = st["runpod"]["sibling_pods"].as_array().expect("siblings");
    assert_eq!(sibs.len(), 3);
    for (tag, why) in [
        (&none_tag, "could not be read"),
        (&junk_tag, "could not be read"),
        (&idle_tag, "no open plan"),
    ] {
        let s = sibs.iter().find(|x| x["lane"] == *tag).expect("sibling");
        assert!(s["plan"].is_null(), "{tag}: {s}");
        let note = s["plan_note"].as_str().expect("a note says why");
        assert!(note.contains(why), "{tag}: {note}");
        assert_eq!(s["status"], "RUNNING", "the pod is still listed");
    }
    // The unreadable stores were not repaired, created or replaced.
    assert!(!db(&none).exists(), "no store was created");
    assert_eq!(std::fs::read(db(&junk)).expect("junk"), junk_bytes);
    for d in [&none, &junk, &idle, &root] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[tokio::test]
async fn a_plans_pod_is_created_with_lane_plan_and_deadline_in_its_env() {
    let created: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = Arc::clone(&created);
    let pod_name: Arc<Mutex<String>> = Arc::default();
    let name = Arc::clone(&pod_name);
    let (url, _h) = mock(move |route, b, _| {
        let pod = |id: &str| {
            format!(
                r#"{{"id":"{id}","name":"{}","desiredStatus":"RUNNING","costPerHr":2.09}}"#,
                name.lock().expect("name")
            )
        };
        match route {
            "GET /catalog/gpus" => (200, graphql("")),
            "POST /graphql" => (200, graphql(b)),
            "GET /pods" => (200, "[]".into()),
            "POST /pods" => {
                log.lock().expect("log").push(b.to_string());
                (200, pod("p1"))
            }
            "GET /pods/p1" | "DELETE /pods/p1" => (200, pod("p1")),
            _ => (404, "{}".into()),
        }
    });
    let root = temp("sibling-env");
    let cfg = root.join("cfg");
    let me = root.join("me");
    std::fs::create_dir_all(&me).expect("me");
    Store::open(&db(&me))
        .expect("store")
        .set_budget_cap(100.0)
        .expect("cap");
    let client = start(&me, &cfg, &url).await;
    let p = call(
        &client,
        "offrig_plan",
        json!({"profile": "job", "max_hours": 1.5, "no_fallback": true}),
    )
    .await;
    assert_eq!(p.is_error, Some(false), "{:?}", body(&p));
    let tag = body(&p)["lane"].as_str().expect("lane").to_string();
    *pod_name.lock().expect("name") = body(&p)["pod_name"].as_str().expect("name").to_string();
    let plan_id = body(&p)["plan_id"].as_i64().expect("plan id");
    let l = call(&client, "offrig_launch", json!({"plan_id": plan_id})).await;
    assert_eq!(l.is_error, Some(false), "{:?}", body(&l));
    let started = Instant::now();
    let sent = loop {
        if let Some(s) = created.lock().expect("log").first().cloned() {
            break s;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "no pod create was sent"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let _ = call(&client, "offrig_shutdown", json!({"plan_id": plan_id})).await;
    client.cancel().await.expect("shutdown");

    let sent: Value = serde_json::from_str(&sent).expect("create body");
    let env = &sent["env"];
    assert_eq!(env["OFFRIG_LANE"], tag, "{sent}");
    assert_eq!(env["OFFRIG_PLAN"], plan_id.to_string(), "{sent}");
    let deadline = env["OFFRIG_DEADLINE"].as_str().expect("deadline");
    assert!(
        deadline.len() == 20 && deadline.ends_with('Z') && deadline.as_bytes()[10] == b'T',
        "UTC ISO: {deadline}"
    );
    assert!(
        env["OFFRIG_JOB_DIR"].is_string() && env["HF_HOME"].is_string(),
        "the profile's own env stays: {sent}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn status_carries_codes_not_paths_and_the_uncounted_disclaimer() {
    // The sentinel is in the name of the directory everything lives in.
    let root = temp("sentinelxyz-status");
    let cfg = root.join("cfg");
    let me = root.join("me");
    std::fs::create_dir_all(&me).expect("me");
    let reg = offrig_core::account::ProjectsRegistry::at(&cfg);
    // A registered project with no store at all.
    reg.add(&root.join("gone")).expect("gone");
    // A registered project whose store is not a database.
    let junk = root.join("junk");
    std::fs::create_dir_all(junk.join(".offrig")).expect("dir");
    std::fs::write(
        db(&junk),
        b"this is not an sqlite database, not even close, not at all",
    )
    .expect("junk");
    reg.add(&junk).expect("junk reg");
    // RunPod answers 500, so its balance is unknown; there is no OpenRouter key.
    let (url, _h) = mock(move |route, _b, _| match route {
        "GET /pods" => (200, "[]".into()),
        _ => (500, "{}".into()),
    });
    let client = start(&me, &cfg, &url).await;
    let r = call(&client, "offrig_status", json!({})).await;
    assert_eq!(r.is_error, Some(false), "{:?}", body(&r));
    let st = body(&r);
    client.cancel().await.expect("shutdown");
    let text = st.to_string().to_lowercase();
    assert!(!text.contains("sentinelxyz"), "no path in status: {st}");
    assert!(!text.contains("\\\\"), "no windows path in status: {st}");
    for p in ["runpod", "openrouter"] {
        let notes = st["account"][p]["notes"].as_array().expect("notes");
        let notes: Vec<&str> = notes.iter().filter_map(Value::as_str).collect();
        assert!(notes.contains(&"gone: no_store"), "{p}: {notes:?}");
        assert!(notes.contains(&"junk: unreadable"), "{p}: {notes:?}");
    }
    assert_eq!(st["budgets"]["openrouter"]["balance_note"], "no_key");
    assert_eq!(st["budgets"]["runpod"]["balance_note"], "http_error");
    assert_eq!(
        st["uncounted"],
        json!(["manual pods (offrig up, the app)"]),
        "{st}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
