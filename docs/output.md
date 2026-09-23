# Output and logs

Beez prints task output to the terminal while a run happens, and saves the
same output to disk so you can read it later.

## Terminal output

Tasks in a workflow run at the same time, so Beez labels each line with the
task ID. `--output` picks the form.

`stream` is the default. It prints every line as it arrives. A solid bar marks
a line from standard output and a dashed bar marks standard error. One spinner
per running task sits at the bottom of the terminal. When a task ends, one
status line replaces its spinner.

```
api         │ GET /users?page=1 200
web         │ bundling chunk 1 of 7
api         ┊ warning: slow query took 412ms
Finished  worker_pool in 4.1s (ok)
web         │ bundling chunk 7 of 7
Finished  web in 7.2s (ok)
Summary   3 passed in 10.3s
```

`grouped` holds the output of each task and prints it when the task ends, so
two tasks never mix. A status line opens and closes each task.

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

In grouped mode a long task shows nothing until it ends, and a run that is
killed loses what it had collected. Prefer `stream` in CI.

A workflow with one task, `beez run command`, and the single-prompt harness
commands pass the output through unchanged. Use one of those when a later step
needs the exact bytes.

Beez keeps its own lines apart from task output:

- Task output goes to standard output. Every line Beez writes goes to standard
  error.
- Beez's lines start with a status word in the first column. Task output is
  indented or prefixed.
- Beez does not change the bytes of a task, so the colours a task chose reach
  the terminal as written.

```sh
beez run workflow build.yaml > tasks.log 2> beez.log
```

Sandbox notes use the status word form, so a denied connection never looks
like task output:

```
Running   network
Sandbox   denied connection to example.com:443
Finished  network in 0.1s (ok)
```

Spinners draw on standard error and draw nothing when standard error is not a
terminal.

## Timestamps and colour

`--timestamps` adds a UTC date and time to every line Beez writes. Beez does
not read the time zone database, so it reports UTC and marks it with `Z`.

```
2026-09-09T15:16:37.775Z api         │ GET /users?page=1 200
2026-09-09T15:16:41.882Z Finished  worker_pool in 4.1s (ok)
```

`--timestamps=elapsed` shows the time since the run started instead.

```
    0.0s api         │ GET /users?page=1 200
    4.1s Finished  worker_pool in 4.1s (ok)
```

Write the value with an equals sign. Without it, the flag would take the
manifest path as its value. In grouped mode a line keeps the time it arrived,
not the time its block was printed.

`--color auto|always|never` controls Beez's own colours. Beez also follows
`NO_COLOR`, `CLICOLOR_FORCE`, and `TERM=dumb`.

## Saved runs

Every run writes its output to disk. Beez uses `.beez/` in the repository
root, or in the working directory outside a repository. An existing `.beez/`
in a parent directory wins over both. The directory ignores itself in Git.

Each run gets a directory named after its start time and process ID. The
names sort by time, and `latest` points at the newest run.

```
.beez/
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

| File                 | Contents                                                                                            |
| -------------------- | --------------------------------------------------------------------------------------------------- |
| `run.log`            | The whole run in one file: every task line with its ID and stream mark, every Beez line, UTC times. Colours removed. |
| `tasks/<id>.stdout`  | The exact bytes one task wrote to standard output.                                                  |
| `tasks/<id>.stderr`  | The exact bytes one task wrote to standard error.                                                   |
| `run.json`           | The arguments, working directory, manifest, start and end times, exit status, and one entry per task with its dependencies, request, state, exit status, and duration. |

Beez names the run directory before the tasks start:

```
Logging   .beez/runs/20260909T164512815Z-70632
Running   api
```

Options:

- `--log-dir <PATH>` or `BEEZ_LOG_DIR` writes the run somewhere else.
- `--no-log` writes nothing.

A problem with the saved files never stops a run. Beez reports it once as a
warning and continues. Beez keeps every run. Remove old runs yourself when the
directory grows too large. The daemon prunes its own runs, see
[Schedules](schedules.md).
