set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

default:
    @just --list

format:
    cargo fmt --all

format-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

cargo-check:
    cargo check --workspace --all-targets --locked

build:
    cargo build --workspace --locked

test:
    cargo test --workspace --locked

test-integration:
    cargo test --workspace --locked --test '*'

# Needs pi, omp, claude, and codex installed and signed in.
test-contract:
    cargo test --package loom-process --features harness-contract --test harness_contract -- --nocapture

deny:
    cargo deny check

machete:
    cargo machete

check: format-check clippy cargo-check build deny machete

verify: check test

install:
    cargo install --path crates/loom-cli --locked

run *args:
    cargo run --package loom-cli -- "$@"

release version *args:
    python3 scripts/release.py "$@"
