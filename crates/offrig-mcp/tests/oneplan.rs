//! One lane, one live plan (offrig#7), end to end over stdio against a mock RunPod:
//! a second launch on a lane that already has a live plan or pod is refused, and the job
//! tools (`offrig_exec`, `offrig_put`, `offrig_get`) take a `plan_id`, refuse to guess
//! between several open job plans, and act only on the pod the named plan owns.
//!
//! Every pod the mock hands out has no ssh endpoint, so a call that gets past the
//! selection and ownership checks stops at "no ssh endpoint" naming the pod it chose,
//! before anything is written to an ssh config or run over ssh. Nothing here reaches
//! RunPod, ssh or the real config dir.

use std::path::Path;

mod common;

use common::{Hits, count, mock, temp};
use offrig_core::config::Config;
use offrig_core::lanes::Registry;
use offrig_core::store::{NewPlan, Store};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

fn error_of(r: &CallToolResult) -> String {
    assert_eq!(r.is_error, Some(true), "{:?}", body(r));
    body(r)["error"].as_str().expect("error text").to_string()
}

fn pod_json(id: &str, name: &str) -> String {
    format!(r#"{{"id":"{id}","name":"{name}","desiredStatus":"RUNNING","costPerHr":0.25}}"#)
}

struct Lane {
    dir: std::path::PathBuf,
    tag: String,
}

/// A project folder with its lane allocated in a temp registry and a funded store.
fn lane(name: &str) -> Lane {
    let dir = temp(name);
    let lane = Registry::at(dir.join("cfg"))
        .lane_for_project(&dir, &Config::default())
        .expect("lane");
    let s = store(&dir);
    s.set_budget_cap(500.0).expect("cap");
    Lane {
        dir,
        tag: lane.tag.expect("tag"),
    }
}

fn store(dir: &Path) -> Store {
    Store::open(&dir.join(".offrig").join("offrig.db")).expect("store")
}

fn planned(l: &Lane, profile: &str) -> i64 {
    let s = store(&l.dir);
    let p = s
        .create_plan(NewPlan {
            profile: profile.into(),
            gpu_count: 1,
            gpu_types: vec![],
            max_hours: 1.0,
            max_price_hr: 0.5,
            note: None,
        })
        .expect("plan");
    s.set_plan_lane(p.id, &l.tag).expect("lane");
    p.id
}

/// An open plan whose pod is `pod_id`.
fn open(l: &Lane, profile: &str, pod_id: &str) -> i64 {
    let id = planned(l, profile);
    let s = store(&l.dir);
    s.commit_plan(id).expect("commit");
    s.attach_pod(id, pod_id).expect("attach");
    id
}

async fn connect(l: &Lane, url: &str) -> RunningService<RoleClient, ()> {
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(&l.dir);
        c.env("RUNPOD_API_KEY", "test-key");
        c.env("OFFRIG_CONFIG_DIR", l.dir.join("cfg"));
        c.env("OFFRIG_TEST_RUNPOD_BASE", url);
        c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
    });
    ().serve(TokioChildProcess::new(cmd).expect("spawn"))
        .await
        .expect("connect")
}

async fn call(c: &RunningService<RoleClient, ()>, name: &'static str, a: Value) -> CallToolResult {
    c.call_tool(
        CallToolRequestParams::new(name).with_arguments(a.as_object().cloned().expect("object")),
    )
    .await
    .expect("call")
}

fn posts(hits: &Hits) -> usize {
    count(hits, "POST /pods")
}

