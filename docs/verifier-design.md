# offrig verifier: design

A cheap, independent check on work an agent has produced. You give a claim, and offrig
finds the evidence in the project and asks an open-weight model from another family
whether the evidence supports it. Two modes share one engine:

- **one-off** (`offrig verify`): one claim, one structured verdict, then the pod goes
  away;
- **session**: a running dialogue with the verifier for deeper research or a build
  review. The verifier can run its own searches of the project.

The project database gains search by meaning, so the verifier gets the right evidence,
not just the evidence that shares its words. A jury of several models is a later phase.

Status: design, 2026-10-08. Requested by the maintainer. Model selection is owned by the
R&D session (calibration on a gold set). The build and release are owned by the
Publisher. Grounded by a three-question study-swarm and a map of the existing code, both
run the same day.

## Standards compliance

Scored 0 to 3 against the studio workflow standards.

- **PIN_PER_STEP: 2.** Each verdict row records:
  - the verifier model and its digest;
  - the embedding model and its dimension;
  - a hash of the prompt template;
  - the ids of the retrieved evidence;
  - the plan id.

  A change of embedding model is detected from the stored model name and dimension,
  and it forces a re-index. Gap: nothing pins the pod image beyond its tag (same as the
  side-car).
- **ANDON_AUTHORITY: 2.** Each of these stops the step with a structured error:
  - a budget refusal;
  - a verdict that fails its schema twice;
  - an `evidence_quote` that isn't found verbatim in the supplied evidence;
  - an embedding-model mismatch.

  None of these is ever reported as a verdict.
- **NAMED_COMPENSATORS: 2.** The table below.
- **DECOMPOSE_BY_SECRETS: 2.** These change for their own reasons, so they are separate
  modules:
  - the index (chunking, embeddings, fusion);
  - the verdict contract (prompt, schema, parser);
  - the transport (the existing pod lifecycle).
  - Model choice lives in data (the calibration table and the profile), not in code.
- **UNCERTAINTY_GATED_HUMANS: 2.** A human approves the plan's worst case before any pod
  starts (the existing plan flow). Verdicts of `cannot_tell`, two runs that disagree, and
  claims marked high-stakes are reported for a human to decide (or, later, for a jury);
  they are never auto-resolved.
- **EXTERNAL_VERIFIER: 3, by construction.** This feature *is* a verifier:
  - a model family different from the generator's;
  - the generator's reasoning withheld (only the claim and the evidence are sent);
  - its error rate measured on a gold set before it becomes a default.

## Research grounding

Sources gathered by research agents on 2026-10-08. † means read from memory or a
secondary summary and not re-checked; treat its numbers as approximate.

**Which model, and how to choose it**
- Tang, Laban & Durrett 2024, MiniCheck (arXiv:2404.10774): a sub-1B checker matched
  GPT-4 at checking claims against supplied evidence, at a fraction of the cost. On the
  LLM-AggreFact leaderboard (llm-aggrefact.github.io), 7–8B specialised checkers sit
  within about 2 points of 70B general models. → **Grounded checks start small. Always
  send the evidence.**
- Tan et al. 2024, JudgeBench (arXiv:2410.12784): judging objective correctness of
  reasoning and code is hard; strong models scored near 57%, and small fine-tuned judges
  did worse. → **Reasoning checks (does this diff do what the PR says) are a separate
  check type, calibrated separately, starting at 27–32B.**
- Kambhampati et al. 2024 (arXiv:2402.01817)†; Huang et al. 2023 (arXiv:2310.01798)†:
  models don't reliably verify their own reasoning without outside grounding. →
  **Never the generator's family. Withhold the generator's reasoning.**
- Verga et al. 2024, PoLL (arXiv:2404.18796)†: a panel of three small models from
  different families beat one large judge. Against that, Kim et al. 2025
  (arXiv:2506.07962)† found correlated errors, especially within a family, and a 2026
  study (arXiv:2605.29800)† found nine judges behaved like about two independent votes,
  with the best single judge matching the panel. → **The jury is a later, measured
  phase. It is used only if it beats the best single model on the gold set.**
- Selection protocol (from the above). The gold set is JSONL, one claim per line, with
  balanced true and false claims including subtle near-misses. For each model it
  reports, per check type:
  - balanced accuracy;
  - the false-accept rate (a false claim passed) with a Wilson 95% CI;
  - the abstain rate and the cost per claim;
  - thinking on and off;
  - the evidence order swapped.

  The default is **the cheapest model whose false-accept upper bound is under 10%**,
  with at least 100 claims per check type.

**Retrieval**
- Thakur et al. 2021, BEIR (arXiv:2104.08663): BM25 is a strong baseline out of domain,
  and dense retrievers often lose to it. Cormack, Clarke & Büttcher 2009: reciprocal rank
  fusion (k = 60) beats either ranker with no tuning. → **Keep FTS5. Add dense
  retrieval beside it, fused with RRF.**
