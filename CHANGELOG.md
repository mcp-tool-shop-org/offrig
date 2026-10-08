# Changelog

All notable changes to offrig are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and offrig follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

- **`offrig budget` opens a menu in a terminal.** With no amount, it shows the budget and
  any running plans, then offers to set a new cap (confirmed first) or stop new spending.
  `--show` prints the one-line budget, and so does any run where stdout isn't a terminal.
- **The cap can't take back money given to a run.** It can never be set below spent plus
  committed, from the menu or from `offrig budget <usd>`. Stopping new spending sets it to
  exactly that, so a running training job keeps its allocation and runs to its deadline.

- **A Hugging Face token on job pods.** A job profile can name a RunPod secret with
  `hf_token_secret`, as a recipe already could. RunPod substitutes it at start, and the
  bootstrap writes it to a root-only file. Job commands get `HF_TOKEN_PATH`, because an ssh
  session doesn't inherit the pod's environment. The token never enters the spec, a command
  line or a log. (2026-10-08: an anonymous download at about 11 MB/s stopped a training run
  before it could finish inside its cap.)
- **`offrig_complete`, an OpenRouter lane under the budget.** One chat completion against
  OpenRouter, approved for one job: Kimi-K3 (`moonshotai/kimi-k3`) piano arrangements for
  the ai-jam-sessions project. Both the model and the project are allow-listed in code;
  any other model or project is refused, and widening either takes a pull request.
  - Before the call, the worst case is priced from the dearest provider, and the request
    caps the provider price at that rate. The worst case is input bound x input price plus
    max_tokens x output price, and max_tokens covers reasoning and answer together. It is
    committed against the project's budget in the same ledger as pods, and refused
    (`budget_exceeded`) when it exceeds what is left.
  - After the call, OpenRouter's real charge (`usage.cost`) is recorded and the
    commitment released. A charge above the worst case is recorded as charged, with a
    WARNING.
  - If the stream fails, the charge is looked up by generation id and recorded. A
    charge that cannot be read yet stays committed until a later call settles it.
  - The answer goes to a project file, and its inputs must be project files. The key
    (`OPENROUTER_API_KEY`) is never written anywhere. There is no fallback to any other
    service.
