#!/usr/bin/env bash
# Local gate: formatting, lints, tests, and a smoke run of both binaries.
# Run from anywhere; exits non-zero on the first failure. No network, no RunPod.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

step() { printf '\n==> %s\n' "$*"; }

step "cargo fmt --check"
cargo fmt --all -- --check

step "cargo clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings

step "cargo test"
cargo test --workspace --locked

step "smoke: offrig --help, offrig --version, offrig-mcp --help"
cargo run --quiet --locked -p offrig-cli -- --help > /dev/null
cargo run --quiet --locked -p offrig-cli -- --version
cargo run --quiet --locked -p offrig-mcp -- --help > /dev/null

printf '\nverify: all checks passed\n'
