# Changelog

## Unreleased

- Hardware limits on a plan (issues #9 and #10). A plan could land on a fallback card the
  work could not use: a `job` plan rented an A100 host on a CUDA 12.8 driver, and the job's
  CUDA 13 PyTorch failed after setup. Worst-case pricing also always used the profile's top
  price ($3.49/hr), so a 3.5 h pod expecting a $2.09 card committed $12.22 instead of $7.32.
  - Profiles gain `min_cuda` (oldest host CUDA; the pod create sends every version at or
    above it as `allowedCudaVersions`, and a job profile uses the newer of this and its
    image's floor) and `min_vram_gb` (least total VRAM; smaller offers are never chosen).
    Both default to unset, so existing `config.toml` files load unchanged. The `job` profile
    now sets `min_cuda = "13.0"`.
  - `offrig_plan` takes `max_price_hr` (total $/hr; dearer offers are left out and the worst
    case is `max_hours x min(max_price_hr, dearest listed price left)`) and `no_fallback`
    (only the profile's first GPU family; the two RTX PRO 6000 Blackwell editions count as
    one). A plan with nothing left is refused with the reason for each dropped GPU. The reply
    lists the GPUs that were left out.
  - The plan stores its GPU list, price and CUDA floor, and `offrig_launch` now rents only
    from them. Before this the launch built the pod from the profile's full list and ignored
    what the plan priced.
  - `offrig_job` (and the launch result) report the GPU type and, when the pod API reports
    it, the host CUDA version rented. A host older than the plan's floor, a GPU outside the
    plan's list or a price above the plan's produces a `warnings` entry and a `WARNING`
    `next_action`; nothing is terminated automatically. `offrig_status` shows each open
    plan's GPU list, price and CUDA floor, and each pod's GPU.
  - Unverified: whether the pod API reports the host's CUDA version, and under what field
    name. offrig reads `machine.cudaVersion` if present and says so in `rented.notes` when it
    is not, rather than treating a missing report as a pass.

- One lane, one live plan (issue #7). A lane has one SSH alias, so a second pod in it
  re-pointed the alias and sent the first plan's `offrig_put`, `offrig_exec` and
  `offrig_get` to the wrong pod. `offrig_launch` now refuses while the lane has an open
  plan or any live pod it owns (`lane <tag> has a live pod <name> (plan <id>); shut it down
  first`), checked again under the store lock at the commit so two launches cannot both
  pass. `offrig up` and the app (plain lane) refuse a pod of another profile in the lane
  the same way; the same profile's pod is still reused.
- `offrig_put`, `offrig_exec` and `offrig_get` take an optional `plan_id` instead of
  silently using the first open job plan. With several open job plans and no `plan_id` they
  refuse and list the plans (id, profile, pod name). With a `plan_id` they act only on that
  plan's pod, after checking its name is the one the plan owns, which a lane's other pod
  fails. One open job plan and no `plan_id` works as before.
- `offrig_status` lists each open plan with its lane and pod name beside the project, and
  every job-tool reply (and `offrig_job`) states the `project` and `plan_id` it acted on.
  A model turn (`offrig_ask`) no longer picks an open job plan's pod for its tunnel.

- Per-project lanes (issue #2): concurrent side-cars from two projects no longer collide.
  Each project gets its own SSH alias (`offrig-<tag>`), tunnel port (from `11500`, never
  `11434`, `11435` or `11436`) and pod names (`offrig-<tag>-<profile>`), recorded in
  `lanes.toml` in the config directory and allocated under a lock on the project's first
  plan. A plan records its lane; the launch check, status, shutdown, runner and ssh/tunnel
  paths use that lane and match only its pods. The tunnel's orphan reclaim kills an `ssh`
  only when its forward and alias are the lane's own, and is now testable without touching
  real processes. Shutdown refuses a pod whose name is not the plan's lane's.
- The plain lane is unchanged: the CLI, the app and Zed keep alias `offrig`, port `11435`
  and pods `offrig-<profile>`. Plans made before lanes keep running on it.
- `OFFRIG_CONFIG_DIR` points offrig at another config directory (tests and sandboxes).

## 0.1.0 (unreleased)

First version, built and tested live on 2026-10-02.

- A `jam` job profile for ai-jam-sessions' singing renders (SoulX-Singer): one cheap
  24-48 GB card, A40 first, on the same pinned PyTorch image as `job`.
- Job profiles name the oldest host CUDA version their image runs on (`min_cuda`), and the
  pod is created with RunPod's `allowedCudaVersions` from it. The PyTorch image is a CUDA
  12.8 build, so `job` and `jam` land only on 12.8 hosts or newer: on an older driver the
  pod starts and torch finds no GPU, after the rent has begun.

- Job pods: a profile with a `job` rents a GPU for work that runs on it (a training run)
  instead of a model server. The pod runs a pinned PyTorch image with sshd only, no
  forwarding and no tunnel. New side-car tools `offrig_put`, `offrig_exec` (start, status,
  stop; detached on the pod) and `offrig_get`; a default `job` profile on the medium
  tier's GPUs. `offrig up` and the app refuse a job profile before renting.

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
- README: status summary, the side-car and runner in the overview, both engines in the guarantee, install steps for the side-car, updated standards and compensators.
- Staging: `offrig stage <profile> --dc <id> [--yes]` puts a recipe profile's weights on a RunPod network volume with a short-lived download pod, pins the profile (pods, offers, plan prices) to the volume's data center, and makes launches run Hugging Face offline once the stage is marked complete. `--remove --yes` deletes the volume. Human-only: the volume bills monthly. Built and tested; no volume created.
- First frontier run (2026-10-03): 4x RTX PRO 6000 serving Qwen3-Coder-480B AWQ on SGLang, ready in 22 minutes, 31 handoffs drained in 20 seconds, $3.59. A concurrency sweep set `parallel` to 64 on the frontier and 32 on the one-card SGLang tiers. Fixes from the SGLang rehearsals: the `no_repeats` check, the handoff's format over the role's, Markdown-named heading feedback, and stopping a stalled revision.
- Side-car phase 3b, recipe engines: a profile's `recipe` serves with SGLang (pinned `lmsysorg/sglang:v0.5.20-cu130`, a Hugging Face model, extra arguments, optional RunPod-secret token). The frontier tier now runs Qwen3-Coder-480B AWQ on SGLang; `frontier-mini` and `frontier-mini-awq` rehearse that path on one RTX PRO 6000. The launch waits on `/health` plus the model list with download progress and fails fast with the engine log. The guard and `offrig models` read the OpenAI model list, which every engine serves.
- Side-car phase 3a, the handoff runner: `offrig_run` starts a detached process (`offrig-mcp --runner <plan>`) that works the queue on the pod model with the profile's slots plus one in flight, critical path first. Each handoff gets a draft and at most two revisions, driven only by deterministic checks that failed (heading, items, contains, absent, words). Dependency results feed dependent handoffs. Work that code cannot accept goes to a new `review` state, with `offrig_handoffs action=output` and send-back feedback. The runner shuts the pod down when the queue is dry. Store schema v3 (checks, outputs) with migration. Grounded in research recorded in docs/sidecar-design.md.
- Live side-car rehearsal on the small tier (2026-10-03, $0.08): launch, ask and shutdown against the real RunPod. Fixes from it: profiles gain `parallel` (default 4, `OLLAMA_NUM_PARALLEL`; 40 to 102 tok/s measured), `complete` defaults its reason, status points at a live session, leaked thinking is stripped from replies, `offrig_ask` reports the records it always injects.
- Side-car phase 2: `offrig_launch` (plan id only, idempotent, waits for GPUs renting nothing), `offrig_job`, `offrig_ask` (role-headed turn on the pod model, untrusted reply) and `offrig_shutdown`; handoff outcomes; a detached watchdog process that terminates the pod at the plan's deadline; store schema v2 with migration.
- Renamed from podbay before release: the name collides with Podbay Cloud and podbay.fm.
