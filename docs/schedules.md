# Schedules

A manifest can say when it runs. The Loom daemon, a background process, reads
the manifests it watches and starts them at the times they name.

```yaml
schedule:
  cron: "0 3 * * *"

sandbox:
  network:
    groups: [github]

tasks:
  - id: review
    harness: claude
    prompt: review yesterday's commits
```

Watch the manifest, then start the daemon:

```sh
loom schedule add nightly.yaml
loom daemon start
loom schedule list
```

```
Daemon running, pid 4123, started 2026-09-09T18:18:02Z, 0 running
JOB      CONDITION  SCHEDULE   NEXT FIRE             LAST RUN
nightly  waiting    0 3 * * *  2026-09-10T03:00:00Z  20260909T030000004Z-4123 (ok)
```

The schedule lives only in the manifest. Loom stores the list of watched
manifests and what already happened, and nothing else. When you edit a
manifest, the daemon reads it again on the next change and on every fire, so
the schedule updates without a command.

One daemon serves every repository. It lives in `~/.loom` no matter where you
run a command, and each watched manifest remembers the directory its tasks run
in.

```sh
loom schedule add ~/work/alpha/nightly.yaml
loom schedule add ~/work/beta/weekly.yaml
loom daemon start
```

`--root <PATH>` or `LOOM_ROOT` uses another directory. A second root is a
second daemon with its own jobs, socket, and runs.

## Cron expressions

Loom takes the five fields of a crontab, in the same order and with the same
meaning.

```
minute  hour  day-of-month  month  day-of-week
```

| Form                 | Meaning                                                    |
| -------------------- | ---------------------------------------------------------- |
| `0 3 * * *`          | Every day at 03:00                                         |
| `*/15 9 * * *`       | Every 15 minutes in the 09:00 hour                         |
| `0 9,17 * * Mon-Fri` | 09:00 and 17:00 on weekdays                                |
| `0 3 * * 1-5`        | The same, with days as numbers                             |
| `0 3 13 * Fri`       | Every 13th, and every Friday                               |
| `@daily`             | Midnight. Also `@hourly`, `@weekly`, `@monthly`, `@yearly` |

Days of the week count from Sunday as 0, and 7 is Sunday too. A restricted day
of month and a restricted day of week mean either day, as in a crontab.

A six-field expression starts with seconds, and a seven-field expression ends
with a year. The seconds must be one fixed value, so a job starts at most once
a minute.

Times are UTC. A schedule may name a fixed offset instead:

```yaml
schedule:
  cron: "0 3 * * *"
  offset: "+02:00"
```

Loom does not read the time zone database, so an offset stays the same all
year. A schedule in a zone with daylight saving moves by one hour twice a year.

## One-time runs

`at` names one instant. The job runs once and is then done.

```yaml
schedule:
  at: "2026-09-10T03:00:00Z"
```

## Schedule fields

| Field               | Default | Meaning                                                   |
| ------------------- | ------- | --------------------------------------------------------- |
| `cron`              |         | A cron expression. Either this or `at`.                   |
| `at`                |         | One RFC 3339 instant. Either this or `cron`.              |
| `offset`            | `Z`     | Fixed offset the `cron` expression is read in.            |
| `on_overlap`        | `skip`  | `skip`, `queue`, or `parallel`.                           |
| `catch_up`          | `false` | Run one missed fire after the daemon was down.            |
| `enabled`           | `true`  | `false` keeps the schedule in the file without firing it. |
| `allow_unsandboxed` | `false` | Let the job run tasks that have `sandbox: false`.         |
| `name`              |         | Names one schedule when a manifest has more than one.     |

A manifest may hold a list of schedules. Each one needs a name:

```yaml
schedule:
  - name: weekday
    cron: "0 3 * * 1-5"
  - name: weekend
    cron: "0 5 * * 6,0"
```

The job ID is the manifest file name, then the schedule name after a colon:
`nightly` or `nightly:weekday`.

## What happens when a job fires

- The daemon runs two jobs at the same time by default. `--limit` changes
  that. A fire that cannot start waits, and a job keeps at most one waiting
  fire.
