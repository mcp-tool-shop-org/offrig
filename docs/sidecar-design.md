# offrig side-car: design

An MCP server (`offrig-mcp`) that an agent calls as an instrument: plan a paid GPU
session, launch it, keep the pod model busy with role-headed handoffs, keep project
memory across compaction and restarts, and shut down with the work harvested.

Status: design, 2026-10-02. Grounded by a five-question study-swarm run the same day.

## Standards compliance

Scored 0 to 3 against the studio workflow standards.

- **PIN_PER_STEP: 2.** The pod image is pinned by tag. The engine version and engine
  flags come from a pinned recipe. Each handoff records model, role id, role-data
  hash and prompt hash. Remaining gap: models are pinned by tag, not by digest.
  Remediation: record the model digest in the handoff row (phase 2).
- **ANDON_AUTHORITY: 2.** These steps halt the session:
  - the budget ledger refusing a launch;
  - a failed acceptance check, which blocks the handoff;
  - an ownership violation, where a handoff touched files outside its scope, which also
    blocks it;
  - a guard failure, which halts the queue;
  - a failed harvest, which blocks shutdown until overridden with a reason.
- **NAMED_COMPENSATORS: 2.** The table below gives every irreversible action its undo
  and an owner. The journal is written before every side effect, and reconcile runs on
  start.
- **DECOMPOSE_BY_SECRETS: 2.** Each of these modules changes for its own reasons and
  owns its state:
  - budget ledger
  - memory store
  - handoff queue and state machine
  - role rendering
  - engine recipe
  - transport (the existing core)
- **UNCERTAINTY_GATED_HUMANS: 2.** A human is asked twice: at launch, approving the
  plan with its worst-case cost, and at a terminate that would lose unharvested work.
  Nothing else asks. Each ask is framed contrastively.
- **EXTERNAL_VERIFIER: n/a.** The checks are deterministic: tests, acceptance checks and
  the guard. No specialized claims are made.

## Research grounding

Each finding is tied to the decision it drives. The sources were gathered by research
agents on 2026-10-02. Papers marked † were read as abstracts only, or reported second-hand.

**Tool design**
- MCP spec 2026-07-28 (modelcontextprotocol.io/specification/2026-07-28):
  - Tasks moved to the extension `io.modelcontextprotocol/tasks`, and polling replaced
    blocking reads.
  - Elicitation became a multi-round-trip `input_required` result.
  - Tool annotations are untrusted hints.
  - Stateful work should use server-minted handles.

  → **Long jobs return a handle; `offrig_job` reports progress. No feature depends on
  client support for tasks or elicitation.**
- Anthropic Engineering, "Writing effective tools for AI agents" (2025): workflow-shaped
  tools, concise outputs, and errors that say what to do next. → **About 10 tools, each
  returning a `next_action` line.**
- Gan & Sun, RAG-MCP (2025): tool-selection accuracy fell from about 85% at 5 tools to
  about 45% at 20. → **Keep the tool count small, with an `offrig_` prefix and a fixed
  order.**
- "MCP Tool Descriptions Are Smelly!" (2026, arXiv:2602.14878): better descriptions
  added a median 5.85 points, but longer ones sometimes hurt. → **Each description gets
  a fixed shape: purpose, when to use, returns, cost and side effects. They get tuned
  against an eval of real transcripts.**

**Spending guardrails**
- AWS Budgets docs: provider budgets lag 8-12 hours and are alerts, not caps. → **The cap
  is enforced in offrig at commit time, from price × maximum hours.**
- Terraform saved-plan apply: approval binds to a specific plan artifact. → **Plan, then
  apply. `offrig_launch` takes only a `plan_id`, and the server recomputes the cost.
  Caller-supplied `confirm` or `dry_run` flags are never trusted.**
- DN42 agent incident (May 2026, via InfoQ†): template re-applies created duplicate
  resources, about $6.5k. → **The plan id is the idempotency key, so a retry returns the
  existing pod. A reconcile against RunPod's pod list runs on every start.**
- Anthropic, Claude Code auto mode (2026): users approve 93% of permission prompts. →
  **Ask rarely: approve the plan's launch and confirm a lossy terminate. Everything else
  runs without asking.**
