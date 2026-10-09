---
title: App and CLI
description: Launch, use and shut down pods from the desktop app or the offrig command line.
sidebar:
  order: 2
---

The app and the CLI do the same work over the same library. Both use the **plain lane**:
one SSH alias `offrig`, the tunnel on `127.0.0.1:11435`, and pods named
`offrig-<profile>`. A side-car's pods live in their own lanes and are never touched from
here (see [Lanes and job pods](../lanes-and-jobs/)).

## The app

Start `offrig-app`. From one window it:

1. shows your RunPod balance, live GPU prices and how long the balance lasts;
2. launches the pod for the profile you pick, and pulls its models on the pod;
3. opens the SSH tunnel;
4. adds the pod's models to Zed as their own provider;
5. runs the seven guard checks;
6. shuts the pod down, or terminates it after a stretch with every GPU idle.

**Cancel launch** stops a launch that is waiting for GPUs; nothing is rented while it
waits. Closing the app with a pod running asks whether to terminate the pod or leave it
running.

## The CLI

| Command | What it does | Costs money |
|---|---|---|
| `offrig status` | Balance, spend per hour, runway, and every pod on the account | no |
| `offrig gpus --count 2 [--min-vram 160]` | Live secure-cloud offers for a GPU count, cheapest first | no |
| `offrig profiles` | The tiers in your config, with their models and sizes | no |
| `offrig init` | Write the default config file if there is none, and print its path | no |
| `offrig up <profile>` | Launch, pull the models, wire Zed, run the checks, hold the tunnel | **yes** |
| `offrig up <profile> --wait 180` | Wait up to 180 minutes for GPUs, renting nothing meanwhile | **yes**, once they free up |
| `offrig up <profile> --detach` | Set up, then exit instead of holding the tunnel | **yes** |
| `offrig tunnel <profile>` | Hold the tunnel to a running pod, with idle auto-stop | the pod's rate |
| `offrig pull <model> [<profile>]` | Pull another model onto the pod; the pull runs on the pod | the pod's rate |
| `offrig models [<profile>]` | Models on the pod, read over SSH | no |
| `offrig check [<model>]` | A streamed chat with a tool call through the tunnel, the way Zed sends it | the pod's rate |
| `offrig guard [<profile>]` | Run the seven "never on my GPU" checks | no |
| `offrig zed [<profile>]` / `offrig zed-remove` | Write or remove offrig's provider in Zed's settings | no |
| `offrig connect [/workspace]` | Open the pod in Zed for remote editing | the pod's rate |
| `offrig down <profile> --yes` | Terminate the pod; its disk goes with it | stops billing |
| `offrig budget [--provider runpod\|openrouter] [<usd>]` | Set this project's caps (human only): one per provider, plus an optional overall ceiling. With no amount in a terminal, a menu; `--show` prints every cap beside each provider's own balance. Never below spent + committed | no |
| `offrig stage <profile> --dc <DC> [--yes]` | Stage a recipe profile's weights on a network volume | **yes, monthly** |

`offrig up` refuses when your runway with the pod running would be under one hour,
because at zero RunPod stops every pod on the account. `--yes` overrides that refusal.

## Zed

The pod's models are a separate provider called `offrig`, pointing at the tunnel. If the
pod is down, choosing one of its models errors; Zed never falls back to another provider.
`offrig up <profile> --default-model <model>` also makes one of them Zed's default agent
model. `offrig zed-remove` takes the provider out and restores the previous default.

The provider is deliberately not named `runpod`. Zed would then read `RUNPOD_API_KEY`
and send it to the model server.

## Money safety

- Before a launch, offrig shows the cheapest free match and your runway with the pod
  running.
- Auto-stop terminates the pod after 30 minutes with every GPU under 5% busy. The minutes
  are set by `auto_stop_idle_minutes` in the config; `auto_stop_idle_minutes = "off"`
  turns it off.
- offrig only changes pods it named. Every other pod on the account is listed and left
  alone.

## The seven guard checks

| Check | Fails when |
|---|---|
| Tunnel avoids the local Ollama port | the tunnel port is 11434 |
| Zed sends pod models through the tunnel | Zed's provider URL is anything but the tunnel |
| The pod's Ollama is not exposed to the internet | the pod maps port 11434 publicly |
| The tunnel ends at the pod | the model list through the tunnel differs from the list read on the pod over SSH |
| Pod models are not on this machine | a pod model also exists in the local Ollama |
| No pod model shares a name with a local Zed model | a name in the offrig provider is also in Zed's local Ollama list |
| Every model Zed offers is on the pod | Zed offers a model the pod does not have |
