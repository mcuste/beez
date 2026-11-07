# Sandbox

A sandbox is a set of operating-system rules that limit what a program can
do. Every Beez task runs inside one, so a coding agent or a command cannot
read your secrets, change files outside the project, or talk to hosts you did
not allow. Beez applies the sandbox itself, so every harness and every command
gets the same rules.

- macOS uses Seatbelt through `sandbox-exec`. Nothing to install.
- Linux uses bubblewrap namespaces and Landlock. Install the `bwrap` package.
  Program restrictions need kernel 5.13 or later.

## Default rules

Network:

- All connections are denied unless the host is allowed.
- Beez runs a local HTTP proxy and a SOCKS5 proxy that only connect to
  allowed hosts. It points `HTTP_PROXY`, `HTTPS_PROXY`, and `ALL_PROXY` at
  them.
- A host without a port allows port 443 through a tunnel and port 80 for
  plain HTTP. Name the port to allow another one, such as `github.com:22`.
- A tunnel to port 443 must start TLS for the host it was opened for. The
  proxy reads the server name in the first TLS message and refuses a tunnel
  that names another host, names none, or hides the name with Encrypted
  Client Hello. Without this check, a task could open a tunnel to an allowed
  host on a shared CDN address and then ask that address for any other site
  it serves.
- The proxy writes the `Host` header of a plain HTTP request itself, so the
  request reaches the host the proxy checked.
- An allowed host that resolves to a loopback, link-local, or cloud metadata
  address is refused unless `localhost: true`. This includes IPv6 addresses
  that carry such an IPv4 address.
- Each harness gets the hosts it needs to run, such as `api.anthropic.com`
  for `claude`.

Filesystem:

- Reads are allowed, except credential stores such as `~/.ssh`, `~/.aws`, and
  `~/.git-credentials`, shell history, and password stores.
- Writes are denied, except inside the working directory, the temporary
  directory, and the harness's own state directory.
- Files a task could use to run code later stay read-only: `.git/hooks`,
  `.github/workflows`, `.envrc`, `.vscode`, `.claude`, and `.mcp.json`.
  `.beez` is read-only too, so a task cannot rewrite the log of its own run.

Local services:

- A task cannot reach programs on the host through Unix sockets, such as the
  Docker daemon, an SSH or GPG agent, the D-Bus buses, or a terminal
  multiplexer. Beez removes `SSH_AUTH_SOCK`, `GPG_AGENT_INFO`, and the D-Bus
  addresses from the environment.
- On macOS a task may call only the system services that command-line tools
  need, such as user lookups, preferences, logging, and certificate checks.
  It cannot ask another service to act for it, such as opening a URL in the
  browser or reading the clipboard.
- On Linux `/tmp` and `/run` are empty and private to each task, because they
  hold the host's sockets. Files a task writes in `/tmp` are gone when it
  ends. The program Beez starts stays visible even when it lives in `/tmp`.

Programs:

- Without an `executables` section, any program may run.
- With one, the groups `coreutils`, `text`, `git`, and `net` are on by
  default. The harness program, `sh`, `bash`, `zsh`, `env`, `node`, `bun`,
  and `rg` always may run.

## Allowing more

A `sandbox` section adds permissions for the whole workflow or for one task. A
task adds to the workflow rules and never removes any. A task that needs no
sandbox sets `sandbox: false`.

```yaml
sandbox:
  network:
    groups: [github, crates]
    allow: ["registry.internal:443"]
  filesystem:
    read_deny: ["~/.config/secrets"]
    write_allow: ["/data"]
tasks:
  - id: review
    harness: claude
    prompt: review the repository
  - id: build
    command: [cargo, build]
    sandbox:
      network:
        defaults: false
        localhost: true
      executables:
        groups: [rust]
        disable: [net]
        allow: ["~/.local/share/mise"]
  - id: publish
    command: [./publish.sh]
    sandbox: false
```

| Section       | Fields                                                |
| ------------- | ----------------------------------------------------- |
| `network`     | `defaults`, `groups`, `disable`, `allow`, `localhost` |
| `filesystem`  | `defaults`, `read_deny`, `write_allow`, `write_deny`  |
| `executables` | `defaults`, `groups`, `disable`, `allow`              |

