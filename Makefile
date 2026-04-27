.PHONY: build check fmt lint test lint-test run

build:
	cargo build --release --package loom-cli

check:
	cargo check --workspace --all-targets

fmt:
	cargo fmt --all

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

lint-test:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace

run:
	cargo run --package loom-cli -- $(ARGS)
