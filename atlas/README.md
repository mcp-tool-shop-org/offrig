# offrig: how it works

Mapped at 2026-10-07 from commit 1c76e70 by Atlas 1.24.0.

## What this is

Runs big models on RunPod and wires them into Zed through an SSH tunnel, so they never run on the local GPU. A Rust core library with a CLI and an egui desktop app on top. (written by a person)

5 parts, mostly Rust (38 files). Work enters through 4 doors; the busiest is ci, which reaches 4 parts. offrig, offrig-app and offrig-mcp are commands built from crates/offrig-app, crates/offrig-cli and crates/offrig-mcp (nothing ships them).

## What changed since 2026-10-07 (23ee566)

- ci now also runs crates/offrig-core/src/planning.rs and crates/offrig-mcp/tests/plan_limits.rs.
- 2 files added and 10 changed content, across 3 parts.

## What comes in

1. **ci.** On a pull request; on a push to main touching 8 paths; or by hand. Runs crates/offrig-app/src/app.rs, crates/offrig-core/src/checks.rs, crates/offrig-core/src/config.rs and 25 more; checks crates/offrig-app/src/main.rs, crates/offrig-cli/src/main.rs, crates/offrig-core/src/lib.rs and 1 more.
2. **offrig** (a command built from crates/offrig-cli, which nothing ships). Runs crates/offrig-cli/src/main.rs.
3. **offrig-app** (a command built from crates/offrig-app, which nothing ships). Runs crates/offrig-app/src/main.rs.
4. **offrig-mcp** (a command built from crates/offrig-mcp, which nothing ships). Runs crates/offrig-mcp/src/main.rs.

## What happens through ci

1. The workflow runs crates/offrig-app/src/app.rs in offrig-app, 21 files in offrig-core, and 6 files in offrig-mcp; it checks crates/offrig-app/src/main.rs in offrig-app, crates/offrig-cli/src/main.rs in offrig-cli, crates/offrig-core/src/lib.rs in offrig-core and crates/offrig-mcp/src/main.rs in offrig-mcp.

## Who reads the results

ci writes nothing this map can see.

## The other doors

**offrig** (a command built from crates/offrig-cli, which nothing ships) runs crates/offrig-cli/src/main.rs and reaches offrig-core.

**offrig-app** (a command built from crates/offrig-app, which nothing ships) runs crates/offrig-app/src/main.rs and reaches offrig-core.

**offrig-mcp** (a command built from crates/offrig-mcp, which nothing ships) runs crates/offrig-mcp/src/main.rs and reaches offrig-core.

## What breaks what

- **offrig-core** is imported by 3 parts (offrig-app, offrig-cli, offrig-mcp) and sits on the path of 4 doors.
- **offrig-app** is imported by no other part and sits on the path of 2 doors.
- **offrig-cli** is imported by no other part and sits on the path of 2 doors.
- **offrig-mcp** is imported by no other part and sits on the path of 2 doors.

## What tends to change together

- **crates/offrig-cli/src/main.rs** and **crates/offrig-core/src/session.rs** changed together in 5 of 9 commits, and the offrig-cli part imports the offrig-core part.
- **crates/offrig-core/src/config.rs** and **crates/offrig-core/src/session.rs** changed together in 6 of 12 commits, inside the offrig-core part.
- **crates/offrig-core/src/config.rs** and **crates/offrig-core/src/spec.rs** changed together in 5 of 10 commits, inside the offrig-core part.

Confidence is low: fewer than 30 qualifying commits in the window, and fewer than 25 source files reach 10 revisions.

Window: 180 days; a pair counts from 3 shared commits, since the window holds fewer than 30 qualifying commits.

## What no test touches

- **offrig-cli** is imported by no test.

offrig-app is tested only by the unit tests in its own files.

offrig-mcp is tested only by the unit tests in its own files.

## Written but never read

No place this map can see is written, so none goes unread.

## Helpers that look duplicated

No two parts export a helper that looks alike.

## Generated, never hand-edited

Nothing in this repository writes to a tracked place this map can see.

## Hand-authored

People write root; 6 writes with paths built at run time may land here.

## Where to start

crates/offrig-cli/src/main.rs → crates/offrig-core/src/store.rs → crates/offrig-core/src/checks.rs → crates/offrig-core/src/error.rs

Read those in order to follow one run of offrig end to end. This path follows offrig (a command built from crates/offrig-cli, which nothing ships) from its entry, since ci runs only tests and checks.

## What this map cannot see

- 6 writes and 2 reads use paths built at run time and are not named here.
- 5 writes and 7 reads go to a path their caller passes, not to this repository.
- Statistics confidence is low: fewer than 30 qualifying commits in the window, and fewer than 25 source files reach 10 revisions.

Regenerate with `npx --yes @dogfood-lab/atlas map`.