- CaMeL (Debenedetti et al. 2025, arXiv:2503.18813); "The Attacker Moves Second" (2025)†;
  Meta's Agents Rule of Two (2025)†. → **Pod output is untrusted data. Spending parameters
  come only from the plan record, never from model text.**

**Serving engine (frontier: 4× RTX PRO 6000, PCIe, no NVLink)**
- PagedAttention (Kwon et al. 2023, arXiv:2309.06180). SGLang RadixAttention (Zheng et al.
  2024, arXiv:2312.07104). 2026 benchmarks†: llama.cpp is flat past about 10 concurrent
  requests, and SGLang leads on shared-prefix multi-turn work. → **The frontier tier
  serves with SGLang, with vLLM as fallback. Ollama remains for small and medium.**
- vLLM forum, sm120 RTX PRO 6000 report†: TP=4 was best, expert parallelism over PCIe
  collapsed, NVFP4 kernels produced garbage, and Marlin W4A16 worked. → **TP=4, EP off,
  AWQ 4-bit Qwen3-Coder-480B (236 GB), `--kv-cache-dtype fp8`.**
- Run:ai Model Streamer (NVIDIA blog)†: weights load 3-6× faster. → **Stage the weights
  on local NVMe, and budget 10-15 minutes of cold start.**

**Memory**
- MemGPT (Packer et al. 2023, arXiv:2310.08560): durable state lives outside the window,
  edited through tools. → **Memory read and write are tools, and context is rebuilt from
  the store.**
- "Lost in Compaction" (2026, arXiv:2608.11242): free-text compaction kept about 17% of
  constraints; a separate extractor kept more than 90%. "Knowledge Objects" (2026,
  arXiv:2603.17781): addressable fact tuples held at 100%. → **Typed, addressable
  records. Constraints are a class that is always injected and never summarized.**
- Nakayashiki (2026, arXiv:2608.25553): withdrawn constraints still drove decisions in
  about 75% of episodes. StateMemBench (2026, arXiv:2608.19652): explicit supersession
  tracking added 32-67 points. → **Rows are never edited. A new row supersedes an old
  one through an explicit edge, and retrieval returns active rows only.**
- Liu et al. 2023, Lost in the Middle (arXiv:2307.03172); Chroma, Context Rot (2025);
  "Harness the Memory" (2026, arXiv:2608.15008): excess retrieval hurts action-critical
  work. → **Fixed budget and order. Brief, constraints and task go first. A small
  retrieved block goes in the middle. Instruction and latest checkpoint go last.**
- BM25 beat dense retrieval in Akarsu et al. 2026 (arXiv:2604.01733), on a different
  domain. → **SQLite FTS5 first. Embeddings only if measured recall is poor.**

**Roles**
- Zheng, Pei, Jurgens (EMNLP Findings 2024, arXiv:2311.10054) and "Playing Pretend"
  (2025, arXiv:2512.05858): personas do not raise factual accuracy. → **Correctness lives
  in the acceptance check, not the role.**
- Kong et al. (NAACL 2024, arXiv:2308.07702) and Xiao et al. (2026, arXiv:2605.29420)†:
  roles shape focus and structure, and structured role setup helps reasoning. → **Role
  OS aptitudes render as concrete behaviours, not identity statements.**
- Gupta et al., "Bias Runs Deep" (ICLR 2024, arXiv:2311.04892); "Too Nice to Tell the
  Truth" (2026)†: irrelevant persona details bias reasoning, and agreeable personas raise
  sycophancy on 7-8B models. → **Voices stay short and task-relevant. Candor and
  skepticism become explicit instructions.**
- Kim et al. (2025, arXiv:2512.08296): multi-agent setups gained up to 81% on
  decomposable tasks and lost up to 70% on sequential ones. "Two Calls Beat Five Agents"
  (2026)†. → **Parallel handoffs only for independent work. Sequential work stays one
  handoff with checkpoints.**
- Lexical bans: no peer-reviewed test found. → **Bans are optional and A/B tested per
  model before being trusted.**

**Prior art in the studio**
- dogfood-swarm control plane (`testing-os/packages/dogfood-swarm`):
  - SQLite is the truth.
  - One state machine governs runs; illegal transitions throw.
  - Blocked states need an override with a reason.
  - Receipts are derived from the database.
  - One shared liveness predicate serves both `status` and `resume`.

  → **Handoffs use the same rule set. Status computes liveness with the same function the
  reaper uses.**
