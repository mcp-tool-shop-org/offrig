# offrig

Runs big models on RunPod and wires them into Zed through an SSH tunnel, so they never
run on the local GPU. Rust workspace: `offrig-core` (all logic), `offrig-cli` (`offrig`),
`offrig-app` (`offrig-app`, egui).

## Verify

This is a desktop app and CLI, not a web app: never use preview or browser tools.

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
npx --yes @dogfood-lab/atlas@1.24.0 check      # regenerate with `map` after structural changes
```

## Rules

- Rust guidance comes from the readouts Rust KB (`E:/AI/readouts/rust-knowledge`, start
  at `catalog/README.md`). Lanes used here: errors-panics, concurrency-async,
  testing-tooling, cargo-modules, crate-licences, ci-reproducible-builds.
- Library errors are the `thiserror` enum in `error.rs`; binaries use `anyhow`. Input
  gets a `Result`, never `expect`.
- The guarantee is the product. Any change to the tunnel, ports, pod spec or Zed wiring
  must keep every guard check meaningful, and a new way for a model to reach the local
  GPU gets a new guard check with a test.
- offrig touches only pods of its own lane. The plain lane (CLI, app, Zed) is alias
  `offrig`, port 11435, pods `offrig-<profile>`; each project's side-car has its own lane
  in `lanes.toml` (alias `offrig-<tag>`, a port from 11500 up, pods
  `offrig-<tag>-<profile>`). A lane matches, lists and stops only its own pod names.
  Never stop, edit or delete another lane's pod, the plain lane's from a project lane,
  or any other pod on the account (other studio work runs there). No lane may use 11434.
- Tests never touch the real config dir, `~/.ssh/config`, RunPod or a real process: set
  `OFFRIG_CONFIG_DIR` to a temp dir, use the mock RunPod, and inject `tunnel::Processes`.
- A live test spends money. Budget the first run of a changed pod path as a bug-finding
  run, check the runway first, and terminate the pod when done. Read pod state from
  `publicIp` then `portMappings` then SSH; give a pod 10-15 minutes before judging it.
- Verify any new image tag on Docker Hub before using it: a missing tag looks exactly
  like a dead host.
- Never put a process pattern inside an ssh command that kills by pattern; it matches
  the ssh session itself.
- `Cargo.lock` is committed; CI builds with `--locked`.
