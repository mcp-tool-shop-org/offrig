//! MCP Tasks (`io.modelcontextprotocol/tasks`, SEP-2663) adapter over `offrig_launch`.
//!
//! A task handle is a read-only view over the launch job row and plan state:
//! - The detached watchdog, `plan.deadline()`, and `offrig_shutdown` remain authoritative.
//! - `ttlMs` governs only metadata retention of the task mapping, never the rented pod.
//! - `tasks/cancel` sets the launch cancel flag and routes through `offrig_shutdown` with
//!   `ops::TASK_CANCEL_REASON` without bypassing in-flight handoffs.

use offrig_core::cost;
use offrig_core::store::{self, Store};
use rmcp::ErrorData as McpError;
use rmcp::model::{
    CallToolResult, DetailedTask, ErrorCode, GetTaskResult, JsonObject, MetaObject, Task,
    TaskPayload, TaskStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Suggested polling interval for `offrig_launch` MCP tasks, in milliseconds.
pub const LAUNCH_TASK_POLL_INTERVAL_MS: u64 = 5_000;

/// Retention buffer beyond `plan.deadline()` for display-only `ttlMs` metadata.
pub const LAUNCH_TASK_TTL_MARGIN_MS: u64 = 600_000;

/// Fallback metadata TTL when a plan has no `committed_at` / deadline timestamp yet.
pub const LAUNCH_TASK_DEFAULT_TTL_MS: u64 = 3_600_000;

/// Store settings prefix holding persisted `LaunchTaskRecord` entries.
pub const LAUNCH_TASK_KEY_PREFIX: &str = "launch_task:";

/// Test-only override for `ttlMs`, compiled out of release builds.
#[cfg(debug_assertions)]
pub fn test_ttl_override_ms() -> Option<u64> {
    std::env::var("OFFRIG_TEST_TASK_TTL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchTaskRecord {
    pub task_id: String,
    pub job_id: i64,
    pub plan_id: i64,
    pub created_at: String,
    pub last_updated_at: String,
    pub created_at_ms: u64,
    pub ttl_ms: Option<u64>,
    #[serde(default)]
    pub cancel_requested: bool,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub cancel_refusal: Option<Value>,
}

impl LaunchTaskRecord {
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.ttl_ms
            .is_some_and(|ttl| now_ms >= self.created_at_ms.saturating_add(ttl))
    }
}

pub fn task_setting_key(task_id: &str) -> String {
    format!("{LAUNCH_TASK_KEY_PREFIX}{task_id}")
}

/// Compute display-only `ttlMs` from `plan.deadline()` plus margin. Never governs pod life.
pub fn display_ttl_ms(deadline_secs: Option<i64>, now_ms: u64) -> u64 {
    #[cfg(debug_assertions)]
    if let Some(ms) = test_ttl_override_ms() {
        return ms;
    }
    match deadline_secs {
        Some(d) => {
            let deadline_ms = u64::try_from(d.max(0)).unwrap_or(0).saturating_mul(1000);
            deadline_ms
                .saturating_sub(now_ms)
                .saturating_add(LAUNCH_TASK_TTL_MARGIN_MS)
        }
        None => LAUNCH_TASK_DEFAULT_TTL_MS,
    }
}

fn save_record(store: &Store, rec: &LaunchTaskRecord) -> offrig_core::Result<()> {
    let serialized = serde_json::to_string(rec)
        .map_err(|e| offrig_core::Error::Refused(format!("serializing task record: {e}")))?;
    store.set_setting(&task_setting_key(&rec.task_id), &serialized)
}

/// Evict expired task metadata entries without touching plans, jobs, pods, or watchdogs.
pub fn sweep_expired_tasks(store: &Store, now_ms: u64) {
    let Ok(entries) = store.settings_with_prefix(LAUNCH_TASK_KEY_PREFIX) else {
        return;
    };
    for (key, val) in entries {
        if let Ok(rec) = serde_json::from_str::<LaunchTaskRecord>(&val)
            && rec.is_expired(now_ms)
        {
            let _ = store.clear_setting(&key);
        }
    }
}

/// Create (or return an existing active) non-guessable bearer `task_id` mapped to `(job_id, plan_id)`.
pub fn create_or_load_launch_task(
    store: &Store,
    job_id: i64,
    plan_id: i64,
    deadline_secs: Option<i64>,
    now_ms: u64,
) -> offrig_core::Result<LaunchTaskRecord> {
    sweep_expired_tasks(store, now_ms);
    if let Ok(existing) = store.settings_with_prefix(LAUNCH_TASK_KEY_PREFIX) {
        for (_, val) in existing {
            if let Ok(rec) = serde_json::from_str::<LaunchTaskRecord>(&val)
                && rec.job_id == job_id
                && rec.plan_id == plan_id
                && !rec.is_expired(now_ms)
            {
                return Ok(rec);
            }
        }
    }
    let task_id = loop {
        let candidate = format!("task-{}", store.random_hex_128()?);
        if store.setting(&task_setting_key(&candidate))?.is_none() {
            break candidate;
        }
    };
    let now_iso = cost::iso_utc(i64::try_from(now_ms / 1000).unwrap_or(i64::MAX));
    let ttl_ms = Some(display_ttl_ms(deadline_secs, now_ms));
    let rec = LaunchTaskRecord {
        task_id,
        job_id,
        plan_id,
        created_at: now_iso.clone(),
        last_updated_at: now_iso,
        created_at_ms: now_ms,
        ttl_ms,
        cancel_requested: false,
        cancelled: false,
        cancel_refusal: None,
    };
    save_record(store, &rec)?;
    Ok(rec)
}

/// Look up an active, non-expired launch task by exact bearer ID. Unknown or expired IDs
/// fail with `-32602` (`INVALID_PARAMS`), matching `rmcp::TaskManager`'s unknown-task error.
pub fn load_active_task(
    store: &Store,
    task_id: &str,
    now_ms: u64,
) -> Result<LaunchTaskRecord, McpError> {
    sweep_expired_tasks(store, now_ms);
    let raw = store
        .setting(&task_setting_key(task_id))
        .map_err(|e| McpError::internal_error(format!("reading task store: {e}"), None))?
        .ok_or_else(|| McpError::invalid_params(format!("unknown task: {task_id}"), None))?;
    let rec: LaunchTaskRecord = serde_json::from_str(&raw)
        .map_err(|e| McpError::internal_error(format!("corrupt task record: {e}"), None))?;
    if rec.is_expired(now_ms) {
        let _ = store.clear_setting(&task_setting_key(task_id));
        return Err(McpError::invalid_params(
            format!("unknown task: {task_id}"),
            None,
        ));
    }
    Ok(rec)
}

pub fn update_record(store: &Store, rec: &LaunchTaskRecord) -> Result<(), McpError> {
    save_record(store, rec)
        .map_err(|e| McpError::internal_error(format!("saving task record: {e}"), None))
}

/// Initial seed `Task` returned immediately on `CreateTaskResult`.
pub fn seed_task(rec: &LaunchTaskRecord) -> Task {
    let mut task = Task::new(
        &rec.task_id,
        TaskStatus::Working,
        &rec.created_at,
        &rec.last_updated_at,
    )
    .with_status_message("launching rented pod")
    .with_poll_interval_ms(LAUNCH_TASK_POLL_INTERVAL_MS);
    task.ttl_ms = rec.ttl_ms;
    task
}

fn value_to_json_object(v: &Value) -> JsonObject {
    match v {
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => JsonObject::new(),
    }
}

fn error_to_object(error: &McpError) -> JsonObject {
    match serde_json::to_value(error) {
        Ok(Value::Object(map)) => map,
        _ => JsonObject::new(),
    }
}

fn snapshot_to_meta(
    snapshot: &Value,
    deadline: Option<&str>,
    cancel_refusal: Option<&Value>,
) -> MetaObject {
    let mut map = MetaObject::default();
    for key in [
        "job_id",
        "plan_id",
        "state",
        "plan_state",
        "progress",
        "rented",
        "warnings",
        "minutes_left",
        "spent_so_far",
        "watchdog_alive",
        "watchdog_error",
        "failure",
        "error",
        "code",
        "retryable",
        "pod_rented",
        "next_action",
    ] {
        if let Some(val) = snapshot.get(key)
            && !val.is_null()
        {
            map.insert(key.to_string(), val.clone());
        }
    }
    if let Some(d) = deadline {
        map.insert("deadline".to_string(), Value::String(d.to_string()));
    }
    if let Some(refusal) = cancel_refusal {
        map.insert("cancel_refused".to_string(), Value::Bool(true));
        map.insert("cancel_refusal".to_string(), refusal.clone());
    }
    map
}

fn enriched_snapshot(
    snapshot: Value,
    deadline: Option<&str>,
    ok_flag: bool,
    cancel_refusal: Option<&Value>,
) -> Value {
    let mut out = match snapshot {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    out.insert("ok".to_string(), Value::Bool(ok_flag));
    if let Some(d) = deadline {
        out.insert("deadline".to_string(), Value::String(d.to_string()));
    }
    if let Some(refusal) = cancel_refusal {
        out.insert("cancel_refused".to_string(), Value::Bool(true));
        out.insert("cancel_refusal".to_string(), refusal.clone());
        if let Some(msg) = refusal.get("error").and_then(Value::as_str) {
            out.insert("error".to_string(), Value::String(msg.to_string()));
        }
        if let Some(code) = refusal.get("code").and_then(Value::as_str) {
            out.insert("code".to_string(), Value::String(code.to_string()));
        }
        if let Some(next) = refusal.get("next_action").and_then(Value::as_str) {
            out.insert("next_action".to_string(), Value::String(next.to_string()));
        }
    }
    Value::Object(out)
}

/// Project `LaunchTaskRecord` + live DB `Job` + `offrig_job` snapshot into a `GetTaskResult`.
pub fn build_get_task_result(
    rec: &LaunchTaskRecord,
    job: Option<&store::Job>,
    snapshot: Option<Value>,
    deadline: Option<&str>,
    now_iso: &str,
) -> GetTaskResult {
    let Some(job) = job else {
        let err = McpError::new(
            ErrorCode::INTERNAL_ERROR,
            format!(
                "launch job {} for plan {} no longer exists",
                rec.job_id, rec.plan_id
            ),
            None,
        );
        let mut task = Task::new(&rec.task_id, TaskStatus::Failed, &rec.created_at, now_iso)
            .with_status_message("launch job record is missing")
            .with_poll_interval_ms(LAUNCH_TASK_POLL_INTERVAL_MS);
        task.ttl_ms = rec.ttl_ms;
        return GetTaskResult::new(DetailedTask::new(
            task,
            TaskPayload::Failed {
                error: error_to_object(&err),
            },
        ));
    };

    let snapshot = snapshot.unwrap_or_else(|| json!({ "job_id": job.id, "plan_id": job.plan_id, "state": job.state, "progress": job.progress }));
    let meta = snapshot_to_meta(&snapshot, deadline, rec.cancel_refusal.as_ref());
    let step = job.progress["step"].as_str().unwrap_or("");
    let report_only = step == "engine_hung" || step == "engine_unresponsive";
    let last_updated = job.progress["at"]
        .as_i64()
        .map(cost::iso_utc)
        .unwrap_or_else(|| rec.last_updated_at.clone());

    let cancel_msg = rec.cancel_refusal.as_ref().and_then(|r| {
        r.get("error")
            .and_then(Value::as_str)
            .map(|e| format!("cancel refused: {e}"))
    });

    let (status, status_message, payload) = if rec.cancel_refusal.is_some() {
        if job.state == "running" && !report_only {
            (
                TaskStatus::Working,
                cancel_msg.unwrap_or_else(|| "cancel refused".to_string()),
                TaskPayload::Working,
            )
        } else {
            let body = enriched_snapshot(snapshot, deadline, false, rec.cancel_refusal.as_ref());
            let call_res = CallToolResult::structured_error(body);
            let obj = value_to_json_object(&serde_json::to_value(call_res).unwrap_or_default());
            (
                TaskStatus::Completed,
                cancel_msg.unwrap_or_else(|| "cancel refused".to_string()),
                TaskPayload::Completed { result: obj },
            )
        }
    } else if rec.cancelled || job.state == "cancelled" || rec.cancel_requested {
        if job.state == "running" && snapshot["plan_state"] == "committed" {
            (
                TaskStatus::Working,
                "cancelling launch".to_string(),
                TaskPayload::Working,
            )
        } else {
            (
                TaskStatus::Cancelled,
                "task cancelled".to_string(),
                TaskPayload::Cancelled,
            )
        }
    } else if job.state == "running" && !report_only {
        let msg = if step.is_empty() {
            "launching rented pod".to_string()
        } else {
            step.to_string()
        };
        (TaskStatus::Working, msg, TaskPayload::Working)
    } else if job.state == "failed" {
        // SEP-2663: tool-level launch errors (no_capacity, pod_not_ready, etc.) MUST report
        // TaskStatus::Completed with result.isError = true; Failed is reserved for JSON-RPC errors.
        let body = enriched_snapshot(snapshot, deadline, false, None);
        let msg = body["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| "launch failed".to_string());
        let call_res = CallToolResult::structured_error(body);
        let obj = value_to_json_object(&serde_json::to_value(call_res).unwrap_or_default());
        (
            TaskStatus::Completed,
            msg,
            TaskPayload::Completed { result: obj },
        )
    } else {
        let body = enriched_snapshot(snapshot, deadline, true, None);
        let msg = if report_only {
            step.to_string()
        } else {
            "pod ready".to_string()
        };
        let call_res = CallToolResult::structured(body);
        let obj = value_to_json_object(&serde_json::to_value(call_res).unwrap_or_default());
        (
            TaskStatus::Completed,
            msg,
            TaskPayload::Completed { result: obj },
        )
    };

    let mut task = Task::new(&rec.task_id, status, &rec.created_at, last_updated)
        .with_status_message(status_message)
        .with_poll_interval_ms(LAUNCH_TASK_POLL_INTERVAL_MS);
    task.ttl_ms = rec.ttl_ms;

    let mut out = GetTaskResult::new(DetailedTask::new(task, payload));
    out.meta = Some(meta);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_job(state: &str, step: &str) -> store::Job {
        store::Job {
            id: 7,
            plan_id: 3,
            kind: "launch".to_string(),
            state: state.to_string(),
            progress: json!({
                "step": step,
                "at": 1_700_000_050i64,
            }),
            error: None,
            started_at: 1_700_000_000,
            updated_at: 1_700_000_050,
        }
    }

    fn sample_record() -> LaunchTaskRecord {
        LaunchTaskRecord {
            task_id: "task-0123456789abcdef0123456789abcdef".to_string(),
            job_id: 7,
            plan_id: 3,
            created_at: "2023-11-14T22:13:20Z".to_string(),
            last_updated_at: "2023-11-14T22:13:20Z".to_string(),
            created_at_ms: 1_700_000_000_000,
            ttl_ms: Some(1_200_000),
            cancel_requested: false,
            cancelled: false,
            cancel_refusal: None,
        }
    }

    #[test]
    fn display_ttl_ms_respects_deadline_and_fallback() {
        if std::env::var_os("OFFRIG_TEST_TASK_TTL_MS").is_none() {
            assert_eq!(
                display_ttl_ms(Some(1_700_000_600), 1_700_000_000_000),
                600_000 + LAUNCH_TASK_TTL_MARGIN_MS
            );
            assert_eq!(
                display_ttl_ms(None, 1_700_000_000_000),
                LAUNCH_TASK_DEFAULT_TTL_MS
            );
        }
    }

    #[test]
    fn build_get_task_result_missing_job_reports_failed() {
        let rec = sample_record();
        let res = build_get_task_result(&rec, None, None, None, "2023-11-14T22:14:00Z");
        assert_eq!(res.task.task.status, TaskStatus::Failed);
        match res.task.payload {
            TaskPayload::Failed { error } => {
                assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32603));
            }
            other => panic!("expected TaskPayload::Failed, got {other:?}"),
        }
    }

    #[test]
    fn build_get_task_result_report_only_step_completes_without_error() {
        let rec = sample_record();
        for step in ["engine_hung", "engine_unresponsive"] {
            let job = sample_job("running", step);
            let snapshot = json!({
                "job_id": 7,
                "plan_id": 3,
                "state": "running",
                "plan_state": "committed",
                "progress": job.progress,
                "error": "engine stalled",
            });
            let res = build_get_task_result(
                &rec,
                Some(&job),
                Some(snapshot),
                Some("2023-11-14T22:23:20Z"),
                "2023-11-14T22:14:10Z",
            );
            assert_eq!(res.task.task.status, TaskStatus::Completed);
            assert_eq!(res.task.task.status_message.as_deref(), Some(step));
            match res.task.payload {
                TaskPayload::Completed { result } => {
                    assert_eq!(result.get("isError").and_then(Value::as_bool), Some(false));
                }
                other => panic!("expected TaskPayload::Completed, got {other:?}"),
            }
        }
    }

    #[test]
    fn build_get_task_result_cancel_refusal_surfaces_while_running_and_done() {
        let mut rec = sample_record();
        rec.cancel_requested = true;
        rec.cancel_refusal = Some(json!({
            "error": "1 handoff still running",
            "code": "handoff_in_flight",
            "next_action": "wait for handoff",
        }));
        let running = sample_job("running", "waiting_engine");
        let res_running = build_get_task_result(
            &rec,
            Some(&running),
            Some(
                json!({ "job_id": 7, "plan_id": 3, "state": "running", "plan_state": "committed" }),
            ),
            None,
            "2023-11-14T22:14:10Z",
        );
        assert_eq!(res_running.task.task.status, TaskStatus::Working);
        assert!(
            res_running
                .task
                .task
                .status_message
                .as_deref()
                .is_some_and(|m| m.contains("cancel refused: 1 handoff still running"))
        );

        let done = sample_job("done", "ready");
        let res_done = build_get_task_result(
            &rec,
            Some(&done),
            Some(json!({ "job_id": 7, "plan_id": 3, "state": "done", "plan_state": "committed" })),
            None,
            "2023-11-14T22:14:20Z",
        );
        assert_eq!(res_done.task.task.status, TaskStatus::Completed);
        match res_done.task.payload {
            TaskPayload::Completed { result } => {
                assert_eq!(result.get("isError").and_then(Value::as_bool), Some(true));
            }
            other => panic!("expected TaskPayload::Completed, got {other:?}"),
        }
    }

    #[test]
    fn create_or_load_launch_task_reuses_active_and_replaces_expired() {
        let dir = std::env::temp_dir().join(format!("offrig-tasks-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create unit test dir");
        let store = Store::open(&dir.join("offrig.db")).expect("open test store");
        let now_ms = 1_700_000_000_000u64;
        let first = create_or_load_launch_task(&store, 11, 2, Some(1_700_000_120), now_ms)
            .expect("create initial launch task");
        let reused = create_or_load_launch_task(&store, 11, 2, Some(1_700_000_120), now_ms + 1_000)
            .expect("reuse active launch task");
        assert_eq!(first.task_id, reused.task_id);

        let after_ttl = now_ms + first.ttl_ms.expect("task ttl_ms") + 1;
        let replaced = create_or_load_launch_task(&store, 11, 2, Some(1_700_000_120), after_ttl)
            .expect("replace expired launch task");
        assert_ne!(first.task_id, replaced.task_id);
        assert!(load_active_task(&store, &first.task_id, after_ttl).is_err());
        assert!(load_active_task(&store, &replaced.task_id, after_ttl).is_ok());
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
