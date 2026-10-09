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
```

**Each provider has its own cap.** RunPod and OpenRouter hold separate money, so a pod is
checked against the RunPod cap and an OpenRouter completion against the OpenRouter cap.
When an overall ceiling is set, a spend must fit under that too. `--show` prints each
provider's cap, committed, spent and remaining next to the balance the provider itself
reports. It warns when a cap is above what the account holds, because the account would
run dry first. A provider cap above the overall ceiling never takes effect: the ceiling
refuses first. Local Ollama costs nothing and has no cap, and Ollama Cloud is refused.

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
   exists) and the host CUDA version once ssh is up, minutes left, and spend so far. Poll it
   with the `plan_id`, or with the `job_id` from `offrig_launch`; the two are different
   numbers. If the launch fails, `offrig_job` gives the failure's `code`, whether it is
   `retryable`, and whether a pod was rented and billed (see below).
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

See [Configuration](../configuration/) for what each limit does.

## The watchdog

Every launch starts a **watchdog**: a separate process that terminates the pod at the
plan's deadline (the time the plan was committed, plus its max hours), even if the agent,
the session and the side-car are all gone.

- It never acts on a failed lookup.
- It terminates exactly once, then closes the books.
- It logs to `.offrig/watchdog-<plan>.log`.

If getting a rented pod ready fails, the launch terminates the pod itself instead of
leaving it billing. The two ways a launch can fail cost different amounts, and
`offrig_job` says which happened:

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

Embeddings come from a dedicated, CPU-only Ollama, never the shared one on `11434` and never
a GPU: loading an embedding model into the shared server can evict a GPU model mid-run, and
a zero-GPU request can still open a CUDA context. Start it once, on its own port:

```bash
OLLAMA_HOST=127.0.0.1:11490 CUDA_VISIBLE_DEVICES=-1 ollama serve
OLLAMA_HOST=127.0.0.1:11490 ollama pull nomic-embed-text
```

In PowerShell, set the variables first, then run the same two commands, each in its own
window or job:

```powershell
$env:OLLAMA_HOST = '127.0.0.1:11490'; $env:CUDA_VISIBLE_DEVICES = '-1'
ollama serve
```

Check it with `nvidia-smi`: no process from this server should appear.

offrig sends documents and queries to nomic-embed-text with the `search_document: ` and
`search_query: ` prefixes the model was trained with; other models get none.

The URL is `embed_url` in `config.toml` (default `http://127.0.0.1:11490`), and
`OFFRIG_EMBED_URL` overrides it. offrig refuses a port that belongs to something else it
runs: `11434`, the configured tunnel port and its runner, the plain lane, the lane range
and the side-car range. Requests also carry `num_gpu: 0` as a second guard. If nothing
answers, the error shows the lines above; if the model is missing, it says
`ollama pull nomic-embed-text`. If the index was built with a different model or dimension,
every search and index call stops and tells you to run `offrig index --rebuild`.

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

Every error a tool returns is a structured result that names a next action. Error codes
are listed in the [Reference](../reference/).
