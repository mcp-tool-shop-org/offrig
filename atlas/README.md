# offrig: how it works

Mapped at 2026-10-07 from commit 2e8cadb by Atlas 1.24.0.

## What this is

Runs big models on RunPod and wires them into Zed through an SSH tunnel, so they never run on the local GPU. A Rust core library with a CLI and an egui desktop app on top. (written by a person)

5 parts, mostly Rust (67 files), PowerShell (1), Python (1) and shell (1). Work enters through 6 doors; the busiest is release, which reaches 5 parts. People run offrig, offrig-app and offrig-mcp.

## What changed since 2026-10-07 (8a2c9b9)

- ci now also runs crates/offrig-core/src/siblings.rs and crates/offrig-mcp/tests/sibling_pods.rs.
- In crates/offrig-cli/src/main.rs, `main` gained a step, `open_default`, before `now_unix`.
- In crates/offrig-cli/src/main.rs, `main` gained a step, `lane_of`, before `read_plan`.
- In crates/offrig-cli/src/main.rs, `main` gained a step, `read_plan`, before `new`.
- And 3 more changes to the order of work.
- 2 files added and 11 changed content, across 3 parts.

## What comes in

1. **release.** When a tag matching `v*` is pushed; or by hand. Runs crates/offrig-app/build.rs; checks README.md, crates/offrig-app/src/main.rs, crates/offrig-cli/src/main.rs and 1 more.
2. **ci.** On a pull request; on a push to main touching 9 paths; or by hand. Runs crates/offrig-app/build.rs, crates/offrig-app/src/app.rs, crates/offrig-app/src/app/ and 53 more; checks crates/offrig-cli/src/main.rs, crates/offrig-core/examples/fake_ssh.rs, crates/offrig-core/src/lib.rs and 1 more.
3. **offrig** (a command people run). Runs crates/offrig-cli/src/main.rs.
4. **offrig-app** (a command people run). Runs crates/offrig-app/src/main.rs.
5. **offrig-mcp** (a command people run). Runs crates/offrig-mcp/src/main.rs.
6. **fake_ssh** (a command people run with `cargo run --example fake_ssh`). Runs crates/offrig-core/examples/fake_ssh.rs.

## What happens through release

1. The workflow runs crates/offrig-app/build.rs in offrig-app; it checks crates/offrig-app/src/main.rs in offrig-app, crates/offrig-cli/src/main.rs in offrig-cli, crates/offrig-mcp/src/main.rs in offrig-mcp and README.md in root.
2. That reaches offrig-core (28 files).
3. It creates a GitHub release.

## Who reads the results

release writes nothing this map can see.

## The other doors

**ci** runs crates/offrig-app/build.rs, crates/offrig-app/src/app.rs, crates/offrig-app/src/app/ and 53 more, checks crates/offrig-cli/src/main.rs, crates/offrig-core/examples/fake_ssh.rs, crates/offrig-core/src/lib.rs and 1 more, and uploads coverage to Codecov.

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

- **crates/offrig-app/src/worker.rs** and **crates/offrig-cli/src/main.rs** changed together in 6 of 9 commits, though neither part imports the other.
- **crates/offrig-app/src/worker.rs** and **crates/offrig-core/src/session.rs** changed together in 6 of 10 commits, and the offrig-app part imports the offrig-core part.
- **crates/offrig-cli/src/main.rs** and **crates/offrig-core/src/session.rs** changed together in 7 of 12 commits, and the offrig-cli part imports the offrig-core part.
- **crates/offrig-mcp/src/main.rs** and **crates/offrig-mcp/src/ops.rs** changed together in 10 of 18 commits, inside the offrig-mcp part.
- **crates/offrig-core/src/lib.rs** and **crates/offrig-core/src/store.rs** changed together in 6 of 11 commits, inside the offrig-core part.

Confidence is low: fewer than 25 source files reach 10 revisions in the window.

Window: 180 days; a pair counts from 3 shared commits, since 4 source files reach 10 revisions; the floor rises to 10 when 25 do.

## What no test touches

Every code part is touched by at least one test.

offrig-cli is touched by tests only through a spawn: a test runs its files as a child process.

offrig-app is tested only by the unit tests in its own files.

offrig-mcp is tested only by the unit tests in its own files.

## Written but never read

- **crates/offrig-app/assets/icon/offrig.ico** is written by crates/offrig-app/assets/icon/make_ico.py and read by nothing else in this repository.

## Helpers that look duplicated

No two parts export a helper that looks alike.

## Generated, never hand-edited

- **crates/offrig-app/assets/icon/offrig.ico** is written by crates/offrig-app/assets/icon/make_ico.py.

## Hand-authored

People write root; 7 writes with paths built at run time may land here.

## Where to start

crates/offrig-cli/src/main.rs → crates/offrig-core/src/trace.rs

Read those in order to follow one run of offrig end to end. This path follows offrig (a command people run) from its entry, since ci runs only tests, scripts that import no code here and checks.

## What this map cannot see

- 7 writes and 4 reads use paths built at run time and are not named here.
- 9 writes and 9 reads go to a path their caller passes, not to this repository.
- 4 writes go to a temporary directory, not to this repository.
- 2 files belong to no part: scripts/verify.ps1 and scripts/verify.sh.
- Statistics confidence is low: fewer than 25 source files reach 10 revisions in the window.

Regenerate with `npx --yes @dogfood-lab/atlas map`.
