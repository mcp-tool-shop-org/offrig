---
title: Configuration
description: Tiers and profiles, engine recipes, capacity waits, hardware limits, container disks and staged weights.
sidebar:
  order: 4
---

offrig's settings live in `%APPDATA%\offrig\config.toml`. The file is written the first
time something changes; `offrig init` writes the defaults and prints the path. Delete the
file to return to the defaults.

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

Prices are RunPod's secure-cloud prices, read live; RunPod's pricing page is not the
available price. `offrig gpus` shows what is free now.

## Engine recipes

A profile with a `recipe` runs SGLang instead of Ollama. A recipe sets:

- a pinned image (`lmsysorg/sglang:v0.5.20-cu130`);
- a Hugging Face model the pod downloads at start;
- extra server arguments.

offrig sets tensor parallelism from the GPU count and the context length from the profile.
It keeps the engine on the pod's loopback, and a recipe cannot override any of those.

For a gated model repo, `hf_token_secret` names a RunPod secret. offrig references it as
`{{ RUNPOD_SECRET_<name> }}`, so the token never enters the pod spec.

The launch waits for the engine's `/health` and model list, and reports weights on disk
while it downloads. It stops at once, showing the engine's log, if the engine exits.

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
different price. Four limits keep a plan to hardware its work can use:

| Limit | Where | Effect |
|---|---|---|
| `min_cuda` | profile | The oldest host CUDA (driver) version, from RunPod's list (`13.0` down to `11.8`). The pod is created only on hosts at that version or newer. The `job` profile sets `13.0`. |
| `min_vram_gb` | profile | The least total VRAM a plan accepts. Smaller offers are dropped. |
| `max_price_hr` | `offrig_plan` | The most the pod may cost, in total $/hr. Dearer offers are dropped, and so is a type with no price listed now. |
| `no_fallback` | `offrig_plan` | Only the profile's first GPU family. The two RTX PRO 6000 Blackwell editions count as one family. |

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
