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

## Output

Workflow tasks run at the same time, so Loom labels their output. `--output`
picks the form.

`stream` is the default. It prints every line as it arrives, behind the task
ID, and keeps one spinner per running task at the bottom of the terminal. When
a task ends, its spinner gives way to one status line, so the finished task
stays in the log. A solid bar marks a line the task wrote on standard output,
and a dashed bar marks standard error.

```
api         │ GET /users?page=1 200
web         │ bundling chunk 1 of 7
api         ┊ warning: slow query took 412ms
Finished  worker_pool in 4.1s (ok)
web         │ bundling chunk 7 of 7
Finished  web in 7.2s (ok)
Summary   3 passed in 10.3s
```

`grouped` collects each task and prints it when the task ends, so the output of
two tasks never mixes. A status line opens and closes each task, and the output
between them is indented.

```
Running   api
Running   web
    claimed job 881
    claimed job 882
Finished  worker_pool in 4.1s (ok)
    bundling chunk 1 of 7
Finished  web in 7.2s (ok)
Summary   3 passed in 10.3s
```

Grouped holds a task's output until the task ends, so a long task shows nothing
while it runs, and a run that is killed loses what it had collected. Stream has
already printed every line, so prefer it in CI.

A workflow with one task relays its streams unchanged, because there is nothing
to tell apart. `loom run command` and the harness commands do the same, so use
one of those when a later step needs the exact bytes of a task.

Loom keeps its own lines apart from the output of the tasks in two ways. Task
output goes to standard output, and every line Loom writes itself goes to
standard error. Loom's lines start with a status word in the first column, and
only task output is indented or prefixed.

```sh
loom run workflow build.yaml > tasks.log 2> loom.log
```

Loom never restyles the bytes of a task, so the colours a task chose reach the
terminal as it wrote them. Sandbox notes take the status word form, so a denied
connection never reads as something a task printed.

```
Running   network
Sandbox   denied connection to example.com:443
Finished  network in 0.1s (ok)
```

The spinners draw on standard error. When standard error is not a terminal they
draw nothing, and every line still arrives.

`--timestamps` prefixes every line Loom writes with a UTC date and time. Local
time needs the time zone database, so Loom reports UTC and marks it with `Z`.

```
2026-09-09T15:16:37.775Z api         │ GET /users?page=1 200
2026-09-09T15:16:40.881Z api         ┊ warning: slow query took 412ms
2026-09-09T15:16:41.882Z Finished  worker_pool in 4.1s (ok)
```

`--timestamps=elapsed` prefixes the time since the run started instead.

```
    0.0s api         │ GET /users?page=1 200
    3.1s api         ┊ warning: slow query took 412ms
    4.1s Finished  worker_pool in 4.1s (ok)
```

A value needs an equals sign, because the bare flag would otherwise take the
path of the manifest as its value. In grouped mode a line keeps the time it
arrived, not the time its block was written, so a stamp always says when the
task wrote the line.

`--color auto|always|never` controls Loom's own colours. Loom also follows
`NO_COLOR`, `CLICOLOR_FORCE`, and `TERM=dumb`.

## Log artifacts

Every run also writes its output to disk, so a finished run stays open to
inspection. Loom keeps the artifacts in `.loom` in the repository root, or in
the working directory outside a repository. A `.loom` that already exists wins
over both, so runs from a subdirectory join the runs already there. The
directory ignores itself in Git, so no repository needs a change for it.

Each run gets its own directory, named after the time it started and the
process that ran it. The names sort by time, and `latest` points at the newest
run.

```
.loom/
  latest -> runs/20260909T164512815Z-70632
  runs/
    20260909T164512815Z-70632/
      run.json
      run.log
      tasks/
        api.stdout
        api.stderr
        web.stdout
        web.stderr
```

`run.log` holds the whole run in one file: every line of every task, behind its
task ID and its stream mark, and every line Loom wrote itself. Each line
carries the UTC time it arrived. Loom removes the colours a task chose, so the
file stays readable.

```
2026-09-09T16:45:12.818Z Running   api
2026-09-09T16:45:12.868Z api         │ GET /users?page=1 200
2026-09-09T16:45:16.925Z api         ┊ warning: slow query took 412ms
2026-09-09T16:45:23.079Z Finished  api in 10.3s (ok)
2026-09-09T16:45:23.080Z Summary   3 passed in 10.3s
```

`tasks/<id>.stdout` and `tasks/<id>.stderr` keep the bytes of one stream of one
task, as the task wrote them. Use them when a later step needs the exact output
of a task.

`run.json` records the run itself: the arguments, the working directory, the
manifest, the start and end times, Loom's exit status, and one entry per task
with its dependencies, its request, its state, its exit status and its
duration.

Loom names the directory of a run before its tasks start.

```
Logging   .loom/runs/20260909T164512815Z-70632
Running   api
```

A run of one task relays the bytes of that task, so Loom writes no line of its
own there. The artifacts still hold the whole run.

`--log-dir <PATH>` writes the artifacts somewhere else, and `LOOM_LOG_DIR` does
the same through the environment. `--no-log` writes none. A problem with the
artifacts never stops a run. Loom reports it once as a warning and carries on.

Loom keeps every run. Remove old runs yourself when the directory grows too
large.

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
`.git/hooks`, `.claude`, and `.mcp.json`, stay read-only, and so does `.loom`,
so a task cannot rewrite the log of its own run.

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