- **Schema v4.** The ledger holds completions as well as plans, and is rebuilt in a
  transaction with every row kept. An older side-car refuses a v4 store ("Update
  offrig") and reads a v4 sibling as unreadable, so every side-car is updated together.
  `offrig_status` lists held and recent completions.

## [1.0.0] - 2026-10-07

The first release. offrig has run real work since 2026-10-02, and since 2026-10-07 two
projects have used it at once, each in its own lane: training runs on `job` pods and
singing renders on `jam` pods. This release adds downloadable Windows binaries, structured
errors and exit codes, and a handbook. Everything below the next list was built before the
first release and ships in it.

- **Windows release binaries.** Each version tag builds `offrig.exe`, `offrig-mcp.exe` and
  `offrig-app.exe` in CI into `offrig-<version>-windows-x64.zip`, with `SHA256SUMS`. The
  workflow stages a draft release and never publishes it; a person checks the draft first.
  Windows only, because offrig relies on Windows' OpenSSH, `%APPDATA%` and Zed's paths.
- **Side-car errors carry a code.** Every tool failure, including bad arguments and an
  unknown tool, returns `ok:false`, a stable `code`, the `error` text, a `next_action` and
  `retryable`. `error` and `next_action` are unchanged, so existing callers keep working.
  Budget refusals have their own code, `budget_exceeded`.
- **CLI exit codes:** `0` success, `1` something to fix on your side, `2` a runtime
  failure. Bad arguments now exit `1` instead of clap's `2`.
- **CLI log levels:** `-q` (errors and a command's own results only), `-v` (each RunPod
  call and its timing, on stderr) and `--debug` (failed response bodies and full error
  chains). The API key and any `Bearer` token are redacted at every level, and a panic
  prints one line unless `--debug` is set.
- **`offrig-mcp --help` and `--version`** answer instead of starting to serve stdio.
- **The app has its icon,** in the window and on the `.exe`.
- **`scripts/verify.sh` and `scripts/verify.ps1`** run the format check, clippy, the tests
  and a smoke run of each binary in one command.
- **Every project can see the others' pods.** `offrig_status` lists each other offrig
  pod with its lane, project, GPU, price and status, and the open plan behind it: plan id,
  note, deadline and committed worst case. The other project's store is read strictly
  read-only, leaving no files behind. Pods offrig did not create are only counted and
  named. Pods carry `OFFRIG_LANE`, and on side-car launches `OFFRIG_PLAN` and
  `OFFRIG_DEADLINE`, as environment variables visible in RunPod's console.
- **A failed launch says what it cost.** `offrig_job` gives the failure's `code`,
  `retryable`, and whether a pod was rented and billed. The new code `pod_not_ready` means a
  pod was rented but never became ready, and was terminated; `no_capacity` means nothing was
  rented. `rented` is filled as soon as the pod exists. A `job_id` that is really a plan id
  gets a message naming the right call.
- **Turning auto-stop off now survives a restart.** It used to be saved as a missing key,
  which reloads as the 30-minute default. It is now saved as `auto_stop_idle_minutes =
  "off"`; numbers, including `0`, are still minutes.
- **Coverage of at least 90% of lines, enforced.** The tests grew from about 250 to more
  than 450 and cover 94% of lines. CI fails below 90% (`cargo llvm-cov --fail-under-lines
  90`), and `codecov.yml` sets 90% targets for the project and for each change.
- **CI** adds an OSV scan of `Cargo.lock`, and uploads coverage to Codecov over OIDC.
- **A handbook and landing page** at https://mcp-tool-shop-org.github.io/offrig/.

- Launch follow-ups from the first real runs of the hardware-limits build (issue #15).
  - A narrowed plan waits for capacity instead of failing. `offrig_plan` takes
    `wait_minutes` (stored with the plan, shown in the reply; `0` fails at once), else the
    profile's `wait_for_gpu_minutes`, and the `job` profile now defaults to 20 minutes.
    Nothing is rented while waiting; the wait is cut to the plan's remaining time less a
    five-minute reserve, so it stays inside the plan's deadline and adds nothing to its
    worst case. Each retry is reported: `offrig_job` shows `progress.capacity_wait` and a
    step naming the check. The `no capacity` error now says how to set a wait.
  - `offrig_job` derives the step of a booting pod from the pod's state on each call (what
    RunPod reports, and whether sshd answers on the published port), instead of reading the
    launch thread's last event, which could be minutes stale on a slow host. The thread's
    own last step is kept beside it as `progress.launch_step`.
  - The host's CUDA version is measured: after ssh is up the launch runs `nvidia-smi` once
    through the lane, reads `CUDA Version: 12.8` or the newer `CUDA UMD Version: 13.4`, and
    fills `rented.cuda_version` (`rented.cuda_source` says `nvidia-smi`). A host below the
    plan's floor keeps the loud `warnings` entry; nothing is terminated. If `nvidia-smi`
    gives nothing, `rented.notes` says the floor is unchecked.

- Job-tool gaps from real runs (issue #12).
  - `offrig_get` creates missing local parent folders instead of refusing with "is not a
    directory", as `offrig_put` already does on the pod side. A path that exists but is a
    file is still refused.
  - `offrig_exec action=run` runs a short command synchronously with a timeout (default
    30 s, at most 120 s) and returns `stdout`, `stderr`, `exit_code` and `timed_out`. The
    command is sent as base64 and run under `timeout` with stdin closed; output is capped
    at 64 KB per stream. `name` is now optional (only start, status and stop need it).
  - `offrig_exec action=status` collapses carriage-return progress lines in its log tail to
    their last frame, and takes `save_log` to copy the job's whole log to a local file.

- Lane names in the plan, stale ssh blocks, and a container disk size (issue #6 and a new
  need from a slow-`/workspace` host).
  - `offrig_plan` shows the lane's `ssh_alias` and the `pod_name` its launch will create
    (for example `offrig-ai-jam-sessions` and `offrig-ai-jam-sessions-jam`), so a session
    can confirm its lane before launching without reading `lanes.toml`. `offrig_status`
    shows `ssh_alias` on each open plan too.
  - `offrig_shutdown` removes the lane's own `~/.ssh/config` block once the pod is gone, but
    only when the block's label names that plan's pod. Another lane's block, and a block
    already rewritten for a newer pod, are never touched. A launch that fails and terminates
    its pod does the same. The reply reports `ssh_block_removed`. A pod ended by the
    watchdog at its deadline still leaves its block, as before; the next launch rewrites it.
  - `offrig_plan` takes `container_disk_gb` (1 to 2000), overriding the profile's
    `container_disk_gb` for that plan. The plan stores it and the launch sends it as
    `containerDiskInGb`; the reply and `offrig_status` show the size in force. The disk is not
    priced into the worst case (offrig prices GPU time only).

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

- A per-project default side-car port (issue #11). The shell driver's default port, `11439`,
  was shared by every project on the machine, so another script taking it silently took a
  running side-car down. A lane now has a side-car port derived from its slot the same way
  its tunnel port is: `11700` + slot (`11700` to `11763`), a range above every lane tunnel
  and runner port (`11500` to `11627`), the plain lane and the local Ollama. Nothing is
  stored in `lanes.toml`; existing registries work unchanged.
  - `offrig-mcp --sidecar-port --project <dir>` prints the project's port (allocating its
    lane if it has none), or `OFFRIG_SIDECAR_PORT` when set. That value is refused when it is
    not a port, is below 1024, or is `11434`, `11435`, `11436` or inside the lane tunnel range.
  - `--check` also exits 1 when the port is held, naming the port and, if an offrig side-car
    answers there, the project it reports. The probe is a request the driver refuses before
    calling any tool, so a running side-car is not touched.
  - `offrig_status` reports the lane's `sidecar_port`.
  - Not in this repository: the HTTP driver itself (`serve`) lives outside it, so it still
    has to read the port from `--sidecar-port` and log why it loses a port. See the pull
    request.

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

## 0.1.0 (never released; the first build, part of 1.0.0)

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
