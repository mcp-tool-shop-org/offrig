# offrig: how it works

Mapped at 2026-10-02 from commit c1b1cea by Atlas 1.24.0.

## What this is

Runs big models on RunPod and wires them into Zed through an SSH tunnel, so they never run on the local GPU. A Rust core library with a CLI and an egui desktop app on top. (written by a person)

4 parts, mostly Rust (22 files). Work enters through 3 doors; the busiest is ci, which reaches 3 parts. offrig and offrig-app are commands built from crates/offrig-app and crates/offrig-cli (nothing ships them).

## What changed since 2026-10-02 (53c9b4b)

- ci now also runs crates/offrig-core/src/context.rs, crates/offrig-core/src/roles.rs and crates/offrig-core/src/store.rs.
- crates/offrig-core/roles/game-designer.json is now read by crates/offrig-core/src/roles.rs.
- crates/offrig-core/roles/game-designer.md is now read by crates/offrig-core/src/roles.rs.
- crates/offrig-core/roles/lore-keeper.json is now read by crates/offrig-core/src/roles.rs.
- And 5 more new writers and readers of places.
- 11 files added, across 1 part.

## What comes in

1. **ci.** On a pull request; on a push to main touching 8 paths; or by hand. Runs crates/offrig-app/src/app.rs, crates/offrig-core/src/config.rs, crates/offrig-core/src/context.rs and 12 more; checks crates/offrig-app/src/main.rs, crates/offrig-cli/src/main.rs and crates/offrig-core/src/lib.rs.
2. **offrig** (a command built from crates/offrig-cli, which nothing ships). Runs crates/offrig-cli/src/main.rs.
3. **offrig-app** (a command built from crates/offrig-app, which nothing ships). Runs crates/offrig-app/src/main.rs.

## What happens through ci

1. The workflow runs crates/offrig-app/src/app.rs in offrig-app and 14 files in offrig-core; it checks crates/offrig-app/src/main.rs in offrig-app, crates/offrig-cli/src/main.rs in offrig-cli and crates/offrig-core/src/lib.rs in offrig-core.

## Who reads the results

ci writes nothing this map can see.

## The other doors

**offrig** (a command built from crates/offrig-cli, which nothing ships) runs crates/offrig-cli/src/main.rs and reaches offrig-core.

**offrig-app** (a command built from crates/offrig-app, which nothing ships) runs crates/offrig-app/src/main.rs and reaches offrig-core.

## What breaks what

- **offrig-core** is imported by 2 parts (offrig-app, offrig-cli) and sits on the path of 3 doors.
- **offrig-app** is imported by no other part and sits on the path of 2 doors.
- **offrig-cli** is imported by no other part and sits on the path of 2 doors.

## What tends to change together

No two source files changed together often enough to name.

Window: 180 days; a pair counts from 3 shared commits, since the window holds fewer than 30 qualifying commits.

## What no test touches

- **offrig-cli** is imported by no test.

offrig-app is tested only by the unit tests in its own files.

offrig-core is tested only by the unit tests in its own files.

## Written but never read

No place this map can see is written, so none goes unread.

## Helpers that look duplicated

No two parts export a helper that looks alike.

## Generated, never hand-edited

Nothing in this repository writes to a tracked place this map can see.

## Hand-authored

People write root; 4 writes with paths built at run time may land here.

## Where to start

crates/offrig-cli/src/main.rs → crates/offrig-core/src/config.rs → crates/offrig-core/src/error.rs

Read those in order to follow one run of offrig end to end. This path follows offrig (a command built from crates/offrig-cli, which nothing ships) from its entry, since ci runs only tests and checks.

## What this map cannot see

- 4 writes use paths built at run time and are not named here.
- 1 write and 7 reads go to a path their caller passes, not to this repository.
- Statistics confidence is low: fewer than 30 qualifying commits in the window, and fewer than 25 source files reach 10 revisions.

Regenerate with `npx --yes @dogfood-lab/atlas map`.
