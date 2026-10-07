---
title: offrig Handbook
description: What offrig is, what it guarantees, and where to start.
sidebar:
  order: 0
---

offrig runs big models on rented RunPod GPUs, with a guarantee: they never run on your
own GPU. It is three front ends over one Rust library:

- **`offrig-app`**, a Windows desktop app: pick a tier, press Launch pod, and the pod's
  models appear in Zed.
- **`offrig`**, a CLI for the same work from a terminal.
- **`offrig-mcp`**, an MCP side-car that lets an agent such as Claude Code plan a paid
  session under a human-set budget, rent GPUs, run work on them, and shut them down.

## What it is for

Models too big for your machine (a 120B model on one 96 GB card, or a 480B model across
four), and GPU work your machine shouldn't do: training runs, renders, evaluations.
offrig rents the hardware by the hour, gets it ready, and keeps the bill bounded.

## The three promises

1. **Nothing runs on your GPU.** The model server is reachable only through an SSH
   tunnel. The tunnel refuses your local Ollama's port, so a dead tunnel fails instead of
   falling through to the local server. Seven checks verify this on every launch.
2. **No pod outlives its plan.** Every side-car launch starts a watchdog: a separate
   process that terminates the pod at the plan's deadline, even if everything else is
   gone. A failed setup terminates the pod instead of leaving it billing.
3. **An agent cannot name its own price.** A human sets each project's budget cap. A plan
   commits its worst case against it before anything is rented, and a launch takes only a
   plan id.

## Where to go next

| If you want to | Read |
|---|---|
| Install offrig and run a first pod | [Getting started](./getting-started/) |
| Use the app or the CLI day to day | [App and CLI](./usage/) |
| Give an agent a GPU and a budget | [The side-car](./side-car/) |
| Change tiers, GPUs, prices or disks | [Configuration](./configuration/) |
| Run several projects at once, or GPU jobs | [Lanes and job pods](./lanes-and-jobs/) |
| Know exactly what offrig touches | [Security and what it changes](./security/) |
| Look up a command, tool, error code or exit code | [Reference](./reference/) |

## Status

offrig has run real work since 2026-10-02. Highlights:

- A frontier run on 4 × RTX PRO 6000 served Qwen3-Coder-480B on SGLang and drained 31
  handoffs in 20 seconds, for $3.59.
- Training runs for aspire-si on job pods.
- Singing renders for ai-jam-sessions on cheap A40 pods.
- Two projects using it at once, each in its own lane.

The local GPU stayed idle throughout.
