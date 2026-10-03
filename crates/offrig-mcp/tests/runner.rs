//! Phase 3a end to end: the real runner process works a queue against a mock pod
//! model and a mock RunPod. Debug builds honour OFFRIG_TEST_OLLAMA_BASE and
//! OFFRIG_TEST_RUNPOD_BASE; release builds ignore them.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

use common::{count, mock, temp};
use offrig_core::checks::Check;
use offrig_core::store::{Kind, NewHandoff, NewPlan, NewRecord, State, Store};
use serde_json::{Value, json};

fn reply(text: &str) -> (u16, String) {
    (
        200,
        json!({
            "choices": [{"message": {"content": text}, "finish_reason": "stop"}],
            "usage": {"completion_tokens": 40}
        })
        .to_string(),
    )
}

/// The pod model: answers by handoff, and remembers every prompt it was sent.
fn model(prompts: Arc<Mutex<Vec<String>>>) -> (String, common::Hits) {
    mock(move |route, body, _| {
        if route != "POST /v1/chat/completions" {
            return (404, "{}".into());
        }
        let v: Value = serde_json::from_str(body).unwrap_or_default();
        let prompt = v["messages"][0]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        prompts.lock().expect("prompts").push(prompt.clone());
        if prompt.contains("## Handoff #1") {
            if prompt.contains("failed these checks") {
                reply("## Verbs\n- Draw\n- Feint\n- Hold\n\n## Failure states\n- Flinch")
            } else {
                reply("## Verbs\n- Draw\n- Feint\n- Hold") // no failure states yet
            }
        } else if prompt.contains("## Handoff #2") {
            reply("## Duel flow\n1. Stare\n2. Draw")
        } else if prompt.contains("A reviewer sent your previous output back") {
            reply("The river Vel runs under the mill.")
        } else {
            reply("<think>lore...</think>The mill stands by a river.")
        }
    })
}

fn runpod() -> (String, common::Hits) {
    mock(|route, _, _| match route {
        "DELETE /pods/p1" => (204, String::new()),
        _ => (404, "{}".into()),
    })
}

