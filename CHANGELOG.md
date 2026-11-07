# Changelog

All notable changes to this project are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project uses [semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

- A tunnel to port 443 must start TLS for the host it was opened for. A task
  can no longer open a tunnel to an allowed host on a shared CDN address and
  then reach another site on that address.
- Plain HTTP requests go to the host the proxy checked, whatever `Host`
  header the task sent.
- Cloud metadata addresses outside the link-local range, and IPv6 addresses
  that carry a local IPv4 address, count as local.
- On Linux each task gets a private `/tmp` and `/run`, so it cannot reach the
  host's Unix sockets, such as the D-Bus buses or the Docker socket.
- On macOS a task may call only a short list of system services, so it cannot
  have Launch Services open a URL or read the clipboard.
- `SSH_AUTH_SOCK`, `GPG_AGENT_INFO`, and the D-Bus addresses no longer reach a
  sandboxed task.

### Changed

- A host rule without a port allows port 443 through a tunnel and port 80 for
  plain HTTP, instead of every port. Name the port to allow another one.
- On Linux files a task writes in `/tmp` are private to the task.

## [0.1.0] - 2026-09-23

### Added

- First release of `beez`. It runs coding agents (`claude`, `codex`, `pi`,
  `omp`) and commands in an operating-system sandbox, alone or as workflows.
- Workflows run tasks in dependency order and can pass one task's output to
  another.
- `beez schedule` and `beez daemon` run workflows on a cron schedule.
- Every run is saved under `.beez`.
- Install with `brew install mcuste/tap/beez` or `cargo install beez --locked`.
