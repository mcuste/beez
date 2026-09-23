# Workflows

A workflow is a YAML or JSON file, called a manifest, with a list of tasks.
Run it with:

```sh
beez run workflow workflow.yaml
```

## Tasks

Each task has an `id` and either a `command` or a `harness` with a prompt.

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
    prompt_file: prompts/review.md
    effort: high

  - id: test
    depends_on: [review]
    command: [cargo, test, --workspace]
```

| Field         | Meaning                                                                 |
| ------------- | ----------------------------------------------------------------------- |
| `id`          | Name of the task. Labels its output and names its log files.            |
| `depends_on`  | Tasks that must finish first. Tasks without a dependency run together.  |
| `command`     | A program and its arguments as a list. Beez runs it without a shell.    |
| `harness`     | The coding agent to run: `claude`, `codex`, `pi`, or `omp`.             |
| `prompt`      | The instruction for the harness, written in the file.                   |
| `prompt_file` | A file with the instruction, relative to the manifest.                  |
| `model`       | Optional model name the harness accepts.                                |
| `effort`      | Optional reasoning effort the harness accepts.                          |
| `sandbox`     | Extra permissions for this task. See [Sandbox](sandbox.md).             |

## Harnesses

A harness is a coding agent program. Beez starts it in a mode that takes one
prompt, works without asking questions, and exits.

| Harness  | Program  | Non-interactive mode | Effort flag                         |
| -------- | -------- | -------------------- | ----------------------------------- |
| `claude` | `claude` | `--print`            | `--effort`                          |
| `codex`  | `codex`  | `exec`               | `-c model_reasoning_effort=<level>` |
| `pi`     | `pi`     | `--print`            | `--effort`                          |
| `omp`    | `omp`    | `--print`            | `--thinking`                        |

Codex refuses to start outside a Git repository. Beez does not pass
`--skip-git-repo-check`, because that turns off a Codex safety check.

Run one prompt without a manifest:

```sh
beez run claude --model opus --effort high "inspect the repository"
beez run codex --model gpt-5 --effort high "inspect the repository"
beez run command cargo test
```

## Task outputs

A task can use the output of a task it depends on. Write
`{{ tasks.<id>.stdout }}` or `{{ tasks.<id>.stderr }}` in a prompt, a prompt
file, or a command argument. Beez fills in the text when the task starts,
after the dependency has finished.

```yaml
tasks:
  - id: changed
    command: [git, diff, --name-only, main]

  - id: review
    depends_on: [changed]
    harness: claude
    prompt_file: prompts/review.md

  - id: notify
    depends_on: [review]
    command: [./notify.sh, "{{ tasks.review.stdout }}"]
```

```markdown
Review these files and report problems:

{{ tasks.changed.stdout }}
```

Rules:

- The task named in a placeholder must be in `depends_on`. A manifest that
  breaks this rule fails to load.
- Beez removes newlines at the end of the output, so a one-line result fits
  inside a sentence.
- Braces that do not start with `tasks.` stay as they are.
- `run.json` records the prompt as the manifest wrote it. `tasks/<id>.stdout`
  holds the text each placeholder received. See [Output and logs](output.md).
