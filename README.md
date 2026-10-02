# offrig

Run big models on RunPod and use them from Zed, with a guarantee: they never run on
your own GPU. A desktop app and a CLI over one Rust library.

## What it does

From one window (or one command), offrig:

1. shows your RunPod balance, live GPU prices and how long the balance will last;
2. launches a pod for a tier (small, medium or frontier) and pulls its models onto the pod;
3. opens an SSH tunnel to the pod;
4. adds the pod's models to Zed as their own provider;
5. runs seven checks that the models cannot land on this machine;
6. shuts the pod down, or terminates it after a stretch with every GPU idle.

## The guarantee, and how it holds

- **The model server is unreachable except through the tunnel.** The pod runs a pinned
  Ollama (`ollama/ollama:0.35.0`) bound to the pod's own loopback, and the pod exposes
  only `22/tcp`. There is no public HTTP endpoint to find or abuse.
- **Zed talks to the tunnel, on its own port.** The tunnel listens on `127.0.0.1:11435`.
  Your local Ollama is on `11434`. offrig refuses to put the tunnel on `11434`, so a dead
  tunnel cannot fall through to the local server: the request fails instead.
- **Zed never switches providers.** The pod's models are a separate `offrig` provider in
  Zed. If the pod is down, choosing one of them errors; Zed does not try another provider.
- **The weights never exist locally.** Models are pulled on the pod, by the pod.

The guard checks verify this each time, from facts offrig can observe:

| Check | Fails when |
|---|---|
| Tunnel avoids the local Ollama port | the tunnel port is 11434 |
| Zed sends pod models through the tunnel | Zed's provider URL is anything but the tunnel |
| Pod's Ollama is not exposed to the internet | the pod maps port 11434 publicly |
| The tunnel ends at the pod | the model list through the tunnel differs from the list read on the pod over SSH |
| Pod models are not on this machine | a pod model also exists in the local Ollama |
| No pod model shares a name with a local Zed model | a name in the offrig provider is also in Zed's local Ollama list |
| Every model Zed offers is on the pod | Zed offers a model the pod does not have |

## Install

Needs Windows with OpenSSH (built in), Rust 1.98.1 (pinned in `rust-toolchain.toml`),
Zed, and a RunPod account.

1. Put your RunPod API key in the user environment variable `RUNPOD_API_KEY`.
2. Add your SSH public key in RunPod's account settings. offrig uses
   `~/.ssh/runpod_rustline` if present, then `~/.ssh/id_ed25519`.
3. Build: `cargo build --release`. This makes `target/release/offrig-app.exe` (the app)
   and `target/release/offrig.exe` (the CLI).

## Use

**App:** start `offrig-app`, pick a profile, press **Launch pod**. When it is ready, the
models appear in Zed's agent panel as "RunPod · …". Restart Zed once after the first
launch so it sees `OFFRIG_API_KEY`.

**CLI:**

```text
offrig status                 balance, runway, pods
offrig gpus --count 2         live offers for a GPU count
offrig profiles               tiers and their models
offrig up medium              launch, pull, wire Zed, run the checks, hold the tunnel
offrig tunnel medium          hold the tunnel to a running pod
offrig check gpt-oss:120b     streamed chat with a tool call, the way Zed sends it
offrig guard                  run the seven checks
offrig pull <model>           pull another model onto the pod
offrig connect                open the pod's /workspace in Zed for remote editing
offrig down medium --yes      terminate the pod
offrig zed-remove             take the provider out of Zed
```

## Tiers

Profiles live in `%APPDATA%\offrig\config.toml` (written on first change). Defaults:

| Profile | GPUs | Models | Typical cost |
|---|---|---|---|
| small | 1 × RTX 2000 Ada / A4000 class | `qwen3:4b` | about $0.25/hr |
| medium | 1 × RTX PRO 6000 (96 GB); A100 or H100 80 GB if none is free | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | $2.09/hr (A100 fallback $1.59) |
| frontier | 4 × RTX PRO 6000 (384 GB) | `qwen3-coder:480b` (290 GB), about 90 GB left for context | $8.36/hr |

Each profile lists GPU types in priority order; RunPod takes the first with capacity.
Prices are secure-cloud prices, read live; the pricing page is not the available price.

## Money safety

- Before a launch, offrig shows the cheapest free match and your runway with the pod
  running. Under one hour of runway it refuses unless you override, because at zero
  RunPod stops every pod on the account, including ones offrig does not manage.
- Auto-stop terminates the pod after 30 minutes with every GPU under 5% (configurable,
  or off).
- Closing the app with a pod running asks whether to terminate it or keep it running.
- offrig only touches pods it named (`offrig-<profile>`). Other pods are listed, never
  changed.

## What it changes on your machine

| What | Where | Undo |
|---|---|---|
| Zed provider `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`; the first original is kept as `settings.json.offrig.bak` |
| Zed default model (only if you ask) | same file | `offrig zed-remove` restores the previous default |
| `OFFRIG_API_KEY` (placeholder; Zed wants a key) | user environment | `setx OFFRIG_API_KEY ""` or remove it in System Properties |
| SSH alias `offrig` | `~/.ssh/config`, between `# >>> offrig:offrig >>>` markers | delete the marked block |
| Pod host keys | `~/.ssh/known_hosts_offrig` | delete the file |
| Settings | `%APPDATA%\offrig\config.toml` | delete the file |

