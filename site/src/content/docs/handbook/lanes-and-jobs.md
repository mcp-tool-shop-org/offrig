---
title: Lanes and job pods
description: How several projects share one RunPod account without colliding, and how job pods run your own GPU work.
sidebar:
  order: 5
---

## Lanes

Several projects can run side-cars at once on one RunPod account. Each project gets its
own **lane**, and no other project shares its SSH alias, tunnel port, side-car port or pod
names.

| | Plain lane (CLI, app, Zed) | A project's lane |
|---|---|---|
| SSH alias | `offrig` | `offrig-<tag>` |
| Tunnel port | `11435` (runner `11436`) | the first free of `11500`, `11502`, ... (runner: the port above) |
| Side-car port (shell driver) | none | `11700` + the lane's slot |
| Pod name | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| SSH config block | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` comes from the project folder's name, for example `aspire-si` or `ai-jam-sessions`.
offrig adds a short hash when two projects share a folder name.

A lane is allocated the first time the project plans a session, and kept in `lanes.toml`
in offrig's config directory. The same project gets the same lane after every restart.
Allocation takes a lock and writes the registry atomically, so two side-cars starting
together never share anything. No lane can use `11434`, your local Ollama's port: the range
starts at `11500`, and a registry edited to say otherwise is refused.

### What a side-car will and won't touch

A side-car only ever lists, matches or stops pods named for its own lane. Everything else
is left alone:

- another project's pods;
- the plain lane's `offrig-<profile>` pods;
- any other pod on the account.

The tunnel's orphan cleanup kills a stale `ssh` only when its forward and alias are the
lane's own.

### Seeing other lanes' pods

Every project can see what the others are running, without asking. `offrig_status` lists
each other offrig pod with:

- its lane and project;
- the pod's name, GPU, price per hour and status;
- the open plan behind it: plan id, profile, note, deadline (UTC) and committed worst case.

offrig reads the other project's store strictly read-only: it never writes to it, never
migrates it, and leaves no files behind. If the store can't be read, the pod still shows,
with a note saying why. Pods offrig did not create are only counted and named. `offrig
status` on the command line prints the same plan line under each lane pod.

Pods also carry their identity as environment variables, visible in RunPod's console:
`OFFRIG_LANE` (the lane tag, or `plain`), and on side-car launches `OFFRIG_PLAN` and
`OFFRIG_DEADLINE`. Plan notes are never put on the pod.

### One lane, one live pod

A lane has one SSH alias, so it serves one pod at a time. `offrig_launch` refuses while
the lane has an open plan or a live pod, before anything is committed or rented:

```text
lane aspire-si has a live pod offrig-aspire-si-job (plan 12); shut it down first
```

Shut the first plan down, then launch the next.

### The side-car's own port

`offrig-mcp` speaks MCP over stdio. A shell driver can hold one open behind a loopback
HTTP port, for sessions whose own MCP connection is stale. Each lane has its own side-car
port, so two projects' drivers never collide:

```text
offrig-mcp --sidecar-port --project <dir>           print the port
offrig-mcp --sidecar-port --check --project <dir>   also exit 1 if something holds it
```

With `--check`, a taken port is an error that names the port and, if an offrig side-car is
listening there, the project it serves. `OFFRIG_SIDECAR_PORT` still overrides the default.
offrig refuses a value that isn't a port, is below 1024, or is one of its own reserved
ports.

## Job pods

A **job pod** rents a GPU for work that runs on it, such as a training run or a render,
instead of serving a model. The `job` and `jam` profiles are job profiles.

The pod runs a pinned PyTorch image with sshd and nothing else. There is no tunnel and no
Zed wiring, and sshd allows no forwarding: the only way in is ssh.

### The workflow

```text
offrig_plan     profile=job max_hours=3.5 no_fallback=true max_price_hr=2.2
offrig_launch   plan_id=<id>                ready when sshd answers
offrig_exec     action=run command="nvidia-smi"     a quick check, 30 s default
offrig_put      local=./data  pod=data             relative pod paths are under /workspace/job
offrig_exec     action=start name=train command="bash run.sh"
offrig_exec     action=status name=train           running or exited, with a log tail
offrig_get      pod=results   local=./results    local parent folders are created
offrig_shutdown plan_id=<id>
```

- **`start` runs detached** in `/workspace/job`, so the job outlives the side-car and the
  ssh session. The command is sent as base64, so the ssh shell never reads it. Its log and
  exit status are kept in `/workspace/offrig/jobs/`.
- **`status` collapses progress bars** to their last frame. `save_log=<local path>` also
  copies the whole log, exactly as written.
- **`run` is for quick checks, not work.** It runs to completion under `timeout` (30 s by
  default, at most 120 s) and returns stdout, stderr, the exit code and whether it timed
  out. Each stream is cut to its last 64 KB. Anything longer is a `start`.
- **`stop` kills the job** and everything it started.

### Which plan a job tool acts on

`offrig_put`, `offrig_exec` and `offrig_get` take an optional `plan_id`. With one open job
plan they use it. With more than one and no `plan_id`, they refuse and list the open plans,
because they never guess. With a `plan_id`, they act only on that plan's own pod, after
checking the pod's name. Every reply states the project, lane and plan it acted on.

### Hosts

The image is a CUDA 12.8 build, so a job profile never lands on an older driver. The `job`
profile asks for CUDA 13.0 hosts, because the jobs it runs install a current vLLM, whose
PyTorch is a CUDA 13 build.

Even then, a host can be slow. Check write and download speed in your setup before
fetching large models, and use a bigger [container disk](../configuration/#container-disk)
when `/workspace` is a slow network filesystem.
