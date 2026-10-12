---
title: Configuration
description: Tiers and profiles, engine recipes, capacity waits, hardware limits, container disks and staged weights.
sidebar:
  order: 4
---

offrig's settings live in `%APPDATA%\offrig\config.toml`. The file is written the first
time something changes; `offrig init` writes the defaults and prints the path. Delete the
file to return to the defaults.

## Environment

The project directory is `--project`, otherwise `OFFRIG_PROJECT`, otherwise the current
directory.

Users set `RUNPOD_API_KEY`. Completions read `OPENROUTER_API_KEY`. `OFFRIG_API_KEY`,
`OFFRIG_SIDECAR_PORT` and `OFFRIG_EMBED_URL` are optional overrides, described where each
one is used.

`OFFRIG_TEST_*` exist only in debug builds for the test suite. They are compiled out of
release binaries, including any switch that would skip the watchdog.

## Tiers

A **profile** is a tier: which GPUs to rent, in priority order, and what runs on them.

| Profile | GPUs | Runs | Typical cost |
|---|---|---|---|
| `small` | 1 × RTX 2000 Ada / A4000 class | `qwen3:4b` on Ollama | about $0.25/hr |
| `medium` | 1 × RTX PRO 6000 (96 GB); A100 or H100 80 GB if none is free | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` on Ollama | $2.09/hr (A100 $1.59) |
| `frontier` | 4 × RTX PRO 6000 (384 GB) | Qwen3-Coder-480B AWQ 4-bit (252 GB) on SGLang | $8.36/hr |
| `frontier-mini` | 1 × RTX PRO 6000 | Qwen3-Coder-30B FP8 on SGLang: the frontier path, rehearsed cheaply | about $1.7/hr |
| `frontier-mini-awq` | 1 × RTX PRO 6000 | Qwen3-Coder-30B AWQ on SGLang: the frontier's 4-bit kernels, rehearsed cheaply | about $1.7/hr |
| `job` | 1 × RTX PRO 6000 (96 GB); A100 or H100 80 GB if none is free | no model server: your own work (see [job pods](../lanes-and-jobs/#job-pods)) | $2.09/hr |
| `jam` | 1 × A40 (48 GB) first; A6000, A5000, 3090, L4 or 4090 if none is free | no model server: singing renders for ai-jam-sessions | $0.49/hr |

Prices are the v2 catalog's secure-cloud list prices, read live: per GPU, times the GPU
count. A type at NONE is not free. RunPod's pricing page is not the available price.
`offrig gpus` shows what is free now.

## Engine recipes

A profile with a `recipe` runs SGLang instead of Ollama. A recipe sets:

- a pinned image (`lmsysorg/sglang:v0.5.20-cu130`);
- a Hugging Face model the pod downloads at start;
- extra server arguments.

offrig sets tensor parallelism from the GPU count and the context length from the profile.
It keeps the engine on the pod's loopback, and a recipe cannot override any of those.

For a gated model repo, `hf_token_secret` names a RunPod secret. offrig references it as
`{{ RUNPOD_SECRET_<name> }}`, so the token never enters the pod spec.

The launch reports weights on disk while the engine downloads them. It stops at once,
showing the engine's log, if the engine process exits.

`/health` returning 200 is not readiness, and neither is `/v1/models`. After `/health`
returns 200, offrig sends one chat completion (`max_tokens` 1, `temperature` 0) and
waits up to 30 seconds. The step is `ready` only when that reply has a choice. If the
completion fails or times out, offrig tries once more after 15 seconds. A second
failure is the step `engine_unresponsive`. The job stays `running`, the plan stays
committed, and the error and the engine log are on the job. The next action is exactly:

```text
the engine is not answering completions: shut the pod down, or wait if it is still warming up
```

That report does not shut the pod down, on the side-car, the CLI, or the app.

The hang clock starts from `grep -c` of `Application startup complete` on the same ssh
read as the log tail, over the whole engine log. The tail the job shows is the last 40
lines. A chatty engine can push the startup line out of those 40. The count still
starts the clock. If `/health` then stays down for 3 minutes, the step is `engine_hung`.
The job includes those 40 lines, the seconds since the count was first seen, and the
next action:

```text
shut the pod down: the engine finished startup but never became healthy
```

offrig reports that hang and leaves the pod up. A GPU-count warning is not prefixed
onto either of those next actions, so the sentence stays exact.

An engine process that has already exited is not either of those reports. On the
side-car that is a failed launch, and the compensator deletes the pod. On the CLI and
the app the command stops with the error and does not delete the pod.

## Waiting for capacity

Each profile lists GPU types in priority order, and RunPod takes the first one with
capacity. When none is free, a launch can wait. offrig checks every minute and creates the
pod the moment GPUs free up. Nothing is rented while it waits.

| Setting | Where | Default |
|---|---|---|
| `wait_for_gpu_minutes` | profile | `frontier` 120, `job` 20 |
| `--wait <minutes>` | `offrig up` | the profile's |
| `wait_minutes` | `offrig_plan` | the profile's; `0` fails at once |

A side-car plan's wait is cut to the time the plan has left, minus a five-minute reserve,
so it never runs past the deadline. Because nothing is rented, waiting adds nothing to the
worst case. When the wait runs out, the launch fails with `no capacity`, and nothing was
rented.

## Pinning a plan's hardware

Without limits, a plan can land on a fallback card with less memory, an older driver and a
different price. Five limits keep a plan to hardware its work can use:

| Limit | Where | Effect |
|---|---|---|
| `min_cuda` | profile | The oldest host CUDA (driver) version, from offrig's list (`13.2`, `13.1`, `13.0`, `12.9`, … `11.8`). The create sends that floor as `gpu.minCudaVersion`, so a host newer than the newest copied version still qualifies. The `job` profile sets `13.0`. |
| `cuda_runtime` | profile | The CUDA version the pod's software is built for, such as `13.4` for a PyTorch cu134 wheel. It can be newer than any listed host. A 13.x build runs on any CUDA 13 driver under minor-version compatibility, so the floor is the oldest listed host of that major (`13.0` for any 13.x) and is sent as `gpu.minCudaVersion`. A major with no listed host is refused when the config loads. The plan reply names the runtime and a `cuda_note` whenever the runtime differs from that floor: prebuilt kernels work; JIT of newer PTX, and features that need a newer driver, do not. |
| `min_vram_gb` | profile | The least total VRAM a plan accepts. Smaller offers are dropped. |
| `max_price_hr` | `offrig_plan` | The most the pod may cost, in total $/hr. Dearer offers are dropped, and so is a type with no price listed now. |
| `no_fallback` | `offrig_plan` | Only the profile's first GPU family. The two RTX PRO 6000 Blackwell editions count as one family. |

`cuda_versions` is an optional exact list beside `min_cuda`:

```toml
cuda_versions = ["13.0"]
```

When it is set, the create sends that list as `gpu.allowedCudaVersions`, in the order
written, and does not also send a floor. `offrig_plan` reports the list. Setting it
together with `min_cuda`, including a job image's own floor, is refused. An empty list
is refused. A version that is not in offrig's list is refused. A pin of `["13.0"]` does
not accept a 13.1 host: matching is exact. The accepted versions, newest first, are
13.2, 13.1, 13.0, then 12.9 down to 11.8. 13.2 and 13.1 are ahead of RunPod's documented
v1 enum (read 2026-10-11, which stops at 13.0). They are listed because a writer pod on
2026-10-11 was placed on a 13.2 host. A floor still sends only `gpu.minCudaVersion`.
The built-in profiles do not set `cuda_versions`.

`cuda_versions` beside `cuda_runtime` is allowed only when every pinned version is the
same major as the runtime's host floor and at or above that floor.
`cuda_runtime = "13.4"` with `cuda_versions = ["13.0"]` is accepted, and the create sends
the pin only. `cuda_runtime = "13.4"` with `cuda_versions = ["12.8"]` is refused, and the
message names both values. A pin beside `min_cuda` stays refused.

The worst case becomes `max_hours × min(max_price_hr, the dearest listed price left)`. A
plan with nothing left is refused, with the reason for every dropped GPU.

The plan stores what is left, and the launch rents only from it. After ssh is up, offrig
runs `nvidia-smi` on the pod once and reports the host's CUDA version in `offrig_job`. If
the host is older than the plan's floor, rents a GPU the plan didn't list, or costs more
than the plan's price, `offrig_job` gives a `WARNING`. It never terminates the pod on its
own: that is the caller's call.

## Container disk

A pod has two disks:

- the **container disk**, local to the host;
- the **volume** mounted at `/workspace`.

On some hosts `/workspace` is a slow network filesystem. One job pod measured 32 MB/s
there, against 354 MB/s on its container disk.

`container_disk_gb` on a profile sets the container disk size; the `job` profile has 60.
`offrig_plan container_disk_gb=150` overrides it for one plan, and the plan reply and
`offrig_status` show the size in force. Things to know:

- **It isn't priced.** offrig prices GPU time only.
- **offrig doesn't move your downloads.** Job commands start with `HF_HOME=/workspace/hf`.
  To use the container disk, set your own, for example `HF_HOME=/root/hf python ...`.
- **It's deleted with the pod.** Copy results back with `offrig_get` before
  `offrig_shutdown`.

### A Hugging Face token on a job pod

Anonymous Hugging Face downloads can be slow: one job pod fetched a 32B model at about
11 MB/s. A token can raise that, and it is needed for gated repos. Give the job profile
`"job": { ..., "hf_token_secret": "<name>" }`, where `<name>` is a RunPod secret holding a
read-only token.

- RunPod puts the token in the pod's environment at start. The pod spec carries only the
  reference `{{ RUNPOD_SECRET_<name> }}`.
- An ssh session doesn't inherit that environment. So the bootstrap writes the token to
  `/root/.offrig-hf-token`, readable by root only, and every job command runs with
  `HF_TOKEN_PATH` set to that file. Hugging Face tools read it from there. This works even
  when your command sets its own `HF_HOME`.
- The token never appears in a command line, a log or offrig's files. Anything you run on
  the pod as root can read it, so use a fine-grained, read-only token.

## Staging weights on a network volume

A recipe profile downloads its weights at every launch. For the frontier that was about 20
of the 22 minutes to ready. Staging puts them on a RunPod network volume once:

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- **It bills monthly** whether a pod runs or not: about $21/month for the frontier's 300 GB.
  Only a human stages; no agent tool can.
- **It ties the profile to one data center.** A volume lives in one, so the profile's pods
  then launch only there. Pick one with network storage and the profile's GPUs.
- **The download runs on a cheap pod** in that data center. That pod is terminated on
  success, on failure or on timeout.
- **A failed stage is never forgotten.** The volume is recorded in the profile before the
  download starts. Re-run to resume, or `--remove`.
- **A launch uses only a complete stage.** It runs Hugging Face offline only when the stage
  completed. A half-staged volume downloads the rest instead.
