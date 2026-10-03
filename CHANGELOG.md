# Changelog

## 0.1.0 (unreleased)

First version, built and tested live on 2026-10-02.

- Core library: RunPod REST and GraphQL client, pod spec with a pinned Ollama image and
  an sshd bootstrap, SSH tunnel with orphan reclaim, on-pod model pulls that survive the
  client exiting, Zed settings edits through a JSONC syntax tree, guard checks, cost and
  idle tracking.
- `offrig` CLI: status, gpus, profiles, up, tunnel, pull, models, zed, zed-remove,
  guard, check, connect, down.
- `offrig-app` desktop app (egui): profiles and live prices, launch with runway check,
  pod status and cost, tunnel control, model pulls and tests, Zed wiring, guard checks,
  shutdown and close-with-pod confirmations, idle auto-stop.
- Three default tiers: small, medium, frontier.
- Refuse to launch a profile whose models already exist in the local Ollama, before any spend.
- Small profile uses qwen3:4b (qwen3:8b is in this rig's local Ollama).
- Frontier tier: 4x RTX PRO 6000 (384 GB, $8.36/hr live); medium leads with 1x RTX PRO 6000. 2x B200 was not rentable.
- Wait for GPUs: a profile can wait (frontier: 120 min) for its GPUs, checking every minute and renting nothing until they are free; `up --wait`, Ctrl+C and the app's Cancel launch.
- Side-car phase 1: `offrig-mcp` MCP server with status, offers, plan, memory search/record and handoff queue tools over a per-project SQLite store (typed records with supersession, FTS5, worst-case budget ledger, handoff state machine). Roles render from Role OS plus four game roles. `offrig budget` sets the cap (human only).
- First frontier run (2026-10-03): 4x RTX PRO 6000 serving Qwen3-Coder-480B AWQ on SGLang, ready in 22 minutes, 31 handoffs drained in 20 seconds, $3.59. A concurrency sweep set `parallel` to 64 on the frontier and 32 on the one-card SGLang tiers. Fixes from the SGLang rehearsals: the `no_repeats` check, the handoff's format over the role's, Markdown-named heading feedback, and stopping a stalled revision.
- Side-car phase 3b, recipe engines: a profile's `recipe` serves with SGLang (pinned `lmsysorg/sglang:v0.5.20-cu130`, a Hugging Face model, extra arguments, optional RunPod-secret token). The frontier tier now runs Qwen3-Coder-480B AWQ on SGLang; `frontier-mini` and `frontier-mini-awq` rehearse that path on one RTX PRO 6000. The launch waits on `/health` plus the model list with download progress and fails fast with the engine log. The guard and `offrig models` read the OpenAI model list, which every engine serves.
- Side-car phase 3a, the handoff runner: `offrig_run` starts a detached process (`offrig-mcp --runner <plan>`) that works the queue on the pod model with the profile's slots plus one in flight, critical path first. Each handoff gets a draft and at most two revisions, driven only by deterministic checks that failed (heading, items, contains, absent, words). Dependency results feed dependent handoffs. Work that code cannot accept goes to a new `review` state, with `offrig_handoffs action=output` and send-back feedback. The runner shuts the pod down when the queue is dry. Store schema v3 (checks, outputs) with migration. Grounded in research recorded in docs/sidecar-design.md.
- Live side-car rehearsal on the small tier (2026-10-03, $0.08): launch, ask and shutdown against the real RunPod. Fixes from it: profiles gain `parallel` (default 4, `OLLAMA_NUM_PARALLEL`; 40 to 102 tok/s measured), `complete` defaults its reason, status points at a live session, leaked thinking is stripped from replies, `offrig_ask` reports the records it always injects.
- Side-car phase 2: `offrig_launch` (plan id only, idempotent, waits for GPUs renting nothing), `offrig_job`, `offrig_ask` (role-headed turn on the pod model, untrusted reply) and `offrig_shutdown`; handoff outcomes; a detached watchdog process that terminates the pod at the plan's deadline; store schema v2 with migration.
- Renamed from podbay before release: the name collides with Podbay Cloud and podbay.fm.
