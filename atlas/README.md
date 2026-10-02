# offrig: how it works

Mapped at 2026-10-02 from commit 7fe6959 by Atlas 1.24.0.

## What this is

Runs big models on RunPod and wires them into Zed through an SSH tunnel, so they never run on the local GPU. A Rust core library with a CLI and an egui desktop app on top. (written by a person)

4 parts, mostly Rust (19 files). Work enters through 2 doors; offrig and offrig-app each reach 2 parts, and offrig is followed because it comes first by name. offrig and offrig-app are commands built from crates/offrig-app and crates/offrig-cli (nothing ships them).

## What changed since the last map

This is the first map.

## What comes in

1. **offrig** (a command built from crates/offrig-cli, which nothing ships). Runs crates/offrig-cli/src/main.rs.
2. **offrig-app** (a command built from crates/offrig-app, which nothing ships). Runs crates/offrig-app/src/main.rs.

## What happens through offrig

1. The command runs crates/offrig-cli/src/main.rs in offrig-cli.
   1. Inside crates/offrig-cli/src/main.rs, `main` does, in order:
      1. `config.rs` (offrig-core, 3 steps)
      2. `new` (Session)
      3. `now_unix`
      4. `session_cost`
      5. `new` (Session)
      6. `chain`
      7. `api_key_env_name`
      8. `settings_path`
      9. `gather`
      10. `evaluate`
      11. `new` (IdleTracker)
      12. `gpu_stats`, and 6 more
   2. **`gather`** (offrig-core) runs, in order: `read_settings`, `read_provider`, `new` (Ollama), `local_ollama_models` and `pod_tags_json`.
   3. **`gpu_stats`** (offrig-core) runs, in order: `run_with_timeout` and `Ssh` (Error).
   4. **`pull_start`** (offrig-core) runs, in order: `Ollama` (Error), `run_with_timeout`, `Ssh` (Error) and `Ollama` (Error).
   5. **`pull_state`** (offrig-core) runs, in order: `Ollama` (Error), `run_with_timeout` and `Ssh` (Error).
   6. **`gather`** (offrig-core) runs, in order: `read_settings`, `read_provider`, `new` (Ollama), `local_ollama_models` and `pod_tags_json`.
2. That reaches offrig-core (14 files).

## Who reads the results

offrig writes nothing this map can see.

## The other doors

**offrig-app** (a command built from crates/offrig-app, which nothing ships) runs crates/offrig-app/src/main.rs and reaches offrig-core.

## What breaks what

- **offrig-core** is imported by 2 parts (offrig-app, offrig-cli) and sits on the path of 2 doors.

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

People write the repository root; 3 writes with paths built at run time may land here.

## Where to start

crates/offrig-cli/src/main.rs → crates/offrig-core/src/config.rs → crates/offrig-core/src/error.rs

Read those in order to follow one run of offrig end to end.

## What this map cannot see

- 3 writes use paths built at run time and are not named here.
- 1 write and 4 reads go to a path their caller passes, not to this repository.
- Statistics confidence is low: fewer than 30 qualifying commits in the window, and fewer than 20 source files reach 10 revisions.

Regenerate with `npx --yes @dogfood-lab/atlas map`.