- Anthropic 2024, "Introducing Contextual Retrieval"†: a context header on each chunk,
  plus keyword search, cut top-20 retrieval failures by about half. → **Each chunk is
  indexed with a header (project, kind, source path, title). LLM-written context is
  added only if measured recall is poor.**
- Nussbaum et al. 2024, nomic-embed-text (arXiv:2402.01613): 137M parameters, 8k
  context, Apache-2.0, Matryoshka dimensions, and available in Ollama. → **The default
  embedding model. Small enough for the host CPU.**
- Aarsen & Shakir 2024 (Hugging Face)†: int8 vectors keep about 99% of retrieval quality
  at a quarter of the size. → **Vectors are stored as int8 BLOBs and scanned in
  process. No SQLite extension; one file.**
- Liu et al. 2023, "Lost in the Middle" (arXiv:2307.03172); Cuconasu et al. 2024
  (arXiv:2401.14887)†, which found near-miss distractors hurt more than random ones. →
  **5–8 passages, the strongest first and last, each with its id and source.**
- Wei et al. 2024, SAFE (arXiv:2403.18802); Yao et al. 2022, ReAct (arXiv:2210.03629):
  a checker that issues its own searches does better on claim checking. → **In
  session mode the verifier can call search, at most 3 queries per claim, each logged.
  One-off mode uses fixed top-k.**

**Pods**
- RunPod pricing (runpod.io/pricing, seen 2026-10-08)†:
  - A40 48 GB costs about $0.35/hr community and $0.49/hr secure. Serverless is 2–4×
    the pod rate per hour.
  - REST v1 retires on 2026-11-15, and GraphQL in early 2027 (docs.runpod.io, checked by
    R&D).
  - v2 pod create places one GPU type with no fallback, and has no spot field.

  → **The verifier is built on the v2 client (the Grok port), never on new v1 calls.
  Default card: an A40-class 48 GB. Secure for sessions, community allowed for one-offs.**
