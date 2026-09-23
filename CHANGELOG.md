# Changelog

All notable changes to this project are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project uses [semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `beez run workflow` runs the tasks of a YAML or JSON manifest in dependency
  order, with independent tasks in parallel.
- `beez run claude`, `codex`, `pi`, and `omp` run one prompt. `model` and
  `effort` are passed with each harness's own flags.
- `beez run command` runs a program without a shell.
- Every task runs in an operating-system sandbox: Seatbelt on macOS,
  bubblewrap and Landlock on Linux. The sandbox limits network hosts, file
  reads and writes, and the programs a task may run. A manifest widens the
  rules per workflow or per task, or opts out with `sandbox: false`.
- Task output is labeled by task ID, streamed or grouped, with optional
  timestamps and colour control.
- Every run is saved under `.beez` with a combined log, the raw streams of
  each task, and a `run.json` record.
- A prompt or command argument can include the output of a dependency with
  `{{ tasks.<id>.stdout }}` and `{{ tasks.<id>.stderr }}`.
- Manifests can carry a `schedule`. `beez schedule` watches manifests and
  `beez daemon` fires them on cron expressions or at one instant.