- engine-room (recipe layer in `readouts/tensor-engine-knowledge`): engine knowledge lives
  in recipes, action lives in the executor. → **Pod engine flags come from a recipe, not
  from code.**

## Architecture

```text
Claude Code / Zed agent
        │  MCP (stdio)
offrig-mcp ──────────────── offrig-core (existing: RunPod, tunnel, guard, Zed wiring)
   │                                  │
   ├─ ledger   (budget, plans, journal)│
   ├─ memory   (typed records + FTS5)  │
   ├─ queue    (handoffs, state machine)
   ├─ roles    (renders Role OS data)  │
   └─ jobs     (long work in the background, state in SQLite)
        │
   one SQLite file per project:  <project>/.offrig/offrig.db   (committed or synced)
```

The pod is disposable and the database is not. The database sits with the project. The
side-car reads and writes it locally, and whatever runs on the pod reaches it through
the side-car's tools.

## Tool surface (11 tools, fixed order)

| Tool | Kind | What it does |
|---|---|---|
| `offrig_status` | read | Balance, runway, pods, budget left, active plan and job, guard summary. Liveness is computed, never stored. |
| `offrig_offers` | read | Live GPU offers for a count, filtered to usable cards. |
| `offrig_plan` | write, no spend | Makes a plan from a profile, maximum hours and budget cap. Optional `max_price_hr` (total $/hr) and `no_fallback` narrow the GPU list, the profile's `min_vram_gb` drops small cards, and the worst case is `max_hours x min(max_price_hr, dearest listed price left)`. The plan stores the list and the profile's CUDA floor, and the launch rents only from them. Optional `wait_minutes` (stored with the plan, default the profile's `wait_for_gpu_minutes`; 20 for `job`) is how long the launch retries quietly when the list has no capacity, cut to the plan's remaining time. Returns `plan_id`, worst-case cost and runway after. |
| `offrig_launch` | **spends** | Takes `plan_id` only. Starts a background job (wait for GPUs, boot, pull, tunnel, guard). Returns `job_id`. Idempotent per plan. |
| `offrig_job` | read | Job state, progress, pull percent and `next_action`. While the pod boots the step is derived from the pod's state at the call (RunPod's answer and whether sshd answers), and capacity retries are counted in `progress.capacity_wait`. The rented host CUDA is measured with `nvidia-smi` once ssh is up. |
| `offrig_memory_search` | read | FTS over active records, filterable by kind, task and tag. Each result carries its source and date. |
| `offrig_memory_record` | write | Adds a fact, decision, constraint or checkpoint, with provenance. `supersedes` is required when it contradicts a record. |
| `offrig_handoffs` | write | Adds handoffs (role, mission, acceptance check, deterministic checks, file scope, dependencies) to the queue, lists them, shows a handoff's best output, and records review outcomes. |
| `offrig_ask` | read-ish | Sends one role-headed handoff turn to the pod model, with context assembled from memory, and returns the reply. |
| `offrig_run` | **destroys at the end** | Starts the detached runner that works the queue on the pod model and shuts the pod down when nothing is left (unless `keep_pod`). Idempotent while a runner is alive. |
| `offrig_shutdown` | **destroys** | Terminates the pod. Refuses while work is unharvested unless given a reason, and records the reason. |

The runner (phase 3a, below) works text handoffs. Code handoffs with a branch each, run
on the pod, come later, behind the same queue.

## Memory store schema (SQLite + FTS5)

```text
records(id, kind[brief|constraint|decision|fact|checkpoint], body, status[active|superseded|withdrawn],
        supersedes_id, superseded_by, reason, author, source, task_id, tags, created_at, valid_to)
records_fts(body, tags)                       -- FTS5, active rows only at query time
plans(id, profile, gpu_count, gpu_types, max_hours, max_price_hr, worst_case, budget_cap, created_at, state)
ledger(id, plan_id, kind[commit|actual|release], amount, at, note)     -- append-only
journal(id, action, plan_id, intent_json, outcome, at)                 -- written before side effects
jobs(id, plan_id, kind, state, progress_json, started_at, heartbeat_at, error)
handoffs(id, role_id, mission, acceptance, scope_json, depends_on, state, attempts, branch,
         model, role_hash, prompt_hash, result_record_id, created_at)
handoff_events(id, handoff_id, from_state, to_state, reason, override, at)
```

