# Contributing

## Before making a change

Keep each change focused on one observable result. Open an issue before a
large behavior or interface change, so the design is settled before the
implementation. Small fixes and documentation changes can go straight to a
pull request.

Do not commit build output, `.beez` directories, editor state, or planning
files.

## Development setup

Install Rust 1.97 or later and the tools the verification gate uses:

```sh
cargo install cargo-deny --version 0.20.2 --locked
cargo install cargo-machete --version 0.9.1 --locked
cargo install just --version 1.58.0 --locked
```

Run the complete gate before opening or updating a pull request:

```sh
just verify
```

`just verify` checks formatting, Clippy warnings, compilation, the build,
dependency policy, unused dependencies, and every test. See
[Development](docs/development.md) for the single commands and the test rules.

## Tests and changelog

Add or update a test when behavior changes. Test the public result, boundary,
or failure, not private details. Keep tests deterministic and independent of
the developer's machine, home directory, and installed harnesses.

Update `CHANGELOG.md` under `Unreleased` for user-visible features, fixes,
security changes, and breaking changes. Tooling-only, test-only, and
documentation-only changes need no entry.

## Commit convention

Use a short [Conventional Commits](https://www.conventionalcommits.org/)
subject:

```text
<type>(<optional-scope>): <imperative summary>
```

| Type       | Use                                                       |
| ---------- | --------------------------------------------------------- |
| `feat`     | New user-visible behavior                                 |
| `fix`      | Correct user-visible behavior                             |
| `perf`     | Improve measured performance without changing behavior    |
| `refactor` | Change implementation without changing behavior           |
| `test`     | Add or correct tests only                                 |
| `docs`     | Change documentation only                                 |
| `build`    | Change build tools, dependencies, or local project gates  |
| `ci`       | Change continuous integration or release automation       |
| `chore`    | Repository maintenance that fits no type above            |

Subject rules:

- Use lower case after the colon.
- Start with an imperative verb such as `add`, `fix`, `reject`, or `document`.
- Keep the subject at 72 characters or fewer.
- Do not end the subject with a period.
- Add a scope only when it makes the area clearer, such as `sandbox` or
  `daemon`.

Use the body for non-trivial commits. State what changed, why, and any
important limit or tradeoff. Wrap prose at 72 characters. Do not restate the
subject or describe the editing process.

Examples:

```text
feat(sandbox): deny writes to .git/hooks by default

A task that can write a hook can run code in the next Git command outside
the sandbox. Keep the directory read-only unless a manifest allows it.
```

```text
fix(daemon): keep a broken manifest in the job list

Mark the job broken and show the load error next to it, so a typo in a
manifest does not silently remove a schedule.
```

For a breaking change, add `!` before the colon and a `BREAKING CHANGE:`
footer:

```text
feat!: require bubblewrap 0.9 on Linux

BREAKING CHANGE: Older bubblewrap versions do not accept the flags the
Linux sandbox uses.
```

Each commit must build on its parent and contain one coherent change. Fold
typo, format, and review fixes into the commit that introduced them before
review. Keep independent behavior in independent commits.

Release commits use this exact subject:

```text
chore: release <version>
```

## Pull requests

A pull request explains the observable result, the reason for the change, and
the commands or manual scenario that verified it. Keep unrelated cleanup out
of the diff. Call out sandbox policy changes, platform-specific behavior,
changes to the files under `.beez` or `~/.beez`, and release changes.

A pull request is ready for review when CI passes, user-visible changes have
changelog entries, and the commits follow the convention above.