#[tokio::test]
async fn a_second_launch_on_a_lane_with_a_live_plan_is_refused() {
    let l = lane("oneplan-launch");
    let live = open(&l, "job", "p-job");
    let second = planned(&l, "jam");
    let (url, hits) = mock(|route, _, _| match route {
        "GET /pods" => (200, "[]".into()),
        "POST /pods" => (200, pod_json("p-jam", "offrig-never-created")),
        _ => (404, "{}".into()),
    });
    let client = connect(&l, &url).await;

    let r = call(&client, "offrig_launch", json!({"plan_id": second})).await;
    let e = error_of(&r);
    assert!(
        e.contains(&format!(
            "lane {0} has a live pod offrig-{0}-job (plan {live}); shut it down first",
            l.tag
        )),
        "{e}"
    );
    // Nothing was rented or committed: the plan is still only planned.
    assert_eq!(posts(&hits), 0);
    assert_eq!(
        store(&l.dir)
            .plan(second)
            .expect("read")
            .expect("plan")
            .state,
        "planned"
    );
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[tokio::test]
async fn a_live_pod_of_the_lane_refuses_a_launch_even_with_no_open_plan() {
    let l = lane("oneplan-pod");
    let second = planned(&l, "jam");
    let name = format!("offrig-{}-job", l.tag);
    let live = pod_json("p-job", &name);
    let other = pod_json("p-other", "offrig-someone-else-job");
    let (url, hits) = mock(move |route, _, _| match route {
        "GET /pods" => (200, format!("[{live},{other}]")),
        "POST /pods" => (200, pod_json("p-jam", "offrig-never-created")),
        _ => (404, "{}".into()),
    });
    let client = connect(&l, &url).await;

    let r = call(&client, "offrig_launch", json!({"plan_id": second})).await;
    let e = error_of(&r);
    assert!(
        e.contains(&format!(
            "lane {} has a live pod {name} (no open plan); shut it down first",
            l.tag
        )),
        "{e}"
    );
    assert_eq!(posts(&hits), 0);
    assert_eq!(
        store(&l.dir)
            .plan(second)
            .expect("read")
            .expect("plan")
            .state,
        "planned"
    );
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[tokio::test]
async fn job_tools_refuse_to_guess_between_open_job_plans() {
    let l = lane("oneplan-ambiguous");
    let job = open(&l, "job", "p-job");
    let jam = open(&l, "jam", "p-jam");
    let (url, hits) = mock(|_, _, _| (404, "{}".into()));
    let client = connect(&l, &url).await;

    let calls = [
        ("offrig_exec", json!({"action": "status", "name": "render"})),
        ("offrig_put", json!({"local": "in.txt", "pod": "in.txt"})),
        ("offrig_get", json!({"local": "out.txt", "pod": "out.txt"})),
    ];
    for (tool, args) in calls {
        let e = error_of(&call(&client, tool, args).await);
        assert!(
            e.contains("2 open job plans") && e.contains("pass plan_id"),
            "{tool}: {e}"
        );
        assert!(
            e.contains(&format!("plan {job} (job, pod offrig-{}-job)", l.tag)),
            "{tool}: {e}"
        );
        assert!(
            e.contains(&format!("plan {jam} (jam, pod offrig-{}-jam)", l.tag)),
            "{tool}: {e}"
        );
    }
    // Refused before any pod was looked up.
    assert!(hits.lock().expect("hits").is_empty());
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[tokio::test]
async fn plan_id_acts_only_on_the_named_plans_own_pod() {
    let l = lane("oneplan-select");
    let job = open(&l, "job", "p-job");
    let jam = open(&l, "jam", "p-jam");
    let job_name = format!("offrig-{}-job", l.tag);
    let jam_name = format!("offrig-{}-jam", l.tag);
    let (j, m) = (pod_json("p-job", &job_name), pod_json("p-jam", &jam_name));
    // The pod behind p-bad carries the other plan's name: the plan does not own it.
    let swapped = pod_json("p-bad", &jam_name);
    let (url, hits) = mock(move |route, _, _| match route {
        "GET /pods/p-job" => (200, j.clone()),
        "GET /pods/p-jam" => (200, m.clone()),
        "GET /pods/p-bad" => (200, swapped.clone()),
        _ => (404, "{}".into()),
    });
    let client = connect(&l, &url).await;

    // Each plan_id reaches its own pod, and only that one (then stops at the missing
    // ssh endpoint, which names the pod it chose).
    for (plan, pod_id, name) in [(jam, "p-jam", &jam_name), (job, "p-job", &job_name)] {
        let before_job = count(&hits, "GET /pods/p-job");
        let before_jam = count(&hits, "GET /pods/p-jam");
        let r = call(
            &client,
            "offrig_exec",
            json!({"action": "status", "name": "render", "plan_id": plan}),
        )
        .await;
        let e = error_of(&r);
        assert!(
            e.contains(&format!("pod {name} has no ssh endpoint")),
            "{e}"
        );
        let asked = |id: &str, before: usize| count(&hits, &format!("GET /pods/{id}")) - before;
        let (want, other) = if pod_id == "p-job" {
            (asked("p-job", before_job), asked("p-jam", before_jam))
        } else {
            (asked("p-jam", before_jam), asked("p-job", before_job))
        };
        assert_eq!((want, other), (1, 0), "{pod_id}");
    }

    // A single open job plan with no plan_id still works as before.
    store(&l.dir).close_plan(jam, 0.0).expect("close jam");
    let r = call(
        &client,
        "offrig_put",
        json!({"local": "in.txt", "pod": "in.txt"}),
    )
    .await;
    let e = error_of(&r);
    assert!(
        e.contains(&format!("pod {job_name} has no ssh endpoint")),
        "{e}"
    );

    // A plan whose recorded pod is not the pod it owns is refused for any tool.
    let bad = open(&l, "job", "p-bad");
    store(&l.dir).close_plan(job, 0.0).expect("close job");
    for tool in ["offrig_exec", "offrig_put", "offrig_get"] {
        let args = match tool {
            "offrig_exec" => json!({"action": "status", "name": "render", "plan_id": bad}),
            _ => json!({"local": "f.txt", "pod": "f.txt", "plan_id": bad}),
        };
        let e = error_of(&call(&client, tool, args).await);
        assert!(
            e.contains(&format!("is not the pod plan {bad} owns ({job_name})")),
            "{tool}: {e}"
        );
    }
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[tokio::test]
async fn status_states_the_project_and_each_open_plans_lane_and_pod() {
    let l = lane("oneplan-status");
    let job = open(&l, "job", "p-job");
    let (url, _hits) = mock(|route, body, _| match route {
        "POST /graphql" if body.contains("myself") => (
            200,
            r#"{"data":{"myself":{"clientBalance":12.0,"currentSpendPerHr":0.0,"spendLimit":80}}}"#
                .into(),
        ),
        "GET /pods" => (200, "[]".into()),
        _ => (404, "{}".into()),
    });
    let client = connect(&l, &url).await;
    let b = body(&call(&client, "offrig_status", json!({})).await);
    // The folder name only (status carries no local paths); the key is lower case on Windows.
    let folder = l
        .dir
        .file_name()
        .expect("folder")
        .to_string_lossy()
        .to_lowercase();
    assert_eq!(b["project"].as_str().map(str::to_lowercase), Some(folder));
    let plan = &b["open_plans"][0];
    assert_eq!(plan["plan_id"], job);
    assert_eq!(plan["profile"], "job");
    assert_eq!(plan["lane"], l.tag.as_str());
    assert_eq!(plan["pod_name"], format!("offrig-{}-job", l.tag));
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&l.dir);
}