- A fire that meets the same job's last run follows `on_overlap`. `skip`
  drops it and says so in the log, `queue` starts it when the run ends, and
  `parallel` starts it beside the run.
- A fire that happened while the daemon was down runs late only when the
  schedule sets `catch_up: true`, and then only once, no matter how many
  fires were missed. Other missed fires go in the log only.
- Nobody watches a scheduled run, so the daemon refuses a job with tasks that
  have no sandbox. Set `allow_unsandboxed: true` on the schedule to run it
  anyway.
- The daemon reads the manifest again at the moment a job fires. A manifest
  that no longer loads keeps its job in the list, marked `broken`, with the
  reason.

## Commands

| Command                           | What it does                              |
| --------------------------------- | ----------------------------------------- |
| `loom schedule add <manifest>`    | Watches one more manifest                 |
| `loom schedule remove <manifest>` | Stops watching one manifest               |
| `loom schedule list`              | Reports every job                         |
| `loom schedule trigger <job>`     | Runs one job now, beside its schedule     |
| `loom schedule pause <job>`       | Holds one job back                        |
| `loom schedule resume <job>`      | Lets a paused job fire again              |
| `loom daemon run`                 | Runs the daemon in this terminal          |
| `loom daemon start`               | Starts the daemon in the background       |
| `loom daemon stop`                | Stops it once its running runs end        |
| `loom daemon status`              | Reports the daemon and its jobs           |
| `loom daemon reload`              | Reads every watched manifest again        |

`add`, `remove`, `list`, `pause`, and `resume` work without a daemon. The
others need a running one.

## Files

The daemon keeps its own files and its runs in one root.

```
~/.loom/
  daemon/
    daemon.sock      commands arrive here
    daemon.json      the pid and start time of the running daemon
    daemon.log       the daemon's own lines
    manifests.json   the manifests it watches
    state.json       the last fire, the last run, and paused jobs
  runs/
    20260910T030000004Z-4123/
```

`manifests.json` holds the path of each watched manifest and the directory its
tasks run in. Nothing is copied, so a manifest stays in its own repository.

A scheduled run writes the same files as a run you start by hand, described in
[Output and logs](output.md), but under the daemon's root. `run.json` names the
manifest and the working directory, and the run log names the fire that
started it:

```
2026-09-10T03:00:00.004Z Trigger   nightly scheduled for 2026-09-10T03:00:00Z
2026-09-10T03:00:00.004Z Running   review
```

The daemon writes its own lines to standard error. `loom daemon start` sends
them to `daemon.log`.

```
2026-09-10T03:00:00.001Z Firing    nightly scheduled for 2026-09-10T03:00:00Z
2026-09-10T03:00:00.004Z Logging   nightly: runs/20260910T030000004Z-4123
2026-09-10T03:04:11.882Z Finished  nightly: 3 passed in 251.3s
```

The daemon keeps the newest 200 runs and removes the rest, when it starts and
after every run. `--keep-runs` changes the number, and zero keeps every run.

## Running at boot

`loom daemon start` survives a closed terminal, but not a reboot. Use launchd
or systemd for that, with `loom daemon run` in the foreground.

launchd:

```xml
<key>ProgramArguments</key>
<array>
  <string>/usr/local/bin/loom</string>
  <string>daemon</string>
  <string>run</string>
</array>
<key>StandardErrorPath</key>
<string>/Users/you/.loom/daemon/daemon.log</string>
```

systemd:

```ini
[Service]
ExecStart=/usr/local/bin/loom daemon run
```

The daemon finds its jobs in `manifests.json` in its root, so the service
needs no working directory. A daemon that cannot write its root says so and
stops. `loom daemon reload` reads the list again, so a running daemon picks
up a list written by hand.

A scheduled harness run needs its credentials in the environment of the
daemon, which is the environment the service manager gives it.

## Limits

- `stop` waits for running runs to end. A second stop signal leaves them
  behind.
- Sandbox notes from a scheduled run go to the daemon's log, not to the log
  of the run, because several runs share one sandbox reporter.
- A schedule holds an offset, not a time zone, so daylight saving moves it.
