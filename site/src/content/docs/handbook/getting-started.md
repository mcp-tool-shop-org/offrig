---
title: Getting started
description: Install offrig on Windows, connect it to RunPod, and run a first pod.
sidebar:
  order: 1
---

## What you need

- **Windows**, with OpenSSH. It is built into Windows 10 and 11.
- **A RunPod account** with some balance. offrig uses RunPod's secure cloud.
- **Zed**, if you want the pod's models in an editor. The CLI and the side-car work
  without it.

## Install

1. Download `offrig-<version>-windows-x64.zip` from the
   [Releases page](https://github.com/mcp-tool-shop-org/offrig/releases).
2. Check it against the release's `SHA256SUMS`:

   ```powershell
   Get-FileHash .\offrig-<version>-windows-x64.zip -Algorithm SHA256
   ```

3. Unzip it somewhere on your `PATH`, for example `%USERPROFILE%\.local\bin`. It holds
   three programs:
   - `offrig.exe`, the CLI;
   - `offrig-app.exe`, the desktop app;
   - `offrig-mcp.exe`, the side-car for agents.

To build from source instead, clone the repository and run `cargo build --release`. The
compiler version is pinned in `rust-toolchain.toml`.

## Connect it to RunPod

1. Create an API key in RunPod's console. Put it in the **user** environment variable
   `RUNPOD_API_KEY`. offrig reads it from there and never writes it anywhere.

   ```powershell
   setx RUNPOD_API_KEY "<your key>"
   ```

   Open a new terminal afterwards so the variable is visible.
2. Add your SSH public key in RunPod's account settings. offrig uses
   `~/.ssh/runpod_rustline` if it exists, then `~/.ssh/id_ed25519`.

Check it works. This costs nothing:

```text
offrig status
offrig gpus --count 1
```

`status` prints your balance, current spend and runway, then every pod on the account,
each marked `offrig`, `lane` or `other`. `gpus` lists live offers for a GPU count, with
price and stock.

## Your first pod

The `small` tier is the cheapest way to see the whole loop: one small card running
`qwen3:4b`, at about $0.25 an hour.

```text
offrig up small
```

offrig shows the cheapest free match and your runway with the pod running, then:

1. creates the pod and waits for SSH;
2. pulls the profile's models on the pod;
3. opens the tunnel on `127.0.0.1:11435`;
4. adds the pod's models to Zed as their own `offrig` provider;
5. runs the seven guard checks.

It then holds the tunnel until you stop it. In Zed, the models appear in the agent panel
as "RunPod · …". Restart Zed once after the first launch so it sees `OFFRIG_API_KEY`.

When you're done:

```text
offrig down small --yes
```

The pod is terminated and billing stops. Its disk goes with it.

## Next

- To launch from a window instead, see [App and CLI](../usage/).
- To let an agent rent GPUs within a budget, see [The side-car](../side-car/).
