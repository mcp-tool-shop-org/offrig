---
title: Reference
description: Flags, exit codes, MCP tool errors and their codes, ports, files, and how to verify a build.
sidebar:
  order: 7
---

## `offrig` global flags

| Flag | Prints |
|---|---|
| `-q`, `--quiet` | Errors only. Progress and confirmations are dropped, but a command's own results (tables, the budget, guard results) still print. |
| *(none)* | Progress and results. |
| `-v`, `--verbose` | Also, on stderr, each RunPod call and how long it took (`runpod: <what> -> <status> in N ms`), and the total time. |
| `--debug` | Everything `--verbose` prints, plus failed response bodies and errors as `error [code, exit N]: <full chain>`. |

The three are mutually exclusive. At every level, the RunPod API key and any `Bearer`
token are redacted. A panic prints one line unless `--debug` is set.

The commands are listed in [App and CLI](../usage/#the-cli). `offrig --help` and
`offrig <command> --help` describe every flag.

## Exit codes

| Code | Meaning | Examples |
|---|---|---|
| `0` | Success | |
| `1` | Something to fix on your side | Bad arguments, config errors, a guard refusal, a missing API key, a pod not found, a budget refusal, a cancelled wait |
| `2` | Something failed at runtime | RunPod's API, the network, ssh, a timeout, disk or database errors, the model server, no GPU capacity |

## `offrig-mcp`

```text
offrig-mcp --project <dir>                          serve MCP over stdio for a project
offrig-mcp --help | --version
offrig-mcp --sidecar-port --project <dir>           print the project's side-car port
offrig-mcp --sidecar-port --check --project <dir>   also exit 1 if something holds it
```

The tools are described in [The side-car](../side-car/#all-tools).

### Tool errors

A tool never fails at the protocol level. Every error, including bad arguments and an
unknown tool name, is a structured result:

```json
{
  "ok": false,
  "code": "budget_exceeded",
  "error": "worst case $12.22 (3.5h at $3.49/hr) exceeds the $9.11 left of the $20.00 budget; shorten max_hours, pick a cheaper profile, or raise the cap",
  "next_action": "shorten max_hours or choose a cheaper profile; the cap itself is the human's to change",
  "retryable": false
}
```

| Code | Meaning | Retryable |
|---|---|---|
| `invalid_input` | An argument is missing or malformed | no |
| `invalid_state` | The call doesn't fit the current state (for example, no open plan) | no |
| `not_ready` | The pod or engine isn't ready yet | no |
| `missing_api_key` | `RUNPOD_API_KEY` isn't set | no |
| `config` | The config or a registry file is invalid | no |
| `not_found` | No such pod or plan | no |
| `guard_refused` | A safety guard refused the action | no |
| `refused` | offrig refused the action, with the reason | no |
| `budget_exceeded` | The worst case is over what's left of the cap | no |
| `invalid_transition` | A handoff can't move to that state | no |
| `cancelled` | The operation was cancelled | no |
| `runpod_api` | RunPod's API returned an error | only for HTTP 429 and 5xx |
| `network` | The request didn't reach RunPod | yes |
| `ssh` | ssh to the pod failed, or it has no ssh endpoint yet | yes |
| `timeout` | Something took longer than its limit | yes |
| `no_capacity` | No GPU of the plan's types is free; nothing was rented | yes |
| `pod_not_ready` | A pod was rented and billed but never became ready; it was terminated | yes |
| `model_server` | Ollama or SGLang on the pod failed | yes |
| `database` | The project store failed | no |
| `io` | A local file operation failed | no |
| `internal` | Anything else; please report it | no |

The `error` and `next_action` fields are always present. Callers written before `code`
and `retryable` existed keep working.

## Ports

| Port | Used by |
|---|---|
| `11434` | Your local Ollama. offrig never uses it, and refuses it for any tunnel or side-car. |
| `11435`, `11436` | The plain lane's tunnel and runner |
| `11500`–`11627` | Project lanes' tunnels and runners, two ports per lane |
| `11700`–`11763` | Project lanes' side-car ports (shell driver), one per lane |
| `11490` | The CPU-only Ollama that computes project-index embeddings (`embed_url`); never `11434` |

## Files

| Path | What it is |
|---|---|
| `%APPDATA%\offrig\config.toml` | Profiles and settings |
| `%APPDATA%\offrig\lanes.toml` | The project lane registry |
| `<project>\.offrig\offrig.db` | A project's plans, memory and handoffs |
| `<project>\.offrig\out\` | Handoff outputs |
| `<project>\.offrig\watchdog-<plan>.log` | A plan's watchdog log |
| `~/.ssh/known_hosts_offrig` | Pinned pod host keys |

## Calibrate a verifier model

```text
offrig verify calibrate gold/grounded.jsonl gold/reasoning.jsonl --model gemma4:31b --think on --split tune
```

Runs each gold claim through offrig's verifier prompt on a local Ollama (loopback only;
cloud models are refused), with the claim's own evidence, and prints the false-accept
rate, abstain rate and balanced accuracy per check type against the default rule. The run
directory (`.offrig/out/calibrate-<model>-<time>`) holds `manifest.json`,
`verdicts.jsonl` and `metrics.json`. Use `--resume <dir>` to continue a run with the same
settings, and `--report-only <dir>` to rescore one. `--swap-evidence` reverses the
evidence order to check for position bias; `--gpu-cost-hr` sets the cost per claim.

## Verify a build

```text
bash scripts/verify.sh        or    pwsh scripts/verify.ps1
```

Both run the format check, clippy with warnings as errors, the full test suite, and smoke
runs of `offrig --help`, `offrig --version` and `offrig-mcp --help`. CI also runs
`cargo deny`, an OSV scan of `Cargo.lock`, coverage (it fails below 90% of lines) and
`atlas check`.

Release binaries are built by CI from the version tag. A local release build embeds the
builder's paths, so publish only CI builds.
