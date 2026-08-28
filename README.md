# Loom

Loom is a local task orchestrator for headless agent harnesses and Bash tasks.

Run a workflow with:

```sh
loom run workflow workflow.yaml
```

Workflow manifests support YAML and JSON. Each task defines either a harness
prompt or a literal command. Dependencies run before their dependents.

```yaml
tasks:
  - id: inspect
    harness: pi
    prompt: inspect the repository

  - id: test
    depends_on: [inspect]
    command: [cargo, test, --workspace]
```
