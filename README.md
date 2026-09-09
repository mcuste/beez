# Loom

Loom is a local task orchestrator for headless agent harnesses and Bash tasks.

Run a workflow with:

```sh
loom run workflow workflow.yaml
```

Workflow manifests support YAML and JSON. Each task defines either a harness
prompt or a literal command. Dependencies run before their dependents.

Loom supports four harnesses: `pi`, `omp`, `claude` for Claude Code, and `codex`.
Harness tasks accept an optional `model` and `effort`. Loom sends the effort with
each harness's own flag:

| Harness  | Program  | Non-interactive mode | Effort                              |
| -------- | -------- | -------------------- | ----------------------------------- |
| `pi`     | `pi`     | `--print`            | `--effort`                          |
| `omp`    | `omp`    | `--print`            | `--thinking`                        |
| `claude` | `claude` | `--print`            | `--effort`                          |
| `codex`  | `codex`  | `exec`               | `-c model_reasoning_effort=<level>` |

Codex refuses to start outside a trusted directory, so run Codex tasks from a Git
repository. Loom does not send `--skip-git-repo-check`, because that turns off a
Codex safety check.

```yaml
tasks:
  - id: inspect
    harness: claude
    prompt: inspect the repository
    model: opus
    effort: high

  - id: review
    depends_on: [inspect]
    harness: codex
    prompt: review the findings
    effort: high

  - id: test
    depends_on: [review]
    command: [cargo, test, --workspace]
```

Run a single harness prompt with the same options:

```sh
loom run claude --model opus --effort high "inspect the repository"
loom run codex --model gpt-5 --effort high "inspect the repository"
```

## Sandbox

A task can run inside an operating-system sandbox that limits the network,
the filesystem, and the programs the task may execute. Loom enforces the
sandbox itself, so every harness and every command task gets the same rules.

- macOS uses Seatbelt through `sandbox-exec`. Nothing to install.
- Linux uses bubblewrap namespaces and Landlock. Install the `bwrap` package.
  Executable restrictions need kernel 5.13 or later.

Network access is denied unless a host is allowed. Loom runs a local HTTP
proxy and a SOCKS5 proxy that only connect to allowed hosts, and points
`HTTP_PROXY`, `HTTPS_PROXY`, and `ALL_PROXY` at them. The proxies also refuse
an allowed host that resolves to a loopback or link-local address, such as the
cloud metadata service, unless `localhost: true`. Reads are allowed
except for credential stores such as `~/.ssh` and `~/.aws`. Writes are denied
except inside the working directory, the temporary directory, and the
harness's own state directory. Files a run could use to escape later, such as
`.git/hooks`, `.claude`, and `.mcp.json`, stay read-only.

Enable the sandbox for a whole workflow or for one task. A task can also opt
out with `sandbox: false`. Each section a task defines replaces the workflow's
section.

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

Every section has `defaults: true`. Set it to `false` to keep only the groups
and rules the task lists. Groups a harness needs to run at all, such as
`anthropic` for `claude`, always apply.

| Section       | Fields                                                |
| ------------- | ----------------------------------------------------- |
| `network`     | `defaults`, `groups`, `disable`, `allow`, `localhost` |
| `filesystem`  | `defaults`, `read_deny`, `write_allow`, `write_deny`  |
| `executables` | `defaults`, `groups`, `disable`, `allow`              |

`allow` in `network` takes hosts such as `github.com`, `*.npmjs.org`, or
`pypi.org:443`. Paths accept `~` for the home directory and relative paths for
the working directory. `allow` in `executables` takes program names, files, or
directories. Without an `executables` section, any program may run.

Domain groups:

| Group                                                   | Hosts                                                                            | Default for     |
| ------------------------------------------------------- | -------------------------------------------------------------------------------- | --------------- |
| `anthropic`                                             | `api.anthropic.com`, `platform.claude.com`, `claude.ai`                          | claude, pi, omp |
| `openai`                                                | `api.openai.com`, `chatgpt.com`, `auth.openai.com`                               | codex, pi, omp  |
| `google`                                                | `generativelanguage.googleapis.com`, `oauth2.googleapis.com`                     | pi, omp         |
| `openrouter`                                            | `openrouter.ai`                                                                  | pi, omp         |
| `bedrock`, `vertex`                                     | provider endpoints                                                               | off             |
| `claude-optional`                                       | Claude Code updates, plugins, and documentation                                  | off             |
| `claude-telemetry`                                      | Claude Code operational telemetry                                                | off             |
| `github`                                                | `github.com`, `api.github.com`, `codeload.github.com`, `*.githubusercontent.com` | off             |
| `npm`, `pypi`, `crates`, `go`, `homebrew`, `docker-hub` | package registries                                                               | off             |

Executable groups: `coreutils`, `text`, `git`, and `net` are on by default.
`node`, `python`, `rust`, and `go` add the toolchain and its cache
directories. The harness binary, `sh`, `bash`, `zsh`, `env`, `node`, `bun`,
and `rg` always may run. Tool version managers that use shims, such as mise,
need their install directory in `allow`.

Run a single sandboxed prompt or command from the command line:

```sh
loom run claude --sandbox "inspect the repository"
loom run codex --allow-domain github.com --allow-write /data "fix the build"
loom run command --sandbox cargo test
```

`--allow-domain` and `--allow-write` imply `--sandbox`.

Limits to keep in mind:

- Allowing a host allows every path on it.
- A process that reads a secret and reaches one allowed host can leak it.
- The executable list limits tooling rather than capability once an interpreter such as `node` is allowed.
- On Linux the executable list also names the dynamic loader, which every linked program needs, and the
  loader can start any file it may read.
- On Linux a write deny needs its parent directory to exist when the run starts, because the parent is
  what stops a task from renaming it and putting a writable directory in its place.
