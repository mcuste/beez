# Development

## Layout

Loom is a Cargo workspace. One binary, `loom`, comes from `crates/loom-cli`.
The other crates are libraries it uses. No crate is published to crates.io.

| Path                          | Contents                                                          |
| ----------------------------- | ----------------------------------------------------------------- |
| `crates/loom-cli`             | The `loom` command: argument parsing, terminal output, reports    |
| `crates/loom-core`            | Task and workflow types, dependency graph, output placeholders    |
| `crates/loom-manifest`        | Reads and validates YAML and JSON manifests                       |
| `crates/loom-policy`          | Sandbox policy: paths, hosts, programs, groups, harness defaults  |
| `crates/loom-sandbox`         | Seatbelt on macOS, bubblewrap and Landlock on Linux, the proxies  |
| `crates/loom-process`         | Starts harnesses and commands as processes                        |
| `crates/loom-runner`          | Runs the tasks of a workflow in dependency order                  |
| `crates/loom-record`          | Writes the files of a run under `.loom`                           |
| `crates/loom-schedule`        | Cron expressions and one-time instants                            |
| `crates/loom-daemon`          | The background process that fires scheduled jobs                  |
| `crates/loom-test-support`    | Helpers shared by the integration tests                           |
| `scripts/check-version.py`    | Checks the version, the changelog, and a release tag              |
| `scripts/release.py`          | Prepares, verifies, commits, tags, and optionally pushes a release|
| `scripts/release-notes.py`    | Extracts the changelog section for the GitHub release             |
| `.github/workflows/ci.yml`    | Verification gate for pull requests and `main`                    |
| `.github/workflows/release.yml` | Builds and publishes release binaries for a `v*` tag            |

## Setup

Install Rust 1.97 or later. `rust-toolchain.toml` selects the stable channel
with `clippy` and `rustfmt`. Then install the tools the gate uses:

```sh
cargo install cargo-deny --version 0.20.2 --locked
cargo install cargo-machete --version 0.9.1 --locked
cargo install just --version 1.58.0 --locked
```

On Linux, install `bwrap` (bubblewrap). The sandbox tests need it.

## Commands

| Command                 | Purpose                                                            |
| ----------------------- | ------------------------------------------------------------------ |
| `just format`           | Format all Rust code                                               |
| `just format-check`     | Check formatting without changing files                            |
| `just clippy`           | Run Clippy on every target with warnings denied                    |
| `just cargo-check`      | Type-check the locked workspace                                    |
| `just build`            | Build the locked workspace                                         |
| `just test`             | Run unit and integration tests                                     |
| `just test-integration` | Run only the integration test targets                              |
| `just test-contract`    | Run the harness contract tests. Needs every harness installed.     |
| `just deny`             | Check advisories, licenses, sources, and banned dependencies       |
| `just machete`          | Find unused dependencies                                           |
| `just check`            | Run every static, build, and dependency check                      |
| `just verify`           | Run `just check` and the full test suite                           |
| `just install`          | Install `loom` from the working tree                               |
| `just run <args>`       | Run `loom` from the working tree                                   |
| `just release <version>`| Prepare a release                                                  |

CI runs `just verify`. Run the same command locally before opening a pull
request.

## Testing

Unit tests live next to the code they test. Integration tests live in the
`tests` directory of a crate:

| Test                                     | Covers                                                   |
| ---------------------------------------- | -------------------------------------------------------- |
| `loom-cli/tests`                         | The built binary: workflows, logs, sandbox, daemon, version |
| `loom-runner/tests/runner.rs`            | Task order, failure handling, and output placeholders    |
| `loom-process/tests/process_runner.rs`   | Process start, streams, and exit status                  |
| `loom-process/tests/harness_contract.rs` | The flags each real harness accepts. Opt-in.             |
| `loom-manifest/tests/load.rs`            | Manifest parsing and validation errors                   |
| `loom-sandbox/tests/proxy.rs`            | The HTTP and SOCKS5 proxies                              |

Tests use a fake harness from `loom-test-support` instead of a real one, so
they need no account and no network. Rules:

- Do not depend on the developer's home directory, credentials, or installed
  harnesses.
- Use a temporary directory for filesystem behavior.
- Do not wait with a fixed delay. Make a fake program wait for a file the test
  creates.
- Assert on the result or the state, not on private call order.

For a behavior change:

1. Add a test that fails for the wrong result.
2. Make the smallest source change that fixes it.
3. Run the narrow test while iterating.
4. Run `just verify` before committing.

## Continuous integration

`ci.yml` runs on pull requests and pushes to `main` with read-only
permissions. The quality job runs `just verify` on Ubuntu and macOS, because
the sandbox differs per platform. On Ubuntu it installs bubblewrap and turns
off the AppArmor rule that stops bubblewrap from bringing up loopback in a
user namespace. A second job runs `scripts/check-version.py`.

Dependabot proposes monthly updates for Cargo and GitHub Actions. Updates must
pass the same gate as source changes.

## Release

A tag named `v<version>` starts `release.yml`. The workflow checks that the
tag matches `Cargo.toml` and that `CHANGELOG.md` has a section for the
version. It then builds `loom` for four targets on native runners:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

Each target becomes `loom-v<version>-<target>.tar.gz`. The publish job writes
`SHA256SUMS`, attaches the archives, and uses the changelog section as the
release body. Nothing goes to crates.io. Users install with
`cargo install --git`, from the release archives, or later from a Homebrew tap
that points at these archives and checksums.

Prepare a release from a clean `main`:

```sh
just release <version>
```

The command needs a three-part version. It refuses a dirty tree, another
branch, an existing tag, a lower version, and an empty `Unreleased` section.
It then:

1. Sets the version under `[workspace.package]` in `Cargo.toml`.
2. Turns `Unreleased` into a dated section and adds a new empty `Unreleased`.
3. Updates `Cargo.lock` and runs `just verify`. It restores the files if that
   fails.
4. Commits `chore: release <version>` and tags `v<version>`.

Pushing is a separate step because it makes the release public:

```sh
git push origin main
git push origin v<version>
```

`just release <version> --push` does both pushes after the tag. Check the
GitHub release assets and `SHA256SUMS` after the workflow completes.
