//! `offrig verify calibrate` end to end: the real binary against a mock Ollama on
//! loopback. The model server answers every chat the same way; what is under test is
//! the command line, the files it writes and the refusals.

use std::process::Command;

use serde_json::{Value, json};

mod common;
use common::{count, err, mock, out, temp};

const GOLD: &str = include_str!("../../offrig-core/tests/fixtures/calibrate-gold.jsonl");

fn server() -> (String, common::Hits) {
    mock(|route, _| {
        match route {
        "GET /api/version" => (200, r#"{"version":"0.35.0"}"#.into()),
        "GET /api/tags" => (
            200,
            json!({"models": [{"name": "judge:latest", "digest": "sha256:feed", "size": 1}]})
                .to_string(),
        ),
        "POST /api/chat" => (
            200,
            json!({"message": {"content": json!({
                "reasoning": "r", "verdict": "unsupported", "evidence_quote": "", "evidence_source": ""
            }).to_string()}, "done_reason": "stop", "eval_count": 3, "prompt_eval_count": 30, "total_duration": 5})
            .to_string(),
        ),
        _ => (404, "{}".into()),
    }
    })
}

fn offrig(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_offrig"))
        .args(["verify", "calibrate"])
        .args(args)
        .current_dir(dir)
        .env_remove("RUST_BACKTRACE")
        .output()
        .expect("run offrig")
}

#[test]
fn calibrate_runs_resumes_reports_and_refuses() {
    let dir = temp("calibrate");
    let gold = dir.join("gold.jsonl");
    std::fs::write(&gold, GOLD).expect("gold");
    let (url, hits) = server();
    let g = gold.to_str().expect("path");
    let o = dir.join("run");
    let os = o.to_str().expect("path");
    let p = dir.to_str().expect("path");

    let r = offrig(
        &dir,
        &[
            "--model",
            "judge",
            "--url",
            &url,
            "--project",
            p,
            "--out",
            os,
            g,
            "--gpu-cost-hr",
            "3.6",
        ],
    );
    assert_eq!(r.status.code(), Some(0), "{}", err(&r));
    let text = out(&r);
    assert!(text.contains("default rule overall: FAIL"), "{text}");
    assert!(text.contains("false-accept (unsup+ct)"), "{text}");
    // Seven measurement calls (num_predict 1) and seven scored ones.
    assert_eq!(count(&hits, "POST /api/chat"), 14);
    let m: Value =
        serde_json::from_str(&std::fs::read_to_string(o.join("metrics.json")).expect("m"))
            .expect("json");
    assert_eq!(m["model_digest"], "sha256:feed");
    assert_eq!(m["claims_selected"], 7);
    assert_eq!(m["settings"]["num_ctx_mode"], "auto");
    assert_eq!(m["ctx"]["method"], "measured");
    assert_eq!(m["ctx"]["num_ctx"], m["settings"]["num_ctx"]);
    let man = std::fs::read_to_string(o.join("manifest.json")).expect("manifest");
    assert!(
        man.contains("gold.jsonl") && !man.contains(p),
        "no directories in the manifest"
    );

    // Resume with nothing left: no new chat. A changed setting is refused (exit 1).
    let r = offrig(
        &dir,
        &[
            "--model",
            "judge",
            "--url",
            &url,
            "--project",
            p,
            "--resume",
            os,
            g,
        ],
    );
    assert_eq!(r.status.code(), Some(0), "{}", err(&r));
    assert_eq!(count(&hits, "POST /api/chat"), 14);
    let r = offrig(
        &dir,
        &[
            "--model",
            "judge",
            "--url",
            &url,
            "--project",
            p,
            "--resume",
            os,
            g,
            "--seed",
            "9",
        ],
    );
    assert_eq!(r.status.code(), Some(1), "{}", err(&r));
    assert!(err(&r).contains("cannot resume"), "{}", err(&r));

    // Report only: no model needed, nothing called.
    let r = offrig(&dir, &["--report-only", os, "--gpu-cost-hr", "2"]);
    assert_eq!(r.status.code(), Some(0), "{}", err(&r));
    assert!(out(&r).contains("default rule"), "{}", out(&r));
    assert_eq!(count(&hits, "POST /api/chat"), 14);

    // The default output directory is under the project's .offrig/out.
    let r = offrig(
        &dir,
        &[
            "--model",
            "judge",
            "--url",
            &url,
            "--project",
            p,
            g,
            "--limit",
            "1",
        ],
    );
    assert_eq!(r.status.code(), Some(0), "{}", err(&r));
    let outs: Vec<_> = std::fs::read_dir(dir.join(".offrig").join("out"))
        .expect("out dir")
        .flatten()
        .collect();
    assert_eq!(outs.len(), 1);
    assert!(
        outs[0]
            .file_name()
            .to_string_lossy()
            .starts_with("calibrate-judge-")
    );

    // Refusals: cloud model, remote server, a model the server lacks, bad values.
    let chats = count(&hits, "POST /api/chat");
    for (args, want) in [
        (
            vec!["--model", "gpt-oss:120b-cloud", "--url", &url],
            "Ollama Cloud",
        ),
        (
            vec!["--model", "judge", "--url", "https://ollama.com"],
            "loopback",
        ),
        (vec!["--model", "absent", "--url", &url], "not installed"),
    ] {
        let mut all = args.clone();
        all.extend(["--project", p, g]);
        let r = offrig(&dir, &all);
        assert_eq!(r.status.code(), Some(1), "{}", err(&r));
        assert!(err(&r).contains(want), "{want}: {}", err(&r));
    }
    let r = offrig(&dir, &["--model", "judge", "--think", "max", g]);
    assert_eq!(r.status.code(), Some(1));
    let r = offrig(&dir, &[g]);
    assert_eq!(r.status.code(), Some(1), "--model is required");
    let r = offrig(
        &dir,
        &[
            "--model",
            "judge",
            "--url",
            &url,
            "--project",
            p,
            "missing.jsonl",
        ],
    );
    assert_eq!(r.status.code(), Some(1), "{}", err(&r));
    assert!(err(&r).contains("missing.jsonl"));
    assert_eq!(count(&hits, "POST /api/chat"), chats);
    let _ = std::fs::remove_dir_all(&dir);
}