- `defaults: true` is the default and brings in the rules above. Set it to
  `false` to keep only the groups and rules the manifest lists. A task that
  leaves the field out keeps the choice of the workflow, and so does
  `localhost`. Hosts a harness needs to run always apply.
- `allow` in `network` takes hosts such as `github.com`, `*.npmjs.org`, or
  `pypi.org:443`. A host without a port allows 443 for TLS and 80 for plain
  HTTP.
- Paths accept `~` for the home directory. Relative paths start at the
  working directory.
- `allow` in `executables` takes program names, files, or directories. Tool
  version managers that use shims, such as mise, need their install directory
  here.

From the command line:

```sh
beez run codex --allow-domain github.com --allow-write /data "fix the build"
```

`--allow-domain` and `--allow-write` add to the default rules. `--no-sandbox`
runs without a sandbox and takes neither.

## Groups

Network groups:

| Group                                                   | Hosts                                                                            | Default for     |
| ------------------------------------------------------- | -------------------------------------------------------------------------------- | --------------- |
| `anthropic`                                             | `api.anthropic.com`, `platform.claude.com`, `claude.ai`                          | claude, pi, omp |
| `openai`                                                | `api.openai.com`, `chatgpt.com`, `auth.openai.com`                               | codex, pi, omp  |
| `google`                                                | `generativelanguage.googleapis.com`, `oauth2.googleapis.com`                     | pi, omp         |
| `openrouter`                                            | `openrouter.ai`                                                                  | pi, omp         |
| `bedrock`, `vertex`, `azure-openai`                     | cloud provider endpoints                                                         | off             |
| `mistral`, `deepseek`, `xai`, `groq`, `ollama`          | other model providers                                                            | off             |
| `huggingface`                                           | `huggingface.co` and its content hosts                                           | off             |
| `claude-optional`                                       | Claude Code updates, plugins, and documentation                                  | off             |
| `claude-telemetry`                                      | Claude Code operational telemetry                                                | off             |
| `github`                                                | `github.com`, `api.github.com`, `codeload.github.com`, `*.githubusercontent.com` | off             |
| `gitlab`, `bitbucket`                                   | forge web and API hosts                                                          | off             |
| `npm`, `pypi`, `crates`, `go`, `rubygems`, `maven`, `nuget`, `homebrew` | package registries                                               | off             |
| `docker-hub`, `container-registry`                      | container registries                                                             | off             |
| `hashicorp`                                             | Terraform registry and releases                                                  | off             |
| `playwright`                                            | Playwright browser downloads                                                     | off             |

Program groups:

| Group                                                          | Programs                                                  |
| -------------------------------------------------------------- | --------------------------------------------------------- |
| `coreutils`, `text`, `git`, `net`                              | Basic tools. On by default.                               |
| `node`, `python`, `rust`, `go`, `jvm`, `ruby`, `dotnet`, `zig` | One language toolchain and its cache directories.         |
| `build`                                                        | `make` and `cmake`                                        |
| `container`                                                    | `docker` and `kubectl`                                    |
| `iac`                                                          | `terraform` and `ansible`                                 |
| `process`                                                      | `ps` and `lsof`                                           |

## Limits

- Allowing a host allows every path on it.
- A process that can read a secret and reach one allowed host can leak it.
- The proxy reads the server name only on port 443. A tunnel to a port that a
  rule names, such as `example.com:8443`, carries any bytes.
- On Linux, sockets outside `/tmp`, `/run`, and the denied credential
  directories stay reachable.
- Once an interpreter such as `node` is allowed, the program list limits
  tooling, not capability.
- On Linux the program list also names the dynamic loader, which every linked
  program needs. The loader can start any file it may read.
- On Linux a write deny needs its parent directory to exist when the run
  starts. The parent is what stops a task from renaming the directory and
  putting a writable one in its place.
- Sandboxes do not nest. A task that starts its own sandbox, or that already
  runs inside one, needs `sandbox: false`.

## Reporting a vulnerability

Report a vulnerability through the repository's
[private security advisory form](https://github.com/mcuste/beez/security/advisories/new).
Include the affected version, the platform, the steps to reproduce, and the
rule that was crossed. Do not open a public issue until a fix is available.
