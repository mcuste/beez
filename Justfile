default: verify

build:
    cargo build --release --package loom-cli

check:
    cargo check --workspace --all-targets

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

deps:
    cargo deny check

machete:
    cargo machete

test:
    cargo test --workspace

test-integration:
    cargo test --workspace --test version

verify:
    just fmt-check
    just lint
    just deps
    just machete
    just test

run *args:
    cargo run --package loom-cli -- {{ args }}
