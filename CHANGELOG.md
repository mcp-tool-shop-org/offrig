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
- Side-car phase 2: `offrig_launch` (plan id only, idempotent, waits for GPUs renting nothing), `offrig_job`, `offrig_ask` (role-headed turn on the pod model, untrusted reply) and `offrig_shutdown`; handoff outcomes; a detached watchdog process that terminates the pod at the plan's deadline; store schema v2 with migration.
- Renamed from podbay before release: the name collides with Podbay Cloud and podbay.fm.
