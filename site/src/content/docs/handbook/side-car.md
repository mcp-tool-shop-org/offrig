---
title: The side-car
description: How an agent plans, launches, uses and shuts down GPUs through offrig-mcp, inside a human-set budget.
sidebar:
  order: 3
---

`offrig-mcp` is an MCP server. An agent such as Claude Code calls it as an instrument: to
price a session, rent a GPU, run work on it, and shut it down. Each project keeps its own
database at `<project>/.offrig/offrig.db`, which outlives every pod, so a session survives
compaction or a restart without re-explaining anything.

## Set it up

Register it with Claude Code once, at user scope:

```text
claude mcp add --scope user offrig -- <path>\offrig-mcp.exe
```

It opens a project's store only on first use, so it is harmless in projects that never use
it. Then a human sets the project's cap, in the project directory:

```text
offrig budget --provider runpod 30       RunPod's cap: pods (and later serverless)
offrig budget --provider openrouter 8    OpenRouter's cap: completions
offrig budget 15                         an overall ceiling across both (optional)
offrig budget --clear-overall            drop the overall ceiling once both caps are set
offrig budget                            in a terminal: a menu for the overall cap
offrig budget --show                     every cap, with each provider's own balance
offrig budget --show --json              the same, as one versioned JSON object
```

**Each provider has its own cap.** RunPod and OpenRouter hold separate money, so a pod is
checked against the RunPod cap and an OpenRouter completion against the OpenRouter cap.
When an overall ceiling is set, a spend must fit under that too. `--show` prints each
provider's cap, committed, spent and remaining next to the balance the provider itself
reports. It warns when a cap is above what the account holds, because the account would
run dry first. A provider cap above the overall ceiling never takes effect: the ceiling
refuses first. Local Ollama costs nothing and has no cap, and Ollama Cloud is refused.

**Caps are per project; the money is per account.** Several projects can each hold a
RunPod cap, and together they can promise more than the RunPod account holds. So
`--show` also prints an `account` line per provider: the caps across every project offrig
knows (each project where a cap was set, and each project with a side-car lane), what they
have committed, what is still unspent, and the balance. It warns when the projects together
promise more than the account holds. Before a pod or an OpenRouter completion is committed,
offrig also checks the account: this amount plus everything already committed, in this
project and the others, must fit in the provider's live balance. That matters most on
RunPod, which stops every pod on the account when the balance reaches zero. When the
balance can't be read (no key, the provider unreachable), the commit goes ahead under the
project's own caps, with a note saying the account check was skipped. That is deliberate: a
provider outage shouldn't stop work that the project's caps already allow. A project whose
records can't be read, on the other hand, stops the check: its commitments can't be
counted, so nothing new is committed until it can be.

A provider with no cap of its own and no overall ceiling shows `(not set)`: nothing can be
spent there until a person sets it.

These checks cover what an agent spends through the side-car (`offrig_launch`,
`offrig_complete`). **A pod started by hand with `offrig up` or the app is not counted**
against any project's cap or in the account totals. `offrig budget` says so in its last
line, the JSON lists it under `uncounted`, and `offrig up` says so before it launches. Its
own guard still applies: it refuses when the runway with the pod running would be under one
hour.

A project set up with one cap keeps working as before: that cap stays as the overall
ceiling, and each provider's cap starts at the same amount. Set the real per-provider caps,
then clear the overall ceiling if you don't want one.

No tool can change a cap. An agent reads it; only a person sets it. Run without an
amount in a terminal, `offrig budget` shows the budget and any running plans, then offers:

1. **Set a new cap.** Asked for, then confirmed.
2. **Stop new spending.** Lowers the cap to what's already spent plus what's committed, so
   nothing is left for a new plan.

