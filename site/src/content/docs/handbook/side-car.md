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
offrig budget 15          set the cap to $15
offrig budget             show cap, committed, spent and remaining
```

No tool can change the cap. An agent reads it; only a person sets it.

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
| `offrig_memory_search` | no | Search project memory |
| `offrig_memory_record` | no | Add to project memory |
| `offrig_put` | pod time | Copy files to a job pod |
| `offrig_exec` | pod time | Start, follow, stop or briefly run a command on a job pod |
| `offrig_get` | pod time | Copy files back from a job pod |
| `offrig_shutdown` | stops billing | Terminate the pod and close the plan's books |

Every error a tool returns is a structured result that names a next action. Error codes
are listed in the [Reference](../reference/).
