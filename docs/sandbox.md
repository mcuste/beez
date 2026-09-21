# Sandbox

A sandbox is a set of operating-system rules that limit what a program can
do. Every Loom task runs inside one, so a coding agent or a command cannot
read your secrets, change files outside the project, or talk to hosts you did
not allow. Loom applies the sandbox itself, so every harness and every command
gets the same rules.

- macOS uses Seatbelt through `sandbox-exec`. Nothing to install.
- Linux uses bubblewrap namespaces and Landlock. Install the `bwrap` package.
  Program restrictions need kernel 5.13 or later.

## Default rules

Network:

- All connections are denied unless the host is allowed.
- Loom runs a local HTTP proxy and a SOCKS5 proxy that only connect to
  allowed hosts. It points `HTTP_PROXY`, `HTTPS_PROXY`, and `ALL_PROXY` at
  them.
- An allowed host that resolves to a loopback or link-local address, such as a
  cloud metadata service, is refused unless `localhost: true`.
- Each harness gets the hosts it needs to run, such as `api.anthropic.com`
  for `claude`.

Filesystem:

- Reads are allowed, except credential stores such as `~/.ssh`, `~/.aws`, and
  `~/.git-credentials`, shell history, and password stores.
- Writes are denied, except inside the working directory, the temporary
  directory, and the harness's own state directory.
- Files a task could use to run code later stay read-only: `.git/hooks`,
  `.github/workflows`, `.envrc`, `.vscode`, `.claude`, and `.mcp.json`.
  `.loom` is read-only too, so a task cannot rewrite the log of its own run.

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
  `pypi.org:443`.
- Paths accept `~` for the home directory. Relative paths start at the
  working directory.
- `allow` in `executables` takes program names, files, or directories. Tool
  version managers that use shims, such as mise, need their install directory
  here.

From the command line:

```sh
loom run codex --allow-domain github.com --allow-write /data "fix the build"
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
[private security advisory form](https://github.com/mcuste/loom/security/advisories/new).
Include the affected version, the platform, the steps to reproduce, and the
rule that was crossed. Do not open a public issue until a fix is available.
