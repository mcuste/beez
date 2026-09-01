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
