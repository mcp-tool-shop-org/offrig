# offrig: how it works

Mapped at 2026-10-15 from commit 8b7acf1 by Atlas 1.24.0.

## What this is

Runs big models on RunPod and wires them into Zed through an SSH tunnel, so they never run on the local GPU. A Rust core library with a CLI and an egui desktop app on top. (written by a person)

6 parts, mostly Rust (86 files), Python (3), CSS (2), TypeScript (2), Astro (1), JavaScript (1), PowerShell (1) and shell (1). Work enters through 7 doors; ci and release each reach 5 parts, and ci is followed because a pull request goes through it. It deploys a site to GitHub Pages. People run offrig, offrig-app and offrig-mcp.

## What changed since 2026-10-09 (d56f458)

- ci's push trigger now also names `tools/**`.
- ci now also runs crates/offrig-core/src/account.rs, crates/offrig-core/tests/engine_ready.rs, crates/offrig-mcp/tests/engine_ready.rs and 2 more.
- In crates/offrig-cli/src/main.rs, `main` gained a step, `parse`, before `default_for`.
- In crates/offrig-cli/src/main.rs, `main` gained a step, `default_for`, before `report`.
- In crates/offrig-cli/src/main.rs, `main` gained a step, `report_json`, before `open`.
- And 10 more changes to the order of work.
- embed-serve is a new part, drawn from `tools/embed-serve/**`.
- 8 files added and 61 changed content, across 5 parts.

## What comes in

1. **ci.** On a pull request; on a push to main touching 10 paths; or by hand. Runs crates/offrig-app/build.rs, crates/offrig-app/src/app.rs, crates/offrig-app/src/app/ and 74 more; checks crates/offrig-core/examples/fake_ssh.rs, crates/offrig-core/src/lib.rs and crates/offrig-mcp/src/main.rs.
2. **release.** When a tag matching `v*` is pushed; or by hand. Runs crates/offrig-app/build.rs; checks README.md, crates/offrig-app/src/main.rs, crates/offrig-cli/src/main.rs and 1 more.
3. **Deploy site to GitHub Pages.** On a push to main touching 2 paths; or by hand. Runs site/astro.config.mjs and site/src/.
4. **offrig** (a command people run). Runs crates/offrig-cli/src/main.rs.
5. **offrig-app** (a command people run). Runs crates/offrig-app/src/main.rs.
6. **offrig-mcp** (a command people run). Runs crates/offrig-mcp/src/main.rs.
7. **fake_ssh** (a command people run with `cargo run --example fake_ssh`). Runs crates/offrig-core/examples/fake_ssh.rs.

## What happens through ci

1. The workflow runs tools/embed-serve/ in embed-serve, 5 files in offrig-app, 5 files in offrig-cli, 46 files in offrig-core, and 19 files in offrig-mcp; it checks crates/offrig-core/examples/fake_ssh.rs and crates/offrig-core/src/lib.rs in offrig-core and crates/offrig-mcp/src/main.rs in offrig-mcp.
2. It uploads coverage to Codecov.

## Who reads the results

ci writes nothing this map can see.

## The other doors

**release** runs crates/offrig-app/build.rs, checks README.md, crates/offrig-app/src/main.rs, crates/offrig-cli/src/main.rs and 1 more, reaches offrig-core, and creates a GitHub release.

**Deploy site to GitHub Pages** runs site/astro.config.mjs and site/src/, and deploys the site.

**offrig** (a command people run) runs crates/offrig-cli/src/main.rs and reaches offrig-core.

**offrig-app** (a command people run) runs crates/offrig-app/src/main.rs and reaches offrig-core.

**offrig-mcp** (a command people run) runs crates/offrig-mcp/src/main.rs and reaches offrig-core.

**fake_ssh** (a command people run with `cargo run --example fake_ssh`) runs crates/offrig-core/examples/fake_ssh.rs.

## What breaks what

- **offrig-core** is imported by 3 parts (offrig-app, offrig-cli, offrig-mcp) and sits on the path of 6 doors.
- **offrig-app** is imported by no other part and sits on the path of 3 doors.
- **offrig-cli** is imported by no other part and sits on the path of 3 doors.
- **offrig-mcp** is imported by no other part and sits on the path of 3 doors.

## What tends to change together

- **crates/offrig-cli/src/verify_cmd.rs** and **crates/offrig-core/src/calibrate_run.rs** changed together in 6 of 9 commits, and the offrig-cli part imports the offrig-core part.
- **crates/offrig-core/src/runpod.rs** and **crates/offrig-core/src/stage.rs** changed together in 5 of 8 commits, inside the offrig-core part.
- **crates/offrig-core/src/calibrate_run.rs** and **crates/offrig-core/src/verify.rs** changed together in 5 of 9 commits, inside the offrig-core part.
- **crates/offrig-core/src/lib.rs** and **crates/offrig-core/src/store.rs** changed together in 12 of 22 commits, inside the offrig-core part.
- **crates/offrig-core/src/config.rs** and **crates/offrig-core/src/spec.rs** changed together in 12 of 23 commits, inside the offrig-core part.

Confidence is low: fewer than 25 source files reach 10 revisions in the window.

Window: 180 days; a pair counts from 3 shared commits, since 10 source files reach 10 revisions; the floor rises to 10 when 25 do.

## What no test touches

Every code part is touched by at least one test.

offrig-app is tested only by the unit tests in its own files.

offrig-cli is tested only by the unit tests in its own files.

offrig-mcp is tested only by the unit tests in its own files.

## Written but never read

- **crates/offrig-app/assets/icon/offrig.ico** is written by crates/offrig-app/assets/icon/make_ico.py and read by nothing else in this repository.

## Helpers that look duplicated

No two parts export a helper that looks alike.

## Generated, never hand-edited

- **crates/offrig-app/assets/icon/offrig.ico** is written by crates/offrig-app/assets/icon/make_ico.py.

## Hand-authored

People write root; 13 writes with paths built at run time may land here.

## Where to start

crates/offrig-cli/src/main.rs → crates/offrig-core/src/trace.rs

Read those in order to follow one run of offrig end to end. This path follows offrig (a command people run) from its entry, since ci runs only tests, scripts that import no code here and checks.

## What this map cannot see

- 13 writes and 7 reads use paths built at run time and are not named here.
- 11 writes and 13 reads go to a path their caller passes, not to this repository.
- 4 writes go to a temporary directory, not to this repository.
- 21 files belong to no part: scripts/verify.ps1, scripts/verify.sh, site/astro.config.mjs and 18 more.
- Statistics confidence is low: fewer than 25 source files reach 10 revisions in the window.

Regenerate with `npx --yes @dogfood-lab/atlas map`.
