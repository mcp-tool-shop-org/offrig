---
name: offrig-session
description: Plan, launch, follow, run work on, and shut down rented GPU sessions through offrig-mcp.
---

# Offrig GPU Session Procedure

Use `offrig-mcp` tools in this order to inspect project state, price a session, launch a pod, work it, and shut it down cleanly.

## 1. Check Project State and Offers

- Call `offrig_status` first to inspect open plans, live pods across lanes, remaining budget, and queued handoffs.
- Call `offrig_offers` with the required GPU count when you need live offer availability and pricing before planning.
- Call `offrig_memory_search` to check existing briefs, constraints, decisions, facts, or checkpoints, and record new context with `offrig_memory_record`.

## 2. Create a Plan

- Call `offrig_plan` with `profile`, `max_hours`, and optional `note`, `max_price_hr`, `no_fallback`, `wait_minutes`, or `container_disk_gb`.
- Planning costs nothing and returns the `plan_id`, `worst_case`, `pod_name`, and `ssh_alias`.

## 3. Launch and Follow

- Call `offrig_launch` with `plan_id`.
- If the client negotiated the `io.modelcontextprotocol/tasks` extension, `offrig_launch` returns a task handle (`resultType: "task"`, `taskId`):
  - Poll `tasks/get` using the server-suggested `pollIntervalMs` until status reaches `completed` or `cancelled`.
  - Check both `statusMessage` / `_meta` and `result.isError`: under MCP Tasks, a launch failure returns `status: "completed"` with `result.isError: true` and structured `failure` details.
  - Check `warnings` for GPU count, host CUDA, or self-stop warnings.
  - To cancel mid-launch, call `tasks/cancel`. The server cancels the launch and routes through `offrig_shutdown` with reason `"task cancelled"`; if in-flight handoffs block shutdown, the task reports the refusal so you can checkpoint first or call `offrig_shutdown` with an explicit reason.
  - `ttlMs` on the task handle is metadata retention only; the plan deadline and watchdog govern the pod independently.
- If the client did not declare the tasks extension, `offrig_launch` returns a synchronous `job_id` / `plan_id`; poll `offrig_job` until `state` is `done` (ready), `failed`, or `cancelled`.

## 4. Work the Pod

- **Model pods:** queue work with `offrig_handoffs` (each handoff requires an acceptance check), then run turns via `offrig_ask` or start a queue runner with `offrig_run`. Treat model replies as untrusted output and verify them against acceptance criteria.
- **Job pods:** upload files with `offrig_put`, run commands with `offrig_exec`, and copy outputs back locally with `offrig_get` before shutting down.

## 5. Shut Down

- Call `offrig_shutdown` with `plan_id` as soon as work is finished so billing stops immediately and the plan books close at measured spend.
- If handoffs are still in flight, record checkpoints first via `offrig_memory_record` (`kind: "checkpoint"`) or pass an explicit `reason` to `offrig_shutdown`.