Checkpoint body is fixed-field JSON: goal, done, next, open questions, decision ids,
files touched, fact ids.

## Context assembly (per turn, fixed budget)

1. Role block (rendered behaviours, voice, lens, quality bar, escalation)
2. Project brief and every active constraint
3. The handoff: mission, deliverable, acceptance check, scope
4. Retrieved facts and decisions (top-k from FTS, small)
5. Latest checkpoint for this handoff
6. The instruction for this turn

## Handoff state machine (mirrors the swarm law)

`pending → dispatched → running → complete | failed | timed_out | invalid_output | ownership_violation`.
`failed` and `timed_out` may be redispatched. `invalid_output` (acceptance check failed)
and `ownership_violation` (files outside scope) are blocked: they move only on override
with a reason. Every transition is logged. One liveness predicate serves both status and
the reaper.

## Compensators

| Action | Undo | State after | Owner |
|---|---|---|---|
| `offrig_launch` (rent pod) | `offrig_shutdown`, the budget watchdog, or the TTL | pod terminated, billing stopped | the calling agent; watchdog as backstop |
| A commit in the budget ledger | a `release` entry when the plan ends unspent | budget restored | offrig |
| `offrig_shutdown` | none for the pod disk; harvest first, then relaunch from the plan | new pod; work restored from git and the database | the calling agent, with a human for unharvested work |
| `offrig_memory_record` | a superseding record with a reason (rows are never deleted) | old row superseded | the calling agent |
| Zed provider write (existing) | `offrig zed-remove` | Zed as before | operator |

## Phases

1. **Core state, no spend.** Memory store, ledger, plans, journal, handoff state machine
   and context assembly in offrig-core, with tests. Then the MCP server with the read
   tools and plan, memory and handoff tools, tested over stdio. Register it with Claude
   Code.
2. **Launch and jobs.** Done 2026-10-02, tested against a mock RunPod, and rehearsed
   live on the small tier 2026-10-03 ($0.08; see the README's record). `offrig_launch` (plan id only,
   idempotent, preflight before any commit, a failed post-rent setup terminates the pod),
   `offrig_job`, `offrig_ask` (context from the store, untrusted reply) and
   `offrig_shutdown` (refused with handoffs in flight unless given a reason). The
   watchdog is a detached process (`offrig-mcp --watchdog <plan>`) that leaves the host's
   job object where Windows allows, so closing the session does not kill it. Schema v2
   adds the plan clock and jobs, with a tested migration from v1.
