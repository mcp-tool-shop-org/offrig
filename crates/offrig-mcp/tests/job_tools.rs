//! Issue #12: the new `offrig_exec` shapes (`run`, `save_log`, a name only where one is
//! needed) are accepted by the tool and reach the job-pod resolution before anything
//! remote happens. With no launched job pod they are refused there, so no ssh is tried.

mod common;

use common::{mock, temp};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

fn body(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

#[tokio::test]
async fn exec_run_and_save_log_are_accepted_and_refused_without_a_job_pod() {
    let (url, _hits) = mock(|_, _, _| (404, "{}".into()));
    let dir = temp("job-tools");
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_offrig-mcp")).configure(|c| {
        c.arg("--project").arg(&dir);
        c.env("RUNPOD_API_KEY", "test-key");
        c.env("OFFRIG_CONFIG_DIR", dir.join("cfg"));
        c.env("OFFRIG_TEST_RUNPOD_BASE", &url);
        c.env("OFFRIG_TEST_NO_WATCHDOG", "1");
    });
    let client = ().serve(TokioChildProcess::new(cmd).expect("spawn")).await.expect("connect");
    let call = |a: Value| {
        let client = &client;
        async move {
            client
                .call_tool(
                    CallToolRequestParams::new("offrig_exec")
                        .with_arguments(a.as_object().cloned().expect("obj")),
                )
                .await
                .expect("call")
        }
    };
    for args in [
        // A quick synchronous check: no name, a timeout.
        json!({"action": "run", "command": "nvidia-smi", "timeout_secs": 20}),
        // Status that also saves the whole log, to a folder that does not exist yet.
        json!({"action": "status", "name": "train", "save_log": "out/logs/train.log"}),
        json!({"action": "status", "name": "train", "tail": 10}),
    ] {
        let r = call(args.clone()).await;
        assert_eq!(r.is_error, Some(true), "{args}");
        let e = body(&r)["error"].as_str().expect("error").to_string();
        assert!(e.contains("no launched job pod"), "{args}: {e}");
    }
    // No folder was made for a log that was never fetched.
    assert!(!dir.join("out").exists());
    client.cancel().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}
