# Changelog

All notable changes to this project are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project uses [semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-09-23

### Added

- First release of `beez`. It runs coding agents (`claude`, `codex`, `pi`,
  `omp`) and commands in an operating-system sandbox, alone or as workflows.
- Workflows run tasks in dependency order and can pass one task's output to
  another.
- `beez schedule` and `beez daemon` run workflows on a cron schedule.
- Every run is saved under `.beez`.
- Install with `brew install mcuste/tap/beez` or `cargo install beez --locked`.