- No network volume (the maintainer's decision, 2026-10-08): the model is pulled at each
  launch, adding about 1–3 minutes of billed time. One-offs are therefore batched: a
  single launch checks every claim in a file before the pod goes away.
- Idle shutdown: a heartbeat on each request, an idle timeout (15 minutes by default),
  and the plan's hard deadline enforced by the existing watchdog. Ollama `keep_alive` is
  set to the session length.

## What already exists, and what is reused

Mapped from origin/main 06aa1e3:
- the plan, commit, watchdog and close lifecycle;
- lanes;
- `ensure_models`, which pulls an embedding model like any other model;
- the append-only records store with FTS5;
- `context::assemble`, which already takes any `&[Record]`;
- `runner::prepare`, the one place retrieval happens;
- `offrig_complete`'s pattern: journal before the side effect, commit and close the
  worst case, mark the output untrusted, write it under `.offrig/out/`.

Missing today:
- embeddings of any kind;
- a `/api/embed` wrapper;
- multi-turn chat (`offrig_ask` sends one user message);
- `format` and `think` options on chat;
- any verdict type.

## Design

### 1. Project index (schema v5)

- **`chunks`:**
  - `id`, `source` (a project-relative path or `record:<id>`), `kind` (doc, code, log,
    record), `title`, `ordinal`, `body`, `sha256`, `created_at`.
  - A chunk is 200–500 tokens on paragraph or function boundaries, carrying the header.
  - Re-indexing a source replaces its chunks only when its hash changed.
  - `chunks_fts` is FTS5 over the header and body.
- **`embeddings`:** `chunk_id`, `model`, `dim`, `vec` (int8 BLOB), `scale`.
  - `settings.embed_model` and `settings.embed_dim` name the index's model.
  - A mismatch refuses search until `offrig index --rebuild`.
- **Records:** active records are embedded when written, so memory search gets the same
  hybrid ranking.
- **Embedding:**
  - Done by the host's local Ollama with `num_gpu: 0`, so it runs on the CPU and never
    touches the local GPU (the maintainer's decision).
  - Batched, and incremental by hash.
  - Missing the model is a clear error with the `ollama pull` line, not a silent fall
    back to keywords.
- **Search:**
  - FTS5 BM25 top 50 and cosine top 50, fused by RRF (k = 60), keeping the top 20.
  - Then 5–8 go into the verifier's context.
  - `offrig_memory_search` gains `mode: keyword | hybrid` (hybrid when an index exists).
- **CLI:** `offrig index <paths…>` (gitignore-aware, size-capped, binaries and secrets
  files skipped) and `offrig index --status`.

### 2. Verdict contract

Each claim is JSONL: `{id, check_type: grounded|reasoning, claim, context?: [{source,
text}], evidence_paths?: [...], high_stakes?: bool}`.

The verifier is sent, in order:
1. its role card: the purpose, the criteria, and 3–5 worked examples of true, false and
   near-miss claims with their reasoning;
2. the claim;
3. the evidence: supplied context first, then retrieved passages, the strongest first and
   last;
4. the reply schema.

The model gets context, one claim per call, and thinking on with room (the studio rule:
set models up to succeed).

The reply is `{reasoning, verdict: supported|unsupported|cannot_tell, evidence_quote,
evidence_source}`, with reasoning first.

The checks on each reply:
- It is sent with a JSON-schema `format` where the model supports it, or as plain text
  with a parser where structured output collapses (measured per model in calibration).
- `evidence_quote` must appear verbatim in the evidence, or the verdict becomes
  `cannot_tell` with the reason `quote_not_found`.
- A schema failure is retried once, then reported as an error.
- Every verdict is marked untrusted model output and stored in the `verdicts` table with
  the pins listed under PIN_PER_STEP.

### 3. One-off: `offrig verify`

- **Input:** `offrig verify <claims.jsonl | --claim "..."> [--evidence path…]`, and the
  MCP tool `offrig_verify`.
- **Flow:**
  1. It plans with the `verify` profile and shows the worst case.
  2. It launches, or reuses this lane's live verify pod.
  3. It runs every claim in the file.
  4. It writes `.offrig/out/verify-<id>.jsonl` and a short summary.
  5. It shuts the pod down unless a session holds it.
- **The quick path for agents:** the CLI blocks; the MCP tool returns a job handle that
  `offrig_job` follows.

### 4. Session: `offrig_verify_session`

- **Actions:** `start` (plan and launch, idle timeout), `turn` (one message, multi-message
  chat with the stored transcript), and `end` (harvest the transcript, shut down).
- **Storage:** the transcript lives in the store, with turns as rows, so a restart or
  compaction loses nothing.
- **Searching:** the verifier can ask for a search with a `search` tool call. offrig runs
  hybrid search and returns passages, at most 3 per turn, and logs each query.
- **Status:** a session pod shows in `offrig_status`. The watchdog ends it at the idle
  timeout or the plan deadline, whichever comes first.

### 5. The `verify` profile

- One serve profile: A40 / L40S / A6000 48 GB in priority order, and secure cloud.
- `models` is empty until calibration names one. **No verify run is allowed before the
  default is set** (the maintainer's decision). The profile refuses with "no calibrated
  verifier model".
- The embedding model is not on the pod; it runs on the host.

### 6. Calibration (with R&D)

`offrig verify --calibrate gold.jsonl --models a,b` runs the gold set and writes the
table described under Research grounding. R&D owns the gold sets:
- grounded claims from `natural-errors`, tune half only, with g2 held out;
- reasoning claims from merged PRs (origin `prs-2026-10-08`).

The profile's default is changed only by a PR that cites a calibration table.

### 7. Jury (later)

`--jury a,b,c` runs claims sequentially per model on one GPU, batching all claims per
model before swapping. It is used only on `cannot_tell`, a disagreement, or
`high_stakes`. Pairwise error correlation is reported. The jury ships only if it beats
the best single model on the gold set.

## Phases (one PR each)

1. **The index (no RunPod):**
   - schema v5;
   - `Ollama::embed`;
   - chunking;
   - the int8 store;
   - RRF hybrid search;
   - `offrig index`;
   - hybrid `offrig_memory_search`;
   - the `runner::prepare` retrieval swap.

   Can start now.
2. **The verdict contract:** the role card, the schema, the parser, the quote check, the
   `verdicts` table, and `chat` with `format`, `think` and messages. Tested against the
   mock pod model server. Can start now.
3. **One-off and session verbs** on the v2 client. **After Grok's v2 port phase 1
   merges.**
4. **Calibration runner.** It runs once R&D's gold sets exist, and the default model is
   set by a PR that cites the table.
5. **Jury.** Later, on the measured trigger.

## Compensators

| Action | Undo | Owner |
|---|---|---|
| Schema v5 migration | Forward-only, like v2–v4. A v4 binary refuses a v5 DB. Restore from `offrig.db` backup taken before migrate | Publisher |
| Index build (local writes) | `offrig index --rebuild` or drop `chunks` and `embeddings` | the operator |
| Pod launch for verify | `offrig_shutdown`; the watchdog terminates at the deadline regardless | the calling agent, then the watchdog |
| Verify spend (committed worst case) | Released on close; the actual spend is recorded and never edited | offrig |
| Default-model change | Revert the PR that set it | Publisher, citing R&D's table |

## Decisions (2026-10-08, the maintainer)

- Embeddings are computed on the host CPU (Ollama, `num_gpu: 0`), never on the local GPU.
- No network volume. Models are pulled at each launch, and one-offs are batched.
- No verifier runs until calibration names the default model.
- Paid API seats (OpenRouter) are not a verifier lane without the maintainer's go.
- No Ollama Cloud models, ever.
