# Beez

[![CI](https://github.com/mcuste/beez/actions/workflows/ci.yml/badge.svg)](https://github.com/mcuste/beez/actions/workflows/ci.yml)

Beez runs coding agents and shell commands as tasks, inside a sandbox, on your
own machine. You describe the tasks in one YAML file. Beez runs them in the
right order, shows their output, saves a log of every run, and can start the
same file on a schedule.

A coding agent is a program such as Claude Code or Codex. You give it a
written instruction, called a prompt, and it reads and changes files and runs
commands to carry it out. Beez calls these programs harnesses. A sandbox is a
set of operating-system rules that limit what a program can read, write, run,
and connect to. Beez puts every task in a sandbox by default, so an agent
cannot reach your secrets or send data to hosts you did not allow.

## Requirements

- macOS or Linux.
- On Linux, the `bwrap` package (bubblewrap). Kernel 5.13 or later for the
  program restrictions.
- One or more harnesses on your `PATH`: `claude`, `codex`, `pi`, or `omp`.
  Command tasks need no harness.

## Install

With Rust installed:

```sh
cargo install --git https://github.com/mcuste/beez --locked
```

Or download a binary from the
[releases page](https://github.com/mcuste/beez/releases) and put `beez` on
your `PATH`. Each release includes a `SHA256SUMS` file to check the download.
A Homebrew tap will follow.

## Quick start

Run one prompt in a sandbox:

```sh
beez run claude "inspect the repository and list the problems you find"
```

Run a workflow. Save this as `review.yaml`:

```yaml
tasks:
  - id: changed
    command: [git, diff, --name-only, main]

  - id: review
    depends_on: [changed]
    harness: claude
    prompt: |
      Review these files and report problems:
      {{ tasks.changed.stdout }}

  - id: test
    depends_on: [review]
    command: [cargo, test, --workspace]
```

```sh
beez run workflow review.yaml
```

Beez runs `changed` first, gives its output to the `review` prompt, and runs
`test` after the review. Tasks that do not depend on each other run at the
same time. The output of each task is labeled with its ID, and the whole run
is saved under `.beez/` next to your repository.

Run the same file every night at 03:00 UTC by adding a schedule and starting
the daemon:

```yaml
schedule:
  cron: "0 3 * * *"
```

```sh
beez schedule add review.yaml
beez daemon start
```

## Documentation

- [Workflows](docs/workflows.md): the YAML file, harnesses, and task outputs
- [Output and logs](docs/output.md): what Beez prints and what it saves
- [Sandbox](docs/sandbox.md): what a task may do, and how to allow more
- [Schedules](docs/schedules.md): running workflows on a timer with the daemon
- [Development](docs/development.md): layout, commands, tests, and releases
- [Contributing](CONTRIBUTING.md)
- [Changelog](CHANGELOG.md)

## License

[MIT](LICENSE)