fn run(dir: &std::path::Path, plan: i64, model_url: &str, runpod_url: &str, keep_pod: bool) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp"));
    cmd.args(["--runner", &plan.to_string(), "--project"])
        .arg(dir)
        .env("RUNPOD_API_KEY", "test-key")
        .env("OFFRIG_TEST_RUNPOD_BASE", runpod_url)
        .env("OFFRIG_TEST_OLLAMA_BASE", model_url)
        .env("OFFRIG_TEST_RUNNER_POLL_MS", "100");
    if keep_pod {
        cmd.arg("--keep-pod");
    }
    let mut child = cmd.spawn().expect("spawn runner");
    let started = Instant::now();
    loop {
        if let Some(st) = child.try_wait().expect("wait") {
            assert!(st.success(), "runner exited with {st}");
            return;
        }
        if started.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("the runner did not finish");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_runner_works_the_queue_revises_against_checks_and_shuts_down() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let (model_url, model_hits) = model(Arc::clone(&prompts));
    let (runpod_url, runpod_hits) = runpod();
    let dir = temp("runner");
    let db = dir.join(".offrig").join("offrig.db");
    let plan = {
        let s = Store::open(&db).expect("store");
        s.set_budget_cap(5.0).expect("cap");
        let p = s
            .create_plan(NewPlan {
                profile: "small".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 1.0,
                max_price_hr: 0.25,
                note: None,
            })
            .expect("plan");
        s.commit_plan(p.id).expect("commit");
        s.attach_pod(p.id, "p1").expect("attach");
        s.record(NewRecord {
            kind: Some(Kind::Constraint),
            body: "Keep every list to five items or fewer.".into(),
            author: "mike".into(),
            ..Default::default()
        })
        .expect("constraint");
        let verbs = s
            .add_handoff(NewHandoff {
                role_id: "game-designer".into(),
                mission: "Core verbs of a standoff duel".into(),
                acceptance: "At least three verbs and a failure state".into(),
                checks: vec![
                    Check::Items {
                        heading: "Verbs".into(),
                        min: 3,
                    },
                    Check::Heading {
                        text: "Failure states".into(),
                    },
                ],
                accept_on_checks: true,
                ..Default::default()
            })
            .expect("verbs");
        s.add_handoff(NewHandoff {
            role_id: "systems-designer".into(),
            mission: "The duel's turn flow, built on the verbs".into(),
            acceptance: "A numbered flow".into(),
            depends_on: vec![verbs],
            checks: vec![Check::Heading {
                text: "Duel flow".into(),
            }],
            accept_on_checks: true,
            ..Default::default()
        })
        .expect("flow");
        s.add_handoff(NewHandoff {
            role_id: "lore-keeper".into(),
            mission: "A line of lore about the mill".into(),
            acceptance: "Reads as in-world and names the river".into(),
            ..Default::default()
        })
        .expect("lore");
        p.id
    };

    // First run keeps the pod: the lore handoff has no checks, so it waits in review.
    run(&dir, plan, &model_url, &runpod_url, true);
    {
        let s = Store::open(&db).expect("reopen");
        let state = |id| s.handoff(id).expect("read").expect("exists").state;
        assert_eq!(
            state(1),
            State::Complete,
            "revised once, then passed its checks"
        );
        assert_eq!(state(2), State::Complete);
        assert_eq!(state(3), State::Review, "nothing completes without checks");
        let turns = s.outputs(1).expect("outputs");
        assert_eq!(turns.len(), 2, "a draft and one revision");
        assert!(
            !turns[0].outcomes.iter().all(|o| o.pass) && turns[1].outcomes.iter().all(|o| o.pass)
        );
        assert_eq!(
            s.outputs(3).expect("lore")[0].body,
            "The mill stands by a river.",
            "thinking stripped"
        );
        assert_eq!(
            s.plan(plan).expect("plan").expect("exists").state,
            "committed",
            "--keep-pod kept it"
        );
        let job = s.job_for_plan(plan, "run").expect("job").expect("exists");
        assert_eq!(job.state, "done");
        assert_eq!(job.progress["turns"], 4);
    }
    assert_eq!(count(&runpod_hits, "DELETE /pods/p1"), 0);
    {
        let p = prompts.lock().expect("prompts");
        let flow = p
            .iter()
            .find(|x| x.contains("## Handoff #2"))
            .expect("flow prompt");
        assert!(
            flow.contains("## Input from handoff #1"),
            "the dependent saw the result"
        );
        assert!(
            flow.contains("## Failure states"),
            "it got the passing revision, not the draft"
        );
        let rev = p
            .iter()
            .find(|x| x.contains("failed these checks"))
            .expect("revision prompt");
        assert!(rev.contains("needs a Markdown heading line containing \"Failure states\""));
        assert!(
            p.iter()
                .all(|x| x.contains("Keep every list to five items")),
            "constraints in every turn"
        );
        assert!(
            p.iter().all(|x| x.starts_with("## Role")),
            "every prompt opens with the role block, so the server can reuse the prefix"
        );
    }

    // The reviewer sends the lore back; the next run revises against the feedback and,
    // with the queue drained, shuts the pod down.
    {
        let s = Store::open(&db).expect("reopen");
        s.transition(3, State::Dispatched, "name the river", None)
            .expect("send back");
    }
    run(&dir, plan, &model_url, &runpod_url, false);
    let s = Store::open(&db).expect("reopen");
    assert_eq!(
        s.handoff(3).expect("read").expect("exists").state,
        State::Review
    );
    let lore = s.outputs(3).expect("lore");
    assert_eq!(lore.len(), 2);
    assert_eq!(lore[1].body, "The river Vel runs under the mill.");
    {
        let p = prompts.lock().expect("prompts");
        let back = p
            .iter()
            .find(|x| x.contains("A reviewer sent"))
            .expect("send-back prompt");
        assert!(back.contains("name the river") && back.contains("The mill stands by a river."));
    }
    assert_eq!(
        count(&runpod_hits, "DELETE /pods/p1"),
        1,
        "shut down once the queue drained"
    );
    assert_eq!(s.plan(plan).expect("plan").expect("exists").state, "closed");
    assert!(count(&model_hits, "POST /v1/chat/completions") >= 5);
    assert!(
        dir.join(".offrig")
            .join(format!("runner-{plan}.log"))
            .exists()
    );
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