3. **Runner and engine recipe.** 3a, the handoff runner: built 2026-10-03, tested end
   to end against a mock pod model, and rehearsed live the same day ($0.04; see the
   README's record). 3b, the SGLang frontier recipe
   (TP=4, AWQ, full-precision KV): done 2026-10-03, rehearsed on one RTX PRO 6000
   (FP8 and AWQ), then run on 4× RTX PRO 6000 ($3.59; see the README's record). 3c, the first real frontier
   queue, once Mike funds it. Later: code handoffs with a branch each, run on the pod.

## Phase 3a: the handoff runner

Research gathered by three research agents on 2026-10-03. † marks sources read as
abstracts or summaries only; none were read in full. The numbers are the papers' headline
claims.

**Revision turns**
- Huang et al. 2023, "Large Language Models Cannot Self-Correct Reasoning Yet"
  (arXiv:2310.01798)†; Stechly, Valmeekam, Kambhampati 2024 (arXiv:2402.08115)†. Without
  an external signal, self-correction often lowers accuracy: the model invents faults in
  correct answers. A sound external verifier gives large gains.
- Kamoi et al. 2024, TACL survey (arXiv:2406.01297)†. Prompted self-correction works
  reliably only with reliable external feedback, or on easily decomposed tasks.
- Xu et al. 2024, "Pride and Prejudice" (arXiv:2402.11436); Pan et al. 2024,
  "Spontaneous Reward Hacking in Iterative Self-Refinement" (arXiv:2407.04549). A model
  grading its own revisions scores them higher each round while human-judged quality
  stalls or falls; smaller models are hit hardest.
- Javaji et al. 2025, "Another Turn, Better Output?" (arXiv:2509.06770). Early turns
  help. Vague "improve it" feedback plateaus or regresses; feedback on a named dimension
  keeps helping.

→ **A draft, then at most two revision turns. A revision runs only to fix named failed
checks, and the turn quotes them. There is no free-form self-critique turn.**

**Acceptance**
- Tan et al. 2024, JudgeBench (arXiv:2410.12784)†. On objective correctness, strong
  judges scored barely above chance.
- Panickssery et al. 2024 (arXiv:2404.13076)†; Pombal et al. 2026 (arXiv:2604.06996)†.
  Judges favour their own outputs. Even on binary rubrics, a judge was over 50% more
  likely to wrongly pass a criterion when the output was its own.
- Zhou et al. 2023, IFEval (arXiv:2311.07911)†. Verifiable instructions are checked by
  code, without judge bias.
- Lee et al. 2024, CheckEval (arXiv:2403.18771)†; Wei et al. 2025, RocketEval
  (arXiv:2503.05142)†. Decomposed binary checklists make small judges agree more.
  Furuhashi et al. 2025 (arXiv:2508.15218)† found the gains inconsistent; vague criteria
  stay the real problem.

→ **Handoffs carry optional deterministic checks (heading present, list items under a
heading, words, substrings), evaluated in code. A handoff completes on its own only when
its checks pass and its author marked them as covering the acceptance check
(`accept_on_checks`). Otherwise it waits in `review` for the orchestrating agent, with
the output and every check result. The pod model never grades its own work. A judge
from a different model family is future work.**

**Keeping the pod fed**
- Kwon et al. 2023, PagedAttention (arXiv:2309.06180)†; Zheng et al. 2024, SGLang
  RadixAttention (arXiv:2312.07104)†. KV memory caps concurrency, and shared prefixes
  are reused across requests.
- Red Hat 2025, "Ollama vs. vLLM"†. Ollama's throughput plateaus at its slot count;
  extra requests only add latency. Measured here 2026-10-03: 1 slot gave 40 tok/s and
  4 slots gave 102 tok/s, under 8 parallel requests.
- Chen et al. 2026, CONCUR (arXiv:2601.22705)†. Too many long-lived agents thrash the KV
  cache before memory is full; admission control won up to 4×.
- Luo et al. 2025, Autellix (arXiv:2502.13965)†; Lin et al. 2024, Parrot
  (arXiv:2405.19888)†. Program-level scheduling, which prioritises work already under
  way and exposes the call graph, avoids head-of-line blocking.

→ **In flight: the profile's `parallel` slots plus one. Order: revisions before new
drafts, then the handoff with the longest chain of dependents. The role block and brief
open every prompt byte-identical. Every turn has a `max_tokens` cap.**

**Architecture.**
- **Process.** `offrig_run` starts a detached process, `offrig-mcp --runner <plan>`, with
  its own tunnel on `tunnel_port + 1`. Like the watchdog, it outlives the session.
- **Queue.** It works ready handoffs concurrently through the turn loop above. Each turn
  is stored in the `outputs` table and checked; the final output of a dependency is put
  in its dependents' context.
- **Progress.** It heartbeats in-flight handoffs and reports progress on a `run` job,
  read with `offrig_job`.
- **Stopping.** It exits when the plan stops being committed. When the queue has nothing
  left to work, it shuts the pod down unless told to keep it.
- **Review.** `offrig_handoffs action=output` shows a handoff's final output and checks.
  `complete`, `invalid` or `retry` (with feedback recorded as a checkpoint) closes
  review.
- **Scope.** Text work products only. Code handoffs with a branch each, run on the pod,
  come later.

**Standards compliance (runner).** PIN_PER_STEP 2: each turn records model, role hash
and prompt hash. ANDON_AUTHORITY 3: failed checks block auto-completion, blocked
handoffs never auto-retry, and the runner stops when the plan closes (tested).
NAMED_COMPENSATORS 3: the runner's only irreversible act is the shutdown when the queue
empties, which is the session compensator itself; the watchdog stays the backstop.
UNCERTAINTY_GATED_HUMANS 2: work that code cannot check is routed to review, not
auto-accepted. EXTERNAL_VERIFIER: n/a (deterministic checks only).

## Phase 3b: recipe engines (SGLang)

Facts gathered by a research agent on 2026-10-03, from Docker Hub, the SGLang releases
and docs, Hugging Face model cards and the RunPod docs. † marks second-hand or
unconfirmed.

- SGLang v0.5.21 (2026-10-02) and v0.5.20 (2026-09-18) ship CUDA 13 images (`-cu130`).
  The CUDA 12 images are retired at v0.5.19, and there is no separate Blackwell tag.
  → **Pin `lmsysorg/sglang:v0.5.20-cu130`, not a day-old release.**
- fp8 KV cache corrupted output on sm_120 in two reports: sgl-project/sglang#19603, and
  the rtx6kpro notes†. → **Full-precision KV by default. AWQ 252 GB on 384 GB still
  leaves about 130 GB for context.**
- `/v1/models` answers 200 while SGLang is still warming up; `/health` waits for it
  (gpustack PR #6302†). → **Readiness needs both `/health` and the served model listed.**
- Qwen3-Coder-480B AWQ: `QuantTrio/Qwen3-Coder-480B-A35B-Instruct-AWQ`, 252 GB
  (measured from the HF API). No report found of it running under SGLang on sm_120. →
  **Two cheap rehearsals first on one RTX PRO 6000: `frontier-mini` (FP8 30B) for the
  engine path, and `frontier-mini-awq` (QuantTrio's AWQ 30B, 17 GB) for the 4-bit MoE
  kernels.** If either fails, the community's sm_120 fallback is
  `--attention-backend triton --moe-runner-backend triton`†.
- Qwen3-Coder is non-thinking (model card). → **The thinking cost measured on qwen3:4b
  does not apply at the frontier.**
- RunPod secrets: `{{ RUNPOD_SECRET_<name> }}` in env. Substitution is documented for
  pods and templates, but not for the REST `POST /pods` env†. → **Supported. It will
  be verified the first time a gated model is used.**
- First download of 252 GB is estimated at 20 to 40 minutes, plus 5 to 15 minutes of
  load. The estimate is unmeasured†. → **75-minute engine timeout. A network volume
  pre-staged with the weights is the cure for repeated frontier runs.**
  `HF_HUB_ENABLE_HF_TRANSFER` is ignored by huggingface_hub 1.x; hf_xet is the default.

## Staging (network volumes)

The frontier's first launch spent about 20 of 22 minutes downloading 252 GB. Mike:
"Staging is imperative." RunPod network volumes cost $0.07/GB/month for the first TB
(docs.runpod.io, 2026-10-03), bill whether or not a pod runs, and tie pods to one
data center. Live on 2026-10-03, the data centers with network storage and RTX PRO 6000
were EUR-IS-1, EU-RO-1, US-CO-1, US-MO-2, US-NC-2 and CA-MTL-3; 4× was free only in
EUR-IS-1 and EU-RO-1. A "global volume" mentioned on one RunPod page is unconfirmed; the
design assumes the data-center lock.

→ **`offrig stage` is a CLI command, never an agent tool: a recurring charge is the
human's call, like the budget cap. Its compensator is `offrig stage --remove`.** The
download pod is guarded and terminated on every exit path (tested). Launches go
offline only behind a completion marker. Whether to create the frontier volume, and
where, is open: Mike chose to build it first (2026-10-03).

## Decisions

- 2026-10-02, Mike: the frontier tier serves with SGLang (TP=4, AWQ, fp8 KV). Small and medium stay on Ollama. Amended 2026-10-03 on the 3b evidence: KV stays full precision on sm_120, where fp8 KV was reported to corrupt output.
- 2026-10-02, Mike: register the side-car with Claude Code at user scope. Because it then starts in every project, the store opens on first use, never on start.
- One database per project, at `<project>/.offrig/offrig.db`, so memory travels with the code (the proposal; not overruled).
- 2026-10-03, Mike: phase 3 green-lit. Order: 3a runner, 3b SGLang frontier recipe (rehearsed on 1× RTX PRO 6000 first), 3c first real frontier queue.
- Open: whether Docker Sandboxes should isolate the agents that run handoffs (under evaluation).
