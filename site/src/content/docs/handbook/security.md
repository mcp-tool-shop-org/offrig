---
title: Security and what it changes
description: offrig's threat model, the files it edits on your machine, and how to undo each change.
sidebar:
  order: 6
---

## Threat model

**What offrig handles**

- **Your RunPod API key.** offrig reads it from the `RUNPOD_API_KEY` environment variable
  and sends it only to RunPod's API. It never writes the key to disk or prints it, at any
  log level.
- **SSH access to your pods,** with your own key. Pods allow key login only. A model pod's
  sshd allows only local port forwarding; a job pod's allows none.
- **Your Zed settings and SSH config,** which offrig edits only in marked or named places.

**What it guards against**

- **The model server is exposed to the internet.** It binds to the pod's loopback, the pod
  exposes only port 22, and a recipe cannot move the engine off loopback.
- **Your API key reaches a model server.** Zed's provider is deliberately not named
  `runpod`. Under that name, Zed would read `RUNPOD_API_KEY` and send it along.
- **A swapped host.** Host keys are pinned per endpoint in a separate known-hosts file.
  offrig forgets a key only when the pod's endpoint changes, because RunPod reuses ip:port
  pairs across pods.
- **Shell injection.** Model names are checked against Ollama's name syntax before they
  reach a remote shell. Job commands are sent as base64, so the ssh shell never parses
  them.
- **Killing something that isn't offrig's.** An orphaned tunnel is killed only if the
  listener is `ssh.exe` carrying the lane's exact alias and forward. Anything else on the
  port is refused, never killed.
- **Runaway spend.** A plan's worst case is committed against a human-set cap before
  anything is rented, a launch takes only a plan id, and a watchdog ends every side-car pod
  at its deadline.
- **Projects interfering with each other.** A side-car only touches pods named for its own
  lane.

**What it does not do**

- **No telemetry.** offrig talks only to RunPod's API, your pods, and your local Ollama,
  which it asks for its model list to compare against the pod's.
- **It does not judge what you run on a pod.** Replies from a pod's model and job output
  are returned as untrusted text.

## What it changes on your machine

| What | Where | Undo |
|---|---|---|
| Zed provider `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`. The first original is kept as `settings.json.offrig.bak`. |
| Zed default model (only if you ask) | the same file | `offrig zed-remove` restores the previous default |
| `OFFRIG_API_KEY`, a placeholder because Zed wants a key | your user environment | `setx OFFRIG_API_KEY ""`, or remove it in System Properties |
| SSH alias `offrig` | `~/.ssh/config`, between `# >>> offrig:offrig >>>` markers | delete the marked block |
| SSH alias `offrig-<tag>`, one per project that launched from a side-car | `~/.ssh/config`, between `# >>> offrig:offrig-<tag> >>>` markers | `offrig_shutdown` removes it when it names that plan's pod; otherwise delete the marked block |
| Project lanes | `lanes.toml` in `%APPDATA%\offrig` | delete the project's entry while no pod runs in its lane, or the whole file |
| Pod host keys | `~/.ssh/known_hosts_offrig` | delete the file |
| Settings | `%APPDATA%\offrig\config.toml` | delete the file |
| A project's store | `<project>/.offrig/` | delete the folder; the plans, memory and handoffs in it are lost |
| Staged weights (only with `offrig stage --yes`) | a RunPod network volume `offrig-<profile>`, billed monthly | `offrig stage <profile> --remove --yes` |

Edits to Zed's settings go through a JSONC syntax tree, so comments and layout are kept.
An edit that does not read back is not written, and a broken settings file is reported,
never rewritten.

## Reporting a vulnerability

Report it privately through GitHub's security advisories on the
[repository](https://github.com/mcp-tool-shop-org/offrig/security), or by email to
64996768+mcp-tool-shop@users.noreply.github.com. Please don't open a public issue.
