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

## Tool surface (10 tools, fixed order)

| Tool | Kind | What it does |
|---|---|---|
| `offrig_status` | read | Balance, runway, pods, budget left, active plan and job, guard summary. Liveness is computed, never stored. |
| `offrig_offers` | read | Live GPU offers for a count, filtered to usable cards. |
| `offrig_plan` | write, no spend | Makes a plan from a profile, maximum hours and budget cap. Returns `plan_id`, worst-case cost and runway after. |
| `offrig_launch` | **spends** | Takes `plan_id` only. Starts a background job (wait for GPUs, boot, pull, tunnel, guard). Returns `job_id`. Idempotent per plan. |
| `offrig_job` | read | Job state, progress, pull percent and `next_action`. |
| `offrig_memory_search` | read | FTS over active records, filterable by kind, task and tag. Each result carries its source and date. |
| `offrig_memory_record` | write | Adds a fact, decision, constraint or checkpoint, with provenance. `supersedes` is required when it contradicts a record. |
| `offrig_handoffs` | write | Adds handoffs (role, mission, acceptance check, file scope, dependencies) to the queue, or lists them with state. |
| `offrig_ask` | read-ish | Sends one role-headed handoff turn to the pod model, with context assembled from memory, and returns the reply. |
| `offrig_shutdown` | **destroys** | Terminates the pod. Refuses while work is unharvested unless given a reason, and records the reason. |

The full autonomous runner on the pod (multi-turn tool loop, branch per handoff, test
runs) is phase 3, behind the same queue.

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
2. **Launch and jobs.** Done 2026-10-02, tested against a mock RunPod; the live
   rehearsal on the small tier waits for funds. `offrig_launch` (plan id only,
   idempotent, preflight before any commit, a failed post-rent setup terminates the pod),
   `offrig_job`, `offrig_ask` (context from the store, untrusted reply) and
   `offrig_shutdown` (refused with handoffs in flight unless given a reason). The
   watchdog is a detached process (`offrig-mcp --watchdog <plan>`) that leaves the host's
   job object where Windows allows, so closing the session does not kill it. Schema v2
   adds the plan clock and jobs, with a tested migration from v1.
3. **Pod runner and engine recipe.** SGLang frontier recipe (TP=4, AWQ, fp8 KV),
   multi-turn runner on the pod, branch per handoff, acceptance checks run on the pod,
   and harvest to git. A paid frontier run once Mike funds it.

## Decisions

- 2026-10-02, Mike: the frontier tier serves with SGLang (TP=4, AWQ, fp8 KV). Small and medium stay on Ollama.
- 2026-10-02, Mike: register the side-car with Claude Code at user scope. Because it then starts in every project, the store opens on first use, never on start.
- One database per project, at `<project>/.offrig/offrig.db`, so memory travels with the code (the proposal; not overruled).
- Open: whether Docker Sandboxes should isolate the agents that run handoffs (under evaluation).
