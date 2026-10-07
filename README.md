# offrig

Run big models on rented RunPod GPUs, with a guarantee: they never run on your own
GPU. A desktop app, a CLI and an MCP side-car for agents, over one Rust library.

The side-car lets an agent plan a paid session under a human-set budget, rent the GPUs,
and hand a queue of role-headed tasks to a detached runner. The runner keeps every model
slot busy, revises only against checks that failed, and shuts the pod down when the queue
is empty. A watchdog terminates the pod at the plan's deadline even if everything else is
gone.

## Status

Proven live on 2026-10-02 and 2026-10-03, about $5 in all:

- **Frontier:** 4 × RTX PRO 6000 (384 GB) serving Qwen3-Coder-480B (4-bit AWQ) on SGLang,
  ready in 22 minutes, then 31 handoffs drained in 20 seconds, for $3.59.
- **Swarm data:** one loaded model serves hundreds of agents at once. The 480B reached
  4,059 tok/s at 512 agents; a 30B on one card reached 10,147 tok/s at 256.
- **Runner:** a queue with dependencies and review send-backs, worked with nobody driving,
  the pod shut down on drain.
- **Guarantee:** the local GPU stayed idle through every run.

Built and tested, waiting on a decision: staging the frontier weights on a network volume,
about $21/month (see [Staging](#staging-weights-on-a-network-volume)).

Next: the first real frontier queue, planned in full before launch; code handoffs compiled
and tested on the pod.

## What it does

From one window (or one command), offrig:

1. shows your RunPod balance, live GPU prices and how long the balance will last;
2. launches a pod for a tier, from 1 small card on Ollama up to 4 × RTX PRO 6000 on SGLang,
   and loads its models on the pod;
3. opens an SSH tunnel to the pod;
4. adds the pod's models to Zed as their own provider;
5. runs seven checks that the models cannot land on this machine;
6. shuts the pod down, or terminates it after a stretch with every GPU idle.

Through the side-car, an agent also plans sessions against a budget, keeps project memory
across compaction and restarts, and runs handoff queues unattended (see
[The side-car](#the-side-car-for-agents)).

## The guarantee, and how it holds

- **The model server is unreachable except through the tunnel.** The pod runs a pinned
  engine, Ollama (`ollama/ollama:0.35.0`) or SGLang (`lmsysorg/sglang:v0.5.20-cu130`),
  bound to the pod's own loopback, and the pod exposes only `22/tcp`. There is no public
  HTTP endpoint to find or abuse. A recipe cannot move the engine off loopback.
- **Zed talks to the tunnel, on its own port.** The tunnel listens on `127.0.0.1:11435`.
  Your local Ollama is on `11434`. offrig refuses to put the tunnel on `11434`, so a dead
  tunnel cannot fall through to the local server: the request fails instead.
- **Zed never switches providers.** The pod's models are a separate `offrig` provider in
  Zed. If the pod is down, choosing one of them errors; Zed does not try another provider.
- **The weights never exist locally.** Models are pulled on the pod, by the pod (or
  downloaded there from Hugging Face, or read from a staged network volume).

The guard checks verify this each time, from facts offrig can observe:

| Check | Fails when |
|---|---|
| Tunnel avoids the local Ollama port | the tunnel port is 11434 |
| Zed sends pod models through the tunnel | Zed's provider URL is anything but the tunnel |
| Pod's Ollama is not exposed to the internet | the pod maps port 11434 publicly |
| The tunnel ends at the pod | the model list through the tunnel differs from the list read on the pod over SSH |
| Pod models are not on this machine | a pod model also exists in the local Ollama |
| No pod model shares a name with a local Zed model | a name in the offrig provider is also in Zed's local Ollama list |
| Every model Zed offers is on the pod | Zed offers a model the pod does not have |

## Install

Needs Windows with OpenSSH (built in), Rust 1.98.1 (pinned in `rust-toolchain.toml`),
Zed, and a RunPod account.

1. Put your RunPod API key in the user environment variable `RUNPOD_API_KEY`.
2. Add your SSH public key in RunPod's account settings. offrig uses
   `~/.ssh/runpod_rustline` if present, then `~/.ssh/id_ed25519`.
3. Build: `cargo build --release`. This makes `target/release/offrig-app.exe` (the app),
   `target/release/offrig.exe` (the CLI) and `target/release/offrig-mcp.exe` (the
   side-car).
4. For agents, register the side-car with Claude Code at user scope:
   `claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`. It opens a project's
   store only on first use, so it is harmless in projects that never use it.

## Use

**App:** start `offrig-app`, pick a profile, press **Launch pod**. When it is ready, the
models appear in Zed's agent panel as "RunPod · …". Restart Zed once after the first
launch so it sees `OFFRIG_API_KEY`.

**CLI:**

```text
offrig status                 balance, runway, pods
offrig gpus --count 2         live offers for a GPU count
offrig profiles               tiers and their models
offrig up medium              launch, pull, wire Zed, run the checks, hold the tunnel
offrig up frontier --wait 180  wait up to 3 hours for the GPUs, renting nothing meanwhile
offrig tunnel medium          hold the tunnel to a running pod
offrig check gpt-oss:120b     streamed chat with a tool call, the way Zed sends it
offrig guard                  run the seven checks
offrig pull <model>           pull another model onto the pod
offrig connect                open the pod's /workspace in Zed for remote editing
offrig down medium --yes      terminate the pod
offrig zed-remove             take the provider out of Zed
offrig budget 15              set this project's spending cap for agent sessions (human only)
offrig stage frontier --dc EUR-IS-1 --yes   stage weights on a network volume (bills monthly)
```

## The side-car (for agents)

`offrig-mcp` is an MCP server an agent such as Claude Code calls as an instrument. It
keeps a database per project at `<project>/.offrig/offrig.db` that outlives every pod,
so a session survives compaction or a restart without re-explaining anything.

| Tool | What it does |
|---|---|
| `offrig_status` | Budget, RunPod balance and runway, offrig's pods, the handoff queue with stale work flagged |
| `offrig_offers` | Live GPU offers for a GPU count |
| `offrig_plan` | Prices a session at its worst case (live price x max hours); refused over the budget left |
| `offrig_memory_search` | Searches active project memory, each result with source and date |
| `offrig_memory_record` | Adds a brief, constraint, decision, fact or checkpoint; changes are supersessions with a reason |
| `offrig_handoffs` | Queues role-headed handoffs (each needs an acceptance check; optional deterministic checks), lists them, previews role blocks, shows a handoff's best output (also written to `.offrig/out/`), records outcomes (complete, invalid, violation, fail, retry with feedback) |
| `offrig_launch` | **Spends.** Takes only a `plan_id`: commits the worst case, waits for GPUs renting nothing, boots the pod, opens the tunnel, pulls the models, starts the watchdog. Idempotent per plan |
| `offrig_job` | Launch progress, watchdog liveness, minutes left, spend so far |
| `offrig_ask` | One turn of a handoff on the pod model, context built from the project store; the reply is returned as untrusted output |
| `offrig_run` | Starts a detached runner that keeps every model slot busy: drafts each ready handoff, revises at most twice against failed checks, feeds results to dependent handoffs, then shuts the pod down when the queue is dry (unless `keep_pod`). Work that code cannot check waits in review |
| `offrig_put` | Copies a local file or directory to a job pod (scp); relative pod paths are under `/workspace/job` |
| `offrig_exec` | Runs a bash command on a job pod, detached so it outlives the side-car (`start`), reports running or exited with its exit code and log tail (`status`), or kills it (`stop`) |
| `offrig_get` | Copies a file or directory back from a job pod; do it before the shutdown, which deletes the pod's disk |
| `offrig_shutdown` | **Destroys the pod.** Terminates it and closes the plan's books with measured spend; refused while handoffs are in flight unless given a reason |

Roles come from Role OS (dossiers and starter-pack cards) plus four game roles shipped
here in Role OS's formats: game-designer, systems-designer, narrative-designer,
lore-keeper. The budget cap is set only by a human:

```text
offrig budget 15          set this project's cap (run in the project directory)
offrig budget             show cap, committed, spent, remaining
```

Every launch starts a **watchdog**: a separate process that terminates the pod at the
plan's deadline (committed time + max hours) even if the agent, the session or the
side-car is gone. It never acts on a failed lookup, terminates exactly once, closes the
books, and logs to `.offrig/watchdog-<plan>.log`. If getting a rented pod ready fails,
the launch terminates it instead of leaving it billing.

The design and its evidence are in [docs/sidecar-design.md](docs/sidecar-design.md).

### Lanes: one side-car per project, no collisions

Two projects can run side-cars at once on one RunPod account. Each project gets its own
**lane**: an SSH alias, a tunnel port and a pod-name tag that no other project shares.

| | Plain lane (the CLI, the app, Zed) | A project's lane |
|---|---|---|
| SSH alias | `offrig` | `offrig-<tag>` |
| Tunnel port | `11435` (runner `11436`) | first free of `11500`, `11502`, ... (runner: the port above) |
| Pod name | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| SSH block | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` comes from the project folder's name (`aspire-si`, `ai-jam-sessions`), with a short
hash added when two projects share a folder name. A project's lane is allocated the first
time it plans a session, written to `lanes.toml` in offrig's config directory, and kept:
the same project gets the same lane after every restart. Allocation takes a lock file and
writes the registry atomically, so two side-cars starting together never share a tag,
alias or port. No lane can be `11434` (the local Ollama's port): the range starts at
`11500`, and a registry edited to say otherwise is refused. A plan records its lane, and
its launch, runner, watchdog and shutdown all use that lane, not the global config.

A side-car only ever matches, lists or stops pods named for its own lane. Another
project's lane, the plain lane's `offrig-<profile>` pods and any other pod on the account
are left alone: the launch check for "a pod for this profile is already running" asks only
its own lane, shutdown refuses a pod whose name is not the plan's lane's, and the tunnel's
orphan check kills a stale `ssh` only when its forward and its alias are the lane's own.
Plans made before lanes existed have no lane recorded and keep running on the plain lane,
so a pod launched under the old scheme is shut down by the same plan that started it.

## Tiers

Profiles live in `%APPDATA%\offrig\config.toml` (written on first change). Defaults:

| Profile | GPUs | Models | Typical cost |
|---|---|---|---|
| small | 1 × RTX 2000 Ada / A4000 class | `qwen3:4b` | about $0.25/hr |
| medium | 1 × RTX PRO 6000 (96 GB); A100 or H100 80 GB if none is free | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | $2.09/hr (A100 fallback $1.59) |
| frontier | 4 × RTX PRO 6000 (384 GB), **SGLang** | Qwen3-Coder-480B AWQ 4-bit (252 GB), about 130 GB left for context | $8.36/hr |
| frontier-mini | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B FP8 (31 GB): the frontier engine path, rehearsed cheaply | about $1.7/hr |
| frontier-mini-awq | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B AWQ (17 GB): the frontier's 4-bit MoE kernels, rehearsed cheaply | about $1.7/hr |
| job | 1 × RTX PRO 6000 (96 GB); A100 or H100 80 GB if none is free | none: a **job pod** runs your work, not a model server | $2.09/hr (A100 fallback $1.59) |
| jam | 1 × A40 (48 GB) first; A6000, A5000, 3090, L4 or 4090 if none is free | none: a **job pod** for ai-jam-sessions' singing renders (SoulX-Singer) | $0.49/hr (A40) |

A profile with a `recipe` runs another engine than Ollama: a pinned image
(`lmsysorg/sglang:v0.5.20-cu130`), a Hugging Face model it downloads at start, and
extra server arguments. offrig sets tensor parallelism from the GPU count, the context
length from the profile, and keeps the engine on the pod's loopback; a recipe cannot
override those. For a gated repo, `hf_token_secret` names a RunPod secret, referenced as
`{{ RUNPOD_SECRET_<name> }}` so the token never enters the pod spec. The launch waits
for the engine's `/health` and model list, reports weights on disk while it downloads,
and stops at once (with the engine's log) if the engine exits.

Each profile lists GPU types in priority order; RunPod takes the first with capacity.
When none is free, a profile can wait (`wait_for_gpu_minutes`; frontier waits up to 120 minutes):
offrig checks every minute and creates the pod the moment the GPUs free up. Nothing is rented
while it waits, Ctrl+C or the app's Cancel launch stops it, and if RunPod's price API is down it
simply retries the create each minute. Large multi-GPU setups come and go within minutes.
Prices are secure-cloud prices, read live; the pricing page is not the available price.

### Job pods

A profile with a `job` rents a GPU for work that runs on it, such as a training run, rather
than for serving a model. Its pod runs a pinned PyTorch image
(`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`, CUDA 12.8 for Blackwell)
with sshd and nothing else:

- It serves no model, so there is no tunnel and nothing is wired into Zed. sshd allows no
  forwarding at all (`AllowTcpForwarding=no`); the only way in is ssh to the pod.
- A job profile lists no models and cannot also have a recipe; the config check refuses
  either.
- `offrig up` and the app refuse a job profile before renting anything. It runs through
  the side-car: `offrig_plan profile=job`, `offrig_launch`, then `offrig_put`,
  `offrig_exec` and `offrig_get`. The launch is ready when sshd answers.
- A command runs detached on the pod (`setsid nohup`) in `/workspace/job`, so it outlives
  the side-car and the ssh session. It is sent as base64, so nothing in it is read by the
  ssh shell. Its log and exit status stay in `/workspace/offrig/jobs/`. Hugging Face
  downloads go to `/workspace/hf` on the pod volume.
- The image is a CUDA 12.8 build, so a job profile names the oldest host CUDA version it
  runs on (`min_cuda = "12.8"`) and the pod is created with RunPod's `allowedCudaVersions`
  from it. Without that, a host with an older driver starts the pod and torch finds no GPU,
  after the rent has begun.
- Budget, plan, watchdog and shutdown work as for every other profile. Copy results back
  before `offrig_shutdown`: the pod's disk goes with it.
- `jam` is the job profile ai-jam-sessions renders its singing on: SoulX-Singer needs far
  less than a training card, so it rents a cheap 24-48 GB one. The setup and the session
  live in that repository (`docs/vocal-offrig.md`); offrig knows nothing about singing.

### Staging weights on a network volume

A recipe profile downloads its weights at every launch: for the frontier that was about
20 of 22 minutes to ready (252 GB, $8.36/hr). Staging puts them on a RunPod network
volume once:

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- The volume bills monthly whether a pod runs or not (300 GB for the frontier is about
  $21/month at $0.07/GB), so only a human stages; no agent tool can.
- A volume lives in one data center, so the profile's pods then launch only there, and
  offers and plans are priced there. Pick one with network storage and the profile's
  GPUs; `offrig gpus` and RunPod's console show where they are.
- The download runs on the cheapest GPU pod available in that data center. The pod is
  terminated on success, on failure, or on timeout.
- The volume is recorded in the profile before the download starts, so a failed stage
  is never forgotten; re-run to resume, or `--remove`.
- A staged launch runs Hugging Face offline, only when the stage completed (a marker on
  the volume). A half-staged volume downloads the rest instead of failing.

## Money safety

- Before a launch, offrig shows the cheapest free match and your runway with the pod
  running. Under one hour of runway it refuses unless you override, because at zero
  RunPod stops every pod on the account, including ones offrig does not manage.
- Auto-stop terminates the pod after 30 minutes with every GPU under 5% (configurable,
  or off).
- Closing the app with a pod running asks whether to terminate it or keep it running.
- offrig only touches pods it named: `offrig-<profile>` for the CLI and the app,
  `offrig-<tag>-<profile>` for a project's side-car lane (see Lanes). A side-car never
  touches another lane's pods, the plain lane's, or any other pod; those are listed,
  never changed.
- For agent sessions, the cap is enforced before any spend: a plan's worst case (live
  price × max hours) is committed against the human-set budget and refused over it, and
  a launch takes only a plan id, so an agent cannot name its own price.
- Every side-car launch has a watchdog that terminates the pod at the plan's deadline,
  and the runner shuts the pod down as soon as its queue is empty.

## What it changes on your machine

| What | Where | Undo |
|---|---|---|
| Zed provider `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`; the first original is kept as `settings.json.offrig.bak` |
| Zed default model (only if you ask) | same file | `offrig zed-remove` restores the previous default |
| `OFFRIG_API_KEY` (placeholder; Zed wants a key) | user environment | `setx OFFRIG_API_KEY ""` or remove it in System Properties |
| SSH alias `offrig` | `~/.ssh/config`, between `# >>> offrig:offrig >>>` markers | delete the marked block |
| SSH alias `offrig-<tag>`, one per project that launched from a side-car | `~/.ssh/config`, between `# >>> offrig:offrig-<tag> >>>` markers | delete the marked block |
| Project lanes | `%APPDATA%\offrig\lanes.toml` (project path, tag, alias, tunnel port) | delete the project's entry while no pod runs in its lane, or the whole file |
| Pod host keys | `~/.ssh/known_hosts_offrig` | delete the file |
| Settings | `%APPDATA%\offrig\config.toml` | delete the file |
| Staged weights (only with `offrig stage --yes`) | a RunPod network volume `offrig-<profile>`; bills monthly | `offrig stage <profile> --remove --yes` |

Comments and layout in Zed's settings are preserved: edits go through a JSONC syntax tree.

## Threat model

- **RunPod API key.** Read from `RUNPOD_API_KEY`; never written to disk or logs. The Zed
  provider is deliberately not named `runpod`, because Zed would then read
  `RUNPOD_API_KEY` and send it to the model server.
- **Model server.** Reachable only through SSH with your key. Password login is off on
  the pod, and sshd allows only local forwarding.
- **Host keys.** Pinned per endpoint in a separate known-hosts file. offrig forgets a key
  only when the pod endpoint changes, because RunPod reuses ip:port pairs across pods.
- **Shell injection.** Model names are validated against Ollama's name syntax before
  they reach a remote shell.
- **Orphaned tunnels.** If offrig dies, its `ssh` may keep holding the port. On the next
  start offrig kills it, but only if the listener is `ssh.exe` carrying offrig's exact
  forward spec. Anything else on the port is refused, never killed.
- **No telemetry.** offrig talks only to RunPod's API, your pod, and your local Ollama
  (to compare model lists).

## Tests

`cargo test --workspace` runs 130 tests:

- **The core library:** RunPod parsing, pod specs for both engines, SSH config, Zed JSONC
  edits, guard rules, cost and idle logic, the store and its migrations, roles, context
  assembly, deterministic checks, the runner's decisions, the watchdog, and staging,
  including a mock RunPod that proves a failed stage terminates its pod.
- **The app:** state handling plus click-through UI tests in egui's test harness.
- **The side-car:** end to end over stdio against a mock RunPod, the real watchdog
  process, and the real runner process against a mock pod model.

CI also runs fmt, clippy with warnings as errors, `cargo deny` and `atlas check`.

### Live test record (2026-10-02, medium tier, A100 80GB, about $0.45)

- Pod up in about 80 s; sshd, tunnel and the pod's Ollama 0.35.0 answering.
- 97 GB of models pulled on the pod at roughly 150–250 MB/s.
- `qwen3-coder:30b-a3b-q8_0` and `gpt-oss:120b` each answered a streamed chat with a
  correct tool call through the tunnel. They took 36 GB and 64 GB of the pod's VRAM; the
  local Ollama loaded nothing and was not on the local GPU at all.
- All seven guard checks passed, from the CLI and from the app.
- An abruptly killed CLI left its `ssh` holding the port; the next run reclaimed it.
- The app's tunnel, checks, model test and shutdown were driven through its buttons.

Bugs the live run found, now fixed and covered: a backgrounded `&&` list kept ssh's
stdout open and hung the pull start; the pod list lacked GPU types without
`includeMachine=true`; the launch check counted a running pod's price twice.

### Side-car rehearsal (2026-10-03, small tier, RTX 2000 Ada, $0.08 booked)

The installed `offrig-mcp` driven over stdio, the way an agent calls it:

- `offrig_plan` priced 0.5 h at $0.15 worst case; `offrig_launch` committed it, started
  the watchdog, and a second call returned the same job. One pod was rented, at $0.24/hr.
- SSH up 100 s after the rent, `qwen3:4b` pulled, ready at 150 s.
- `offrig_ask` ran a game-designer handoff in 46 s; the reply met its acceptance check
  and kept the five-bullet constraint from memory.
- The pod reached the internet (Wikipedia, GitHub API). Given data the pod fetched, the
  model answered current questions correctly; asked cold, it said it had no live access.
- `offrig_shutdown` from a fresh side-car process terminated the pod and closed the
  books; the watchdog saw the plan close and exited. The local GPU stayed idle throughout.

Found and fixed: the pod served one request at a time (`OLLAMA_NUM_PARALLEL=1`); four
slots took 8 parallel requests from 40 to 102 tok/s on the same GPU, so every profile now
has `parallel = 4`. `complete` was refused without a reason (it now defaults to
"acceptance check met"; failures still need one). Status suggested recording a brief
while a session was live. Thinking text that leaks into a reply is stripped, and a reply
emptied by thinking says to raise `max_tokens`.

### Runner rehearsal (2026-10-03, small tier, RTX 2000 Ada, $0.04 booked)

Five handoffs, one depending on another, worked by `offrig_run` with nobody driving:

- Four handoffs in flight at once on four slots (12.9 GB of 16 GB VRAM); the dependent
  started the moment its dependency completed and built on its result.
- The three handoffs whose checks covered acceptance completed on their own; the rival
  backstories (partial checks) and the lore (no checks) went to review.
- Review sent the lore back ("the river is named after the project"); the live runner
  adopted it and revised against the feedback ("Veyl River").
- The queue drained in 6.5 minutes (6 turns, 20,861 tokens); the runner shut the pod
  down itself.

Learned: deterministic checks verify structure, not design quality. The 4B model passed
"three verbs" with thin verbs, so `accept_on_checks` is for structural work and design
work goes to review. qwen3:4b spent about 4,000 tokens thinking per turn, even on three
lines of lore. A queue keeps every slot busy only when it holds enough independent
handoffs; a dependency chain runs one at a time.

### Frontier and SGLang runs (2026-10-03, $4.27 booked)

| Run | Pod | Ready after | Queue | Booked |
|---|---|---|---|---|
| frontier-mini (Qwen3-Coder-30B FP8) | 1 × RTX PRO 6000 | 5.5 min | 4 handoffs in 15 s | $0.30 |
| frontier-mini-awq (Qwen3-Coder-30B AWQ) | 1 × RTX PRO 6000 | 4 min | 4 handoffs | $0.25 |
| **frontier (Qwen3-Coder-480B AWQ)** | **4 × RTX PRO 6000** | **22 min** (252 GB at 278 MB/s, then load) | **31 handoffs in 20 s** | **$3.59** |
| 30B swarm sweep | 1 × RTX PRO 6000 | 10 min (slow pod placement) | sweep only | $0.43 |

- SGLang v0.5.20 (cu130) runs on Blackwell: flashinfer attention, `awq_marlin` for the
  4-bit MoE weights, tensor parallel over PCIe on four cards; `/dev/shm` was 352 GB.
- The frontier's work was clearly better than the small models': in-world barks, and a
  Rust module that compiled and passed its three tests (checked locally). The 30B FP8
  run's version of the same task did not compile.
- One loaded model serves a whole swarm; no copies are needed. Concurrency sweep with
  384-token replies, total tokens per second:

| Agents | 480B on 4 GPUs | 30B AWQ on 1 GPU |
|---:|---:|---:|
| 1 | 88 | 118 |
| 8 | 394 | 809 |
| 16 | 658 | 1,692 |
| 32 | 990 | 2,525 |
| 64 | 1,521 | 4,088 |
| 128 | 2,289 | 6,762 |
| 256 | 3,208 | 10,147 |
| 512 | 4,059 | — |

  Per-agent speed falls as agents are added (480B: 88 → 24 tok/s at 64), but total
  throughput keeps rising; the 480B's gains flatten past 256. The frontier's KV cache
  holds 398,526 tokens, so with real handoff contexts of 2-8k tokens the frontier tier
  now runs 64 in flight and the one-card SGLang tiers 32.

Found and fixed along the way: revisions padded output to pass a heading check (now a
built-in `no_repeats` check, and revisions restructure in place); a role's own required
output leaked into deliverables (the handoff now sets the format); heading feedback
names the Markdown form; an unchanged revision stops instead of repeating.

Not yet verified live: a chat
sent from Zed's agent panel itself (the request shape Zed uses is tested directly).

## Standards compliance

Scored against the studio's workflow standards (0 missing, 1 partial, 2 present,
3 exemplary).

- **PIN_PER_STEP: 2.** Pod images are pinned to version tags (`ollama/ollama:0.35.0`,
  `lmsysorg/sglang:v0.5.20-cu130`; a recipe refuses `latest`), the compiler to 1.98.1,
  dependencies by `Cargo.lock`, and the Atlas engine to the fleet's 1.24.0. Each handoff
  turn records its model, role hash and prompt hash. Models are pinned by tag or repo id,
  not by digest.
- **ANDON_AUTHORITY: 3.** Every step halts the run on a defect: a plan whose weights
  exceed the disk is refused before any spend; a failed pull stops the launch; a Zed edit
  that does not read back is not written; a broken settings file is reported, never
  rewritten; CI blocks on fmt, clippy, tests, licences and advisories.
- **NAMED_COMPENSATORS: 2.** Every irreversible action has an undo, listed below.
- **DECOMPOSE_BY_SECRETS: 2.** One module per thing that changes for its own reasons:
  RunPod's API (`runpod`), the pod's contents (`spec`), the transport (`tunnel`,
  `remote`), each local file offrig edits (`sshconfig`, `zed`), and the rules (`guard`,
  `cost`). Front ends hold no logic beyond presentation.
- **UNCERTAINTY_GATED_HUMANS: 2.** offrig asks only where the outcome is costly or
  lossy: launching under an hour of runway, terminating a pod (with what is lost stated),
  and quitting with a pod still billing. Two decisions belong to a human alone, and no
  agent tool can make them: the budget cap and staging a volume, which bills monthly.
  Handoff output that code cannot check waits in review instead of completing.
- **EXTERNAL_VERIFIER: n/a.** No specialized claims.

**Compensators**

| Action | Undo | State after undo | Owner |
|---|---|---|---|
| Create a pod (starts billing) | `offrig down <profile> --yes`, the app's Shut down, or auto-stop | pod terminated, billing stopped | the operator running offrig |
| Terminate a pod | none for its disk; relaunch the profile and the models re-pull (a network volume keeps them) | new pod, same profile | the operator |
| Write the Zed provider or default model | `offrig zed-remove`, or restore `settings.json.offrig.bak` | Zed as before offrig | the operator |
| Set `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""` or delete it in System Properties | variable gone | the operator |
| Write the SSH alias | delete the marked block in `~/.ssh/config` | config as before | the operator |
| Allocate a project lane (the project's first `offrig_plan`) | delete the project's entry from `lanes.toml` once no pod runs in its lane; a new plan allocates again | lane free for reuse; the alias block is separate (row above) | the operator |
| Write a lane's SSH alias block (a side-car launch) | delete the `# >>> offrig:offrig-<tag> >>>` block in `~/.ssh/config` | config as before; other lanes' blocks untouched | the operator |
| Pull a model on the pod | `ollama rm <model>` on the pod, or terminate the pod | model gone | the operator |
| Kill an orphaned offrig tunnel | none needed; only an `ssh` with this lane's exact alias and forward is ever killed, never another lane's | port free | offrig |
| Side-car launch (`offrig_launch`) | `offrig_shutdown`; automatic if setup fails; the watchdog at the deadline; the runner when its queue drains | pod terminated, plan closed with measured spend | the calling agent, with the watchdog as backstop |
| Stage a volume (`offrig stage --yes`, bills monthly) | `offrig stage <profile> --remove --yes` | volume deleted, profile back to downloading | the human who staged it |
| Start a job on a job pod (`offrig_exec action=start`) | `offrig_exec action=stop`, or `offrig_shutdown` | job killed with everything it started; its log stays until the pod is gone | the calling agent |
| Copy files to or from a job pod (`offrig_put`, `offrig_get`) | delete the copy (on the pod, `offrig_exec`; here, the file) | as before the copy | the calling agent |

## Layout

```text
crates/offrig-core   library: RunPod client, pod specs and engine recipes, tunnel, remote
                     ops, Zed and SSH edits, guard, cost and idle logic, session workflow,
                     project lanes, project store, roles, context assembly, checks,
                     runner decisions, watchdog, staging
crates/offrig-cli    `offrig` command line
crates/offrig-app    `offrig-app` desktop app (egui)
crates/offrig-mcp    `offrig-mcp` side-car: MCP server for agents, plus the detached
                     watchdog and runner processes
docs/                the side-car's design and its research grounding
atlas/               Atlas map of the repo (regenerate with `atlas map`)
```

## License

MIT. See [LICENSE](LICENSE).
