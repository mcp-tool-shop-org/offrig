# offrig: how it works

Mapped at 2026-10-02 from commit d64c817 by Atlas 1.24.0.

## What this is

Runs big models on RunPod and wires them into Zed through an SSH tunnel, so they never run on the local GPU. A Rust core library with a CLI and an egui desktop app on top. (written by a person)

5 parts, mostly Rust (24 files). Work enters through 4 doors; the busiest is ci, which reaches 4 parts. offrig, offrig-app and offrig-mcp are commands built from crates/offrig-app, crates/offrig-cli and crates/offrig-mcp (nothing ships them).

## What changed since 2026-10-02 (8c7b5a1)

- offrig-mcp now imports offrig-core.
- offrig-mcp is a new part, drawn from `crates/offrig-mcp/**`.
- 3 files moved and 1 changed content, across 2 parts.

## What comes in

1. **ci.** On a pull request; on a push to main touching 8 paths; or by hand. Runs crates/offrig-app/src/app.rs, crates/offrig-core/src/config.rs, crates/offrig-core/src/context.rs and 13 more; checks crates/offrig-app/src/main.rs, crates/offrig-cli/src/main.rs, crates/offrig-core/src/lib.rs and 1 more.
2. **offrig** (a command built from crates/offrig-cli, which nothing ships). Runs crates/offrig-cli/src/main.rs.
3. **offrig-app** (a command built from crates/offrig-app, which nothing ships). Runs crates/offrig-app/src/main.rs.
4. **offrig-mcp** (a command built from crates/offrig-mcp, which nothing ships). Runs crates/offrig-mcp/src/main.rs.

## What happens through ci

1. The workflow runs crates/offrig-app/src/app.rs in offrig-app, 14 files in offrig-core, and crates/offrig-mcp/tests/ in offrig-mcp; it checks crates/offrig-app/src/main.rs in offrig-app, crates/offrig-cli/src/main.rs in offrig-cli, crates/offrig-core/src/lib.rs in offrig-core and crates/offrig-mcp/src/main.rs in offrig-mcp.

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

No two source files changed together often enough to name.

Window: 180 days; a pair counts from 3 shared commits, since the window holds fewer than 30 qualifying commits.

## What no test touches

- **offrig-cli** is imported by no test.

offrig-mcp is touched by tests only through a spawn: a test runs its files as a child process.

offrig-app is tested only by the unit tests in its own files.

## Written but never read

No place this map can see is written, so none goes unread.

## Helpers that look duplicated

No two parts export a helper that looks alike.

## Generated, never hand-edited

Nothing in this repository writes to a tracked place this map can see.

## Hand-authored

People write root; 4 writes with paths built at run time may land here.

## Where to start

crates/offrig-cli/src/main.rs → crates/offrig-core/src/store.rs → crates/offrig-core/src/cost.rs → crates/offrig-core/src/remote.rs → crates/offrig-core/src/proc.rs → crates/offrig-core/src/error.rs

Read those in order to follow one run of offrig end to end. This path follows offrig (a command built from crates/offrig-cli, which nothing ships) from its entry, since ci runs only tests and checks.

## What this map cannot see

- 4 writes and 1 read use paths built at run time and are not named here.
- 1 write and 7 reads go to a path their caller passes, not to this repository.
- Statistics confidence is low: fewer than 30 qualifying commits in the window, and fewer than 25 source files reach 10 revisions.

Regenerate with `npx --yes @dogfood-lab/atlas map`.