**Money given to a run stays with it.** The cap can never go below spent plus committed, by
the menu or by `offrig budget <usd>`. Stopping new spending leaves a running training job
alone: it keeps its allocation and goes on to its deadline. To stop a pod now, use
`offrig down` or the session's `offrig_shutdown`. Where stdout isn't a terminal (a script,
an agent's shell), `offrig budget` prints the one-line budget as before.

## The loop an agent follows

1. **`offrig_status`.** The project, budget, RunPod balance and runway, open plans, pods,
   and the handoff queue. It also names other projects' offrig pods, with each one's plan,
   note and deadline (see [Lanes](../lanes-and-jobs/#seeing-other-lanes-pods)). Call it
   first in any session.
2. **`offrig_plan`.** Prices a session at its worst case: live price × max hours. It is
   refused if the worst case is over what's left of the cap. Planning costs nothing.
3. **`offrig_launch`.** Takes only a `plan_id`, so an agent cannot name its own price. It
   commits the worst case, waits for GPUs (renting nothing while it waits), boots the pod,
   and starts the watchdog. Calling it twice with the same plan returns the same job.
4. **`offrig_job`.** Launch progress, the GPU type and price rented (as soon as the pod
   exists), the card count, and the host CUDA version once ssh is up, minutes left, and
   spend so far. Poll it with the `plan_id`, or with the `job_id` from `offrig_launch`;
   the two are different numbers. If the launch fails, `offrig_job` gives the failure's
   `code`, whether it is `retryable`, and whether a pod was rented and billed (see below).
   A recipe is `ready` only when one completion returned a choice. `engine_hung` and
   `engine_unresponsive` leave the pod up.
5. **The work.** On a model pod: `offrig_ask` or `offrig_run`. On a job pod: `offrig_put`,
   `offrig_exec`, then `offrig_get`.
6. **`offrig_shutdown`.** Terminates the pod and closes the plan's books with the measured
   spend. Do it as soon as the work is done, and copy results back first: the pod's disk
   goes with it.

A plan can be narrowed before it spends:

```text
offrig_plan profile=job max_hours=3.5
            no_fallback=true      only the profile's first GPU family
            max_price_hr=2.2      nothing dearer than $2.20/hr in total
            wait_minutes=20       retry quietly if none is free
            container_disk_gb=150 a bigger local disk for downloads
```

The CUDA pin is on the profile, not on the plan call. A floor is `min_cuda`. An exact
list is `cuda_versions`. The plan reply names whichever one the profile set. See
[Configuration](../configuration/) for what each limit does.

Once the pod record has a count, `offrig_status` and `offrig_job` include it:

```json
{
  "gpu_count": 1,
  "gpus": "1x NVIDIA RTX PRO 6000 Blackwell Server Edition",
  "planned_gpu_count": 1,
  "measured_gpu_count": 1,
  "gpu_mismatch": false
}
```

`gpu_mismatch` is true when the record or the `nvidia-smi` reading differs from the
plan. `gpu_mismatch_source` is `record`, `measured`, or `both`. Nothing is shut down
for it. Before a measurement, a record that omits the count leaves `gpu_mismatch` null.
A reading that lists no cards is `measured_gpu_count` `0`.

A recipe that cannot answer looks like this on `offrig_job`. The job `state` stays
`running`. The step lives under `progress`. The `error` is the completion's own
message. One real case is a 200 whose body had no choice:

```json
{
  "state": "running",
  "progress": {
    "step": "engine_unresponsive",
    "error": "ollama: completion smoke: the reply carried no choice",
    "engine_log_tail": "the last lines of the engine log",
    "next_action": "the engine is not answering completions: shut the pod down, or wait if it is still warming up"
  },
  "next_action": "the engine is not answering completions: shut the pod down, or wait if it is still warming up"
}
```

A hang, after `grep -c` has seen `Application startup complete` and `/health` has stayed
down for 3 minutes. `log_tail` here is the last 40 lines, as a list:

```json
{
  "state": "running",
  "progress": {
    "step": "engine_hung",
    "log_tail": ["the last 40 lines, one string each"],
    "since_startup_secs": 180,
    "next_action": "shut the pod down: the engine finished startup but never became healthy"
  },
  "next_action": "shut the pod down: the engine finished startup but never became healthy"
}
```

The 40-line tail is the report. The clock is the count over the whole log, taken on
the same ssh read, so the startup line can already have scrolled out of the tail.

## The watchdog

Every side-car launch starts a **watchdog**: a separate process that terminates the pod at the
plan's deadline (the time the plan was committed, plus its max hours), even if the agent,
the session and the side-car are all gone.

- It never acts on a failed lookup.
- It terminates exactly once, then closes the books.
- It logs to `.offrig/watchdog-<plan>.log`.

If getting a rented pod ready fails before the engine is serving, the side-car launch terminates
the pod itself instead of leaving it billing. `engine_hung` and `engine_unresponsive`
are not those failures. The side-car returns them as a normal result, so nothing
deletes the pod, the plan stays committed, and the job stays `running`. You shut it
down, or the plan's deadline does. An engine process that has already exited is a real
failure, and that pod is deleted. A pod started with `offrig up` or the app has no
watchdog. When the command stops with an error (a hang, an unresponsive engine, an
engine that exited, or a timeout), the pod keeps billing until `offrig down <profile>
--yes` or the app's Shut down. A pod RunPod reports as `ERROR` or `EXITED` is the
exception: that one is terminated on every surface. The two ways a side-car launch can
fail before that cost different amounts, and `offrig_job` says which happened:

| Code | What happened | Cost |
|---|---|---|
| `no_capacity` | No GPU of the plan's types came free within the wait | nothing: no pod was rented (`pod_rented: false`) |
| `pod_not_ready` | A pod was rented but never became ready (no ssh endpoint, or the image never finished) | the minutes it billed, shown in `failure.spent`; the pod is terminated and the plan closed |

Count a `pod_not_ready` as a paid attempt; a `no_capacity` can simply be retried. A capacity wait counts against the plan's hours, so leave room for it
in `max_hours`.

## Handoffs and the runner

A **handoff** is a unit of work for the pod's model, headed by a role and carrying an
acceptance check. Roles come from Role OS (dossiers and starter-pack cards), plus four game
roles shipped with offrig: game-designer, systems-designer, narrative-designer and
lore-keeper.

- `offrig_handoffs` queues, lists and previews handoffs, shows a handoff's best output
  (also written to `.offrig/out/`), and records outcomes: complete, invalid, violation,
  fail, or retry with feedback.
- `offrig_ask` runs one turn of a handoff on the pod's model. Its reply is untrusted
  output.
- `offrig_run` starts a detached **runner**. It keeps every model slot busy, drafts each
  ready handoff, revises at most twice against checks that failed, feeds results to
  dependent handoffs, and shuts the pod down when the queue is dry (unless `keep_pod`).
  Work that code cannot check waits in review.

Deterministic checks verify structure, not design quality. Use `accept_on_checks` for
structural work, and send design work to review.

## Project memory

`offrig_memory_record` adds a brief, constraint, decision, fact or checkpoint. A change is
a supersession with a reason, never an overwrite. `offrig_memory_search` searches what's
active, and each result carries its source and date. The context for every handoff turn is
built from this store.

## Project index

`offrig index <paths...>` cuts documents, code and logs into chunks (about 800 to 2000
characters, on paragraph or declaration boundaries, each opening with a
`[source · kind · title]` line) and stores them in the project database next to the memory
records. Active records are chunks too, so memory search ranks them the same way.

- Re-running it re-embeds only files whose hash changed. `.gitignore` is respected;
  binaries, files over 1 MB, and secrets-like files (`.env*`, `*.pem`, `*.key`, `id_*`,
  `*credentials*`) are skipped and listed.
- `offrig index --status` shows the counts, the embedding model and dimension, and the last
  run. `offrig index --rebuild` drops every vector and embeds all chunks again; it is how
  you change the model (`--model`).
- `offrig_memory_search` takes `mode`: `keyword` (words must match) or `hybrid` (keyword
  BM25 top 50 and cosine top 50, fused by reciprocal rank fusion, top 20 kept). With no
  `mode` it is hybrid once the project has an index and keyword before, and the reply says
  which ran. Asking for `hybrid` with no index, or with the embed server down, is an error;
  it never turns into a keyword search. Handoff prompts use the same search, and fall back
  to keywords if the embed server is unreachable, so a turn is not lost.

### The embed server

Embeddings come from **EmbeddingGemma 2** by Google DeepMind (`google/embeddinggemma-2`,
revision `914f7f8`). It's an open model under the Apache-2.0 licence, with no gate to pass
before you download it. offrig runs it text only, in float32, on the CPU. On a pre-registered
benchmark of the studio's own queries it beat nomic-embed-text, offrig's embedder before
1.2.0: dense nDCG@10 +0.112, code +0.174, prose +0.041. The design and the per-query numbers
are in the rnd library, `experiments/embeddinggemma-2/`.

It's served by `tools/embed-serve`, a small server in this repository that answers the part
of Ollama's API that offrig's embed client uses. Ollama on Windows can't run the model,
because its library builds need MLX. The server runs on its own port, never the shared
Ollama on `11434` and never a GPU. Loading an embedding model into the shared server can
evict a GPU model mid-run. Start it once, from an offrig checkout, with Python 3.12:

```bash
cd tools/embed-serve
python -m venv .venv
.venv/Scripts/python -m pip install -r requirements.txt    # Linux: .venv/bin/python
.venv/Scripts/python embed_serve.py --port 11490 --sub-batch 8
```

`requirements.txt` pins every package exactly, with torch's CPU build from the PyTorch CPU
index. The first start downloads the model, about 1.5 GB, into the Hugging Face cache.
`--threads N` caps torch's CPU threads, so a long rebuild leaves cores to other work. By
default it uses every core.

The server guards itself:
- It refuses to start when torch can see CUDA.
- It serves one model name, `embeddinggemma-2:f32`, and answers 404 for any other.
- It binds `127.0.0.1` only.

offrig checks too. After each embedding call it reads the server's `/api/ps`, and if a model
sits in GPU memory it refuses and shows the start line.

offrig sends documents with `title: none | text: ` and queries with
`task: search result | query: `, the prefixes from the model's card. nomic-embed-text, if you
choose it with `--model`, gets its `search_document: ` and `search_query: `. Other models get
none.

The URL is `embed_url` in `config.toml` (default `http://127.0.0.1:11490`), and
`OFFRIG_EMBED_URL` overrides it. offrig refuses a port that belongs to something else it
runs:
- `11434`;
- the configured tunnel port and its runner;
- the plain lane;
- the lane range;
- the side-car range.

If nothing answers, the error shows the start line above. If the index was built with a
different model or dimension, every search and index call stops and tells you to run
`offrig index --rebuild`.

**Measured cost** (2026-10-15, a 24-thread desktop CPU):
- **Speed:** about 2 chunks a second over HTTP, so about 8 minutes per 1,000 chunks.
- **Memory:** about 1.7 GiB resident once the model is loaded. Long chunks briefly need
  more, which the server bounds by encoding in sub-batches of 8.

#### Upgrading from nomic-embed-text (offrig 1.1.x)

A project index built with nomic-embed-text keeps its vectors, and offrig never mixes them
with the new model's:
- **`offrig index`** refuses: "the project index was built with nomic-embed-text, not
  embeddinggemma-2:f32; run `offrig index --rebuild`". It writes nothing.
- **Hybrid search**, the default once a project has an index, embeds the query with the
  index's own model. embed-serve doesn't serve that model, so the search is refused, never
  quietly downgraded. Search with `mode=keyword` meanwhile.
- **The side-car's background embed** leaves new records unembedded until the rebuild.
  They're still found by keywords.

Re-embed each such project once, with the server running:

```bash
offrig index --rebuild --project <dir>
```

It runs on the CPU at the rate above. Chunks are kept, and only the vectors are replaced. A
project with no vectors yet needs nothing: its first `offrig index` embeds with the new model.

#### Rollback

1. Reinstall the previous offrig.
2. Restart the nomic Ollama on 11490.
3. Any store already rebuilt with the new model then needs
   `offrig index --rebuild --model nomic-embed-text`.

Chunks are kept, and only vectors are replaced.

The CPU-only Ollama line for nomic-embed-text is:

```bash
OLLAMA_HOST=127.0.0.1:11490 CUDA_VISIBLE_DEVICES=-1 GGML_VK_VISIBLE_DEVICES=-1 OLLAMA_VULKAN=0 OLLAMA_IGPU_ENABLE=0 ollama serve
OLLAMA_HOST=127.0.0.1:11490 ollama pull nomic-embed-text
```

## All tools

| Tool | Spends | What it does |
|---|---|---|
| `offrig_status` | no | Project, budget, balance and runway, open plans with lane and pod name, pods (other projects' pods with their plan, note and deadline), handoff queue |
| `offrig_offers` | no | Live GPU offers for a GPU count |
| `offrig_plan` | no | Prices a session at its worst case and records a plan |
| `offrig_launch` | **yes** | Commits the plan, rents the pod, starts the watchdog |
| `offrig_job` | no | Launch progress, what was rented, minutes left, spend so far, and a failed launch's code and cost |
| `offrig_ask` | pod time | One handoff turn on the pod's model |
| `offrig_run` | pod time | A detached runner that works the queue |
| `offrig_handoffs` | no | Queue, list, preview, show and record handoffs |
| `offrig_memory_search` | no | Search project memory (`mode`: keyword or hybrid) |
| `offrig_memory_record` | no | Add to project memory |
| `offrig_put` | pod time | Copy files to a job pod |
| `offrig_exec` | pod time | Start, follow, stop or briefly run a command on a job pod |
| `offrig_get` | pod time | Copy files back from a job pod |
| `offrig_shutdown` | stops billing | Terminate the pod and close the plan's books |
| `offrig_complete` | **yes** | One OpenRouter chat completion for an allow-listed project and model. Commits the worst case, writes the answer to a project file, records the real charge |

Every error a tool returns is a structured result that names a next action. Error codes
are listed in the [Reference](../reference/).