Comments and layout in Zed's settings are preserved: edits go through a JSONC syntax tree.

## Threat model

- **RunPod API key.** Read from `RUNPOD_API_KEY`; never written to disk or logs. The Zed
  provider is deliberately not named `runpod`, because Zed would then read
  `RUNPOD_API_KEY` and send it to the model server.
- **Model server.** Reachable only through SSH with your key. Password login is off on
  the pod, and sshd allows only local forwarding.
- **Host keys.** Pinned per endpoint in a separate known-hosts file. offrig forgets a key
  only when the pod endpoint changes, because RunPod reuses ip:port pairs across pods.
- **Shell injection.** Model names are validated against Ollama's name syntax before
  they reach a remote shell.
- **Orphaned tunnels.** If offrig dies, its `ssh` may keep holding the port. On the next
  start offrig kills it, but only if the listener is `ssh.exe` carrying offrig's exact
  forward spec. Anything else on the port is refused, never killed.
- **No telemetry.** offrig talks only to RunPod's API, your pod, and your local Ollama
  (to compare model lists).

## Tests

`cargo test --workspace` runs 59 tests: the core library (RunPod parsing, pod spec,
SSH config, Zed JSONC edits, guard rules, pull-log parsing, cost and idle logic) and the
app (state handling plus click-through UI tests in egui's test harness). CI also runs
fmt, clippy with warnings as errors, `cargo deny` and `atlas check`.

### Live test record (2026-10-02, medium tier, A100 80GB, about $0.45)

- Pod up in about 80 s; sshd, tunnel and the pod's Ollama 0.35.0 answering.
- 97 GB of models pulled on the pod at roughly 150–250 MB/s.
- `qwen3-coder:30b-a3b-q8_0` and `gpt-oss:120b` each answered a streamed chat with a
  correct tool call through the tunnel. They took 36 GB and 64 GB of the pod's VRAM; the
  local Ollama loaded nothing and was not on the local GPU at all.
- All seven guard checks passed, from the CLI and from the app.
- An abruptly killed CLI left its `ssh` holding the port; the next run reclaimed it.
- The app's tunnel, checks, model test and shutdown were driven through its buttons.

Bugs the live run found, now fixed and covered: a backgrounded `&&` list kept ssh's
stdout open and hung the pull start; the pod list lacked GPU types without
`includeMachine=true`; the launch check counted a running pod's price twice.

Not yet verified live: a frontier-tier run (4 × RTX PRO 6000 at $8.36/hr), and a chat
sent from Zed's agent panel itself (the request shape Zed uses is tested directly).

## Standards compliance

Scored against the studio's workflow standards (0 missing, 1 partial, 2 present,
3 exemplary).

- **PIN_PER_STEP: 2.** The pod image is pinned to a version tag (`ollama/ollama:0.35.0`,
  verified on Docker Hub), the compiler to 1.98.1, dependencies by `Cargo.lock`, and the
  Atlas engine to the fleet's 1.24.0. Models are pinned by tag but not by digest.
- **ANDON_AUTHORITY: 3.** Every step halts the run on a defect: a plan whose weights
  exceed the disk is refused before any spend; a failed pull stops the launch; a Zed edit
  that does not read back is not written; a broken settings file is reported, never
  rewritten; CI blocks on fmt, clippy, tests, licences and advisories.
- **NAMED_COMPENSATORS: 2.** Every irreversible action has an undo, listed below.
- **DECOMPOSE_BY_SECRETS: 2.** One module per thing that changes for its own reasons:
  RunPod's API (`runpod`), the pod's contents (`spec`), the transport (`tunnel`,
  `remote`), each local file offrig edits (`sshconfig`, `zed`), and the rules (`guard`,
  `cost`). Front ends hold no logic beyond presentation.
- **UNCERTAINTY_GATED_HUMANS: 2.** offrig asks only where the outcome is costly or
  lossy: launching under an hour of runway, terminating a pod (with what is lost stated),
  and quitting with a pod still billing.
- **EXTERNAL_VERIFIER: n/a.** No specialized claims.

**Compensators**

| Action | Undo | State after undo | Owner |
|---|---|---|---|
| Create a pod (starts billing) | `offrig down <profile> --yes`, the app's Shut down, or auto-stop | pod terminated, billing stopped | the operator running offrig |
| Terminate a pod | none for its disk; relaunch the profile and the models re-pull (a network volume keeps them) | new pod, same profile | the operator |
| Write the Zed provider or default model | `offrig zed-remove`, or restore `settings.json.offrig.bak` | Zed as before offrig | the operator |
| Set `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""` or delete it in System Properties | variable gone | the operator |
| Write the SSH alias | delete the marked block in `~/.ssh/config` | config as before | the operator |
| Pull a model on the pod | `ollama rm <model>` on the pod, or terminate the pod | model gone | the operator |
| Kill an orphaned offrig tunnel | none needed; only offrig's own `ssh` is ever killed | port free | offrig |

## Layout

```text
crates/offrig-core   library: RunPod client, pod spec, tunnel, remote ops, Zed and SSH
                     edits, guard, cost and idle logic, session workflow
crates/offrig-cli    `offrig` command line
crates/offrig-app    `offrig-app` desktop app (egui)
atlas/               Atlas map of the repo (regenerate with `atlas map`)
```

## License

MIT. See [LICENSE](LICENSE).
