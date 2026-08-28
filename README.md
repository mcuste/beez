# Loom

Loom is a local task orchestrator for headless agent harnesses and Bash tasks.

Run a workflow with:

```sh
loom run workflow workflow.yaml
```

Workflow manifests support YAML and JSON. Each task defines either a harness
prompt or a literal command. Dependencies run before their dependents.

Harness tasks accept an optional `model` and `effort`. Loom sends the effort with
each harness's own flag, `--effort` for `pi` and `--thinking` for `omp`.

```yaml
tasks:
  - id: inspect
    harness: pi
    prompt: inspect the repository
    model: opus
    effort: high

  - id: test
    depends_on: [inspect]
    command: [cargo, test, --workspace]
```

Run a single harness prompt with the same options:

```sh
loom run pi --model opus --effort high "inspect the repository"
```
