//! Writes the artifacts of one run under `.beez`.
//!
//! Every run gets its own directory, named after the time it started. The
//! directory holds the whole run as one readable log, the exact bytes of each
//! task's two streams, and a machine-readable record of the outcome.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::SystemTime;

use anstream::adapter::strip_bytes;
use beez_core::{TaskRequest, Workflow};
use beez_process::OutputStream;
use beez_runner::RunEvent;
use serde::Serialize;

use crate::format::{Labels, stamped_status_line, stream_mark, trim_newline};
use crate::outcome::{TaskOutcome, TaskState};
use crate::time::{compact_utc, utc};

const DIRECTORY: &str = ".beez";

/// Directory that holds every run, inside the Beez root.
pub const RUNS: &str = "runs";
const TASKS: &str = "tasks";
const LATEST: &str = "latest";
const RUN_LOG: &str = "run.log";
const RUN_RECORD: &str = "run.json";
/// Layout version of `run.json`.
const SCHEMA: u32 = 1;

/// What one run executes.
#[derive(Clone, Copy, Debug)]
pub enum RunTarget<'run> {
    /// Every task of a workflow manifest.
    Workflow {
        /// Path of the manifest, as the run named it.
        path: &'run Path,
        /// Loaded workflow.
        workflow: &'run Workflow,
    },
    /// One direct request, named after the command that asked for it.
    Request {
        /// Command name, such as `claude` or `command`.
        name: &'run str,
    },
}

impl RunTarget<'_> {
    /// Task labels in declaration order.
    #[must_use]
    pub fn labels(&self) -> Labels {
        Labels::new(match self {
            Self::Workflow { workflow, .. } => workflow
                .tasks()
                .iter()
                .map(|task| task.id().as_str().to_owned())
                .collect(),
            Self::Request { name } => vec![(*name).to_owned()],
        })
    }

    fn manifest(&self) -> Option<&Path> {
        match self {
            Self::Workflow { path, .. } => Some(path),
            Self::Request { .. } => None,
        }
    }
}

/// Whether a run writes artifacts, and where.
#[derive(Clone, Copy, Debug)]
pub enum LogSettings<'run> {
    /// The run writes no artifacts at all.
    Disabled,
    /// The run writes to the directory Beez resolves itself.
    Resolved,
    /// The run writes to this directory instead.
    At(&'run Path),
}

/// Turns the events of one run into its artifacts.
#[derive(Debug)]
pub struct RunRecorder {
    directory: PathBuf,
    run: Sink,
    labels: Labels,
    tasks: Vec<TaskLog>,
    id: String,
    arguments: Vec<String>,
    working_directory: String,
    manifest: Option<String>,
    started: SystemTime,
}

#[derive(Debug)]
struct TaskLog {
    stdout: Sink,
    stderr: Sink,
    record: TaskRecord,
}

/// One file the run writes line by line.
///
/// After a write fails, the file takes no more writes, so one broken file
/// costs neither the other files nor the record of the run.
#[derive(Debug)]
struct Sink {
    file: File,
    failed: bool,
}

impl Sink {
    fn new(file: File) -> Self {
        Self {
            file,
            failed: false,
        }
    }

    /// Writes to the file unless an earlier write failed.
    fn write(&mut self, action: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
        if self.failed {
            return Ok(());
        }
        let result = action(&mut self.file);
        self.failed = result.is_err();
        result
    }
}

/// The layout of `run.json`.
#[derive(Serialize)]
struct RunRecord<'run> {
    schema: u32,
    id: &'run str,
    arguments: &'run [String],
    working_directory: &'run str,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<&'run str>,
    started: String,
    finished: String,
    duration_seconds: f64,
    exit_status: Option<i32>,
    tasks: Vec<&'run TaskRecord>,
}

#[derive(Debug, Serialize)]
struct TaskRecord {
    id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request: Option<RequestRecord>,
    sandbox: bool,
    state: TaskState,
    exit_status: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum RequestRecord {
    Harness {
        harness: String,
        prompt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },
    Command {
        program: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        arguments: Vec<String>,
    },
}

impl RunRecorder {
    /// Creates the directory of one run and opens every file it writes, or
    /// nothing when `settings` disable the artifacts.
    ///
    /// `working_directory` is the directory the run's tasks run in.
    pub(crate) fn create(
        settings: LogSettings<'_>,
        target: &RunTarget<'_>,
        working_directory: &Path,
    ) -> io::Result<Option<Self>> {
        let (root, resolved) = match settings {
            LogSettings::Disabled => return Ok(None),
            LogSettings::Resolved => (resolve_root(working_directory), true),
            LogSettings::At(directory) => (directory.to_path_buf(), false),
        };
        let started = SystemTime::now();
        let id = run_id(started);
        let directory = root.join(RUNS).join(&id);

        fs::create_dir_all(directory.join(TASKS))?;
        // Beez marks only a directory it resolved itself.
        if resolved {
            let _ = ignore_everything(&root);
        }
        let _ = link_latest(&root, &id);

        let tasks = open_tasks(&directory, target)?;
        let labels = Labels::new(tasks.iter().map(|task| task.record.id.clone()).collect());

        Ok(Some(Self {
            run: Sink::new(File::create(directory.join(RUN_LOG))?),
            directory,
            labels,
            tasks,
            id,
            arguments: std::env::args_os()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect(),
            working_directory: working_directory.display().to_string(),
            manifest: target
                .manifest()
                .map(|path| working_directory.join(path).display().to_string()),
            started,
        }))
    }

    /// The directory that holds the artifacts.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Records one lifecycle or output event.
    pub fn event(&mut self, event: &RunEvent) -> io::Result<()> {
        let index = event.position();
        if let RunEvent::Output { stream, line, .. } = event {
            return self.output(index, *stream, line);
        }
        let Some(outcome) = TaskOutcome::of(event) else {
            return Ok(());
        };
        self.outcome(index, outcome);
        let (state, message) = outcome.status(&self.labels.get(index));

        self.status(state.verb(), &message)
    }

    /// Records one of Beez's own status lines, such as a sandbox note.
    pub fn status(&mut self, verb: &str, message: &str) -> io::Result<()> {
        let line = stamped_status_line(SystemTime::now(), verb, message);
        self.run.write(|file| writeln!(file, "{line}"))
    }

    /// Writes the line that closes the run, then the record of the run.
    ///
    /// The record follows even when the line fails, and the first failure
    /// comes back. `exit_status` is the status Beez itself returns. It is
    /// absent when the run ended in an execution error.
    pub fn finish(&mut self, summary: &str, exit_status: Option<i32>) -> io::Result<()> {
        let line = self.status("Summary", summary);
        let record = self.write_record(exit_status);

        line.and(record)
    }

    /// Records one output line of a task.
    ///
    /// The stream file keeps the bytes as the task wrote them. The run log
    /// keeps the line without its colours, behind the task and the stream.
    /// Both get the line even when the other fails.
    fn output(&mut self, index: usize, stream: OutputStream, line: &[u8]) -> io::Result<()> {
        let width = self.labels.width();
        let stamp = utc(SystemTime::now());
        let Some(task) = self.tasks.get_mut(index) else {
            return Ok(());
        };
        let file = match stream {
            OutputStream::Stdout => &mut task.stdout,
            OutputStream::Stderr => &mut task.stderr,
        };

        let raw = file.write(|file| file.write_all(line));
        let text = strip_bytes(trim_newline(line)).into_vec();
        let text = String::from_utf8_lossy(&text);
        let id = &task.record.id;
        let mark = stream_mark(stream);
        let logged = self
            .run
            .write(|file| writeln!(file, "{stamp} {id:<width$} {mark} {text}"));

        raw.and(logged)
    }

    /// Swaps the run log for a read-only handle, so every later line fails.
    #[cfg(test)]
    pub(crate) fn break_run_log(&mut self) -> io::Result<()> {
        self.run = Sink::new(File::open(self.directory.join(RUN_LOG))?);
        Ok(())
    }

    /// Records what happened to one task.
    fn outcome(&mut self, index: usize, outcome: TaskOutcome) {
        let Some(task) = self.tasks.get_mut(index) else {
            return;
        };
        task.record.state = outcome.state();
        match outcome {
            TaskOutcome::Finished {
                exit_status,
                elapsed,
            } => {
                task.record.exit_status = exit_status;
                task.record.duration_seconds = Some(elapsed.as_secs_f64());
            }
            TaskOutcome::Failed { error, elapsed } => {
                task.record.error = Some(error.to_string());
                task.record.duration_seconds = Some(elapsed.as_secs_f64());
            }
            TaskOutcome::Started | TaskOutcome::Blocked => {}
        }
    }

    /// Writes the record of the run, with Beez's own exit status.
    fn write_record(&self, exit_status: Option<i32>) -> io::Result<()> {
        let finished = SystemTime::now();
        let record = RunRecord {
            schema: SCHEMA,
            id: &self.id,
            arguments: &self.arguments,
            working_directory: &self.working_directory,
            manifest: self.manifest.as_deref(),
            started: utc(self.started),
            finished: utc(finished),
            duration_seconds: finished
                .duration_since(self.started)
                .unwrap_or_default()
                .as_secs_f64(),
            exit_status,
            tasks: self.tasks.iter().map(|task| &task.record).collect(),
        };

        let mut file = File::create(self.directory.join(RUN_RECORD))?;
        serde_json::to_writer_pretty(&mut file, &record).map_err(io::Error::other)?;
        writeln!(file)
    }
}

/// Finds the directory that holds the artifacts of every run.
///
/// The nearest ancestor with a `.beez` directory or a repository wins, so
/// every run of one repository lands together. The working directory is the
/// fallback.
fn resolve_root(working_directory: &Path) -> PathBuf {
    for ancestor in working_directory.ancestors() {
        let candidate = ancestor.join(DIRECTORY);
        // A worktree and a submodule keep `.git` in a file, not a directory.
        if candidate.is_dir() || ancestor.join(".git").exists() {
            return candidate;
        }
    }
    working_directory.join(DIRECTORY)
}

/// The name of one run.
///
/// A PID cannot repeat inside one millisecond, so this names one run only, and
/// the stamp sorts the runs in the order they happened.
fn run_id(started: SystemTime) -> String {
    format!("{}-{}", compact_utc(started), process::id())
}

/// True for a name Beez gives a run, such as `20260910T030000004Z-4123`.
///
/// Anything else in the runs directory, such as the `latest` link, is not a run.
#[must_use]
pub fn is_run_name(name: &str) -> bool {
    let Some((stamp, pid)) = name.split_once('-') else {
        return false;
    };

    stamp.len() == 19
        && stamp.ends_with('Z')
        && stamp.get(8..9) == Some("T")
        && !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
}

/// Keeps the artifacts out of Git without a change to the repository.
pub fn ignore_everything(root: &Path) -> io::Result<()> {
    let path = root.join(".gitignore");
    if path.exists() {
        return Ok(());
    }
    fs::write(path, "*\n")
}

#[cfg(unix)]
fn link_latest(root: &Path, id: &str) -> io::Result<()> {
    let link = root.join(LATEST);
    // A relative target keeps the link working when the directory moves.
    let target = Path::new(RUNS).join(id);
    // The new link takes the place of the old one in one step, so a run that
    // starts at the same time never finds `latest` missing.
    let temporary = root.join(format!(".{LATEST}-{id}"));
    let _ = fs::remove_file(&temporary);
    std::os::unix::fs::symlink(target, &temporary)?;
    fs::rename(&temporary, link).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

#[cfg(not(unix))]
fn link_latest(_root: &Path, _id: &str) -> io::Result<()> {
    Ok(())
}

fn open_tasks(directory: &Path, target: &RunTarget<'_>) -> io::Result<Vec<TaskLog>> {
    task_records(target)
        .into_iter()
        .map(|record| {
            Ok(TaskLog {
                stdout: Sink::new(File::create(directory.join(&record.stdout))?),
                stderr: Sink::new(File::create(directory.join(&record.stderr))?),
                record,
            })
        })
        .collect()
}

/// The record of every task before it runs, in declaration order.
///
/// A direct request needs no description, because the recorded arguments
/// already hold it.
fn task_records(target: &RunTarget<'_>) -> Vec<TaskRecord> {
    match target {
        RunTarget::Workflow { workflow, .. } => workflow
            .tasks()
            .iter()
            .map(|task| {
                let depends_on = task
                    .dependencies()
                    .iter()
                    .filter_map(|index| workflow.task(*index))
                    .map(|dependency| dependency.id().as_str().to_owned())
                    .collect();
                task_record(
                    task.id().as_str().to_owned(),
                    depends_on,
                    Some(request_record(task.request())),
                    task.sandbox().is_some(),
                )
            })
            .collect(),
        RunTarget::Request { name } => {
            vec![task_record((*name).to_owned(), Vec::new(), None, false)]
        }
    }
}

fn task_record(
    id: String,
    depends_on: Vec<String>,
    request: Option<RequestRecord>,
    sandbox: bool,
) -> TaskRecord {
    TaskRecord {
        stdout: format!("{TASKS}/{id}.stdout"),
        stderr: format!("{TASKS}/{id}.stderr"),
        id,
        depends_on,
        request,
        sandbox,
        state: TaskState::Pending,
        exit_status: None,
        duration_seconds: None,
        error: None,
    }
}

fn request_record(request: &TaskRequest) -> RequestRecord {
    match request {
        TaskRequest::Harness {
            harness,
            prompt,
            options,
        } => RequestRecord::Harness {
            harness: harness.name().to_owned(),
            prompt: prompt.clone(),
            model: options.model().map(str::to_owned),
            effort: options.effort().map(str::to_owned),
        },
        TaskRequest::Command { program, arguments } => RequestRecord::Command {
            program: program.clone(),
            arguments: arguments.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LogSettings, RunRecorder, RunTarget, is_run_name, link_latest, resolve_root, run_id,
    };
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, SystemTime};

    use beez_core::{TaskDefinition, TaskRequest, Workflow};
    use beez_test_support::TemporaryDirectory;

    use crate::outcome::TaskOutcome;

    /// Records one run under `root` and returns the text of its `run.json`.
    fn run_record(
        root: &Path,
        target: &RunTarget<'_>,
        outcomes: &[(usize, TaskOutcome)],
    ) -> String {
        let mut recorder = RunRecorder::create(LogSettings::At(root), target, root)
            .unwrap()
            .unwrap();
        for (index, outcome) in outcomes {
            recorder.outcome(*index, *outcome);
        }
        recorder.write_record(Some(0)).unwrap();

        fs::read_to_string(recorder.directory().join("run.json")).unwrap()
    }

    /// Replaces every value that changes from one run to the next.
    fn mask(record: &str) -> String {
        const VOLATILE: [&str; 4] = ["started", "finished", "working_directory", "manifest"];
        const TOP_LEVEL_VOLATILE: [&str; 2] = ["id", "duration_seconds"];

        let mut masked = Vec::new();
        let mut lines = record.lines();
        while let Some(line) = lines.next() {
            if line.starts_with("  \"arguments\": [") {
                if !line.ends_with("],") {
                    for inner in lines.by_ref() {
                        if inner == "  ]," {
                            break;
                        }
                    }
                }
                masked.push("  \"arguments\": <masked>,".to_owned());
                continue;
            }
            let top_level = line.starts_with("  \"");
            let volatile = line.trim_start().split_once(": ").is_some_and(|(key, _)| {
                let key = key.trim_matches('"');
                VOLATILE.contains(&key) || (top_level && TOP_LEVEL_VOLATILE.contains(&key))
            });
            match line.split_once(": ") {
                Some((key, value)) if volatile => {
                    let comma = if value.ends_with(',') { "," } else { "" };
                    masked.push(format!("{key}: <masked>{comma}"));
                }
                _ => masked.push(line.to_owned()),
            }
        }
        masked.join("\n")
    }

    #[test]
    fn names_every_task_state_as_run_json_spells_it() {
        let elapsed = Duration::from_millis(10);
        let cases = [
            (None, "pending"),
            (Some(TaskOutcome::Started), "running"),
            (
                Some(TaskOutcome::Finished {
                    exit_status: Some(0),
                    elapsed,
                }),
                "finished",
            ),
            (
                Some(TaskOutcome::Finished {
                    exit_status: Some(3),
                    elapsed,
                }),
                "failed",
            ),
            (
                Some(TaskOutcome::Failed {
                    error: io::ErrorKind::NotFound,
                    elapsed,
                }),
                "failed",
            ),
            (Some(TaskOutcome::Blocked), "blocked"),
        ];

        for (outcome, state) in cases {
            let directory = TemporaryDirectory::new("log-state").unwrap();
            let outcomes: Vec<_> = outcome.into_iter().map(|outcome| (0, outcome)).collect();
            let record = run_record(
                directory.path(),
                &RunTarget::Request { name: "command" },
                &outcomes,
            );
            let record: serde_json::Value = serde_json::from_str(&record).unwrap();

            assert_eq!(
                record.pointer("/tasks/0/state"),
                Some(&state.into()),
                "{outcome:?}"
            );
        }
    }

    #[test]
    fn writes_the_run_record_in_a_fixed_layout() {
        let directory = TemporaryDirectory::new("log-record-layout").unwrap();
        let workflow = Workflow::try_from(vec![
            TaskDefinition::new(
                "build".parse().unwrap(),
                Vec::new(),
                TaskRequest::command("bash".to_owned(), vec!["-c".to_owned(), "true".to_owned()]),
            ),
            TaskDefinition::new(
                "test".parse().unwrap(),
                vec!["build".parse().unwrap()],
                TaskRequest::command("cargo".to_owned(), Vec::new()),
            ),
        ])
        .unwrap();
        let target = RunTarget::Workflow {
            path: Path::new("beez.toml"),
            workflow: &workflow,
        };

        let record = run_record(
            directory.path(),
            &target,
            &[
                (
                    0,
                    TaskOutcome::Finished {
                        exit_status: Some(0),
                        elapsed: Duration::from_millis(1_500),
                    },
                ),
                (
                    1,
                    TaskOutcome::Failed {
                        error: io::ErrorKind::NotFound,
                        elapsed: Duration::from_millis(200),
                    },
                ),
            ],
        );

        assert_eq!(
            mask(&record),
            r#"{
  "schema": 1,
  "id": <masked>,
  "arguments": <masked>,
  "working_directory": <masked>,
  "manifest": <masked>,
  "started": <masked>,
  "finished": <masked>,
  "duration_seconds": <masked>,
  "exit_status": 0,
  "tasks": [
    {
      "id": "build",
      "request": {
        "kind": "command",
        "program": "bash",
        "arguments": [
          "-c",
          "true"
        ]
      },
      "sandbox": false,
      "state": "finished",
      "exit_status": 0,
      "duration_seconds": 1.5,
      "stdout": "tasks/build.stdout",
      "stderr": "tasks/build.stderr"
    },
    {
      "id": "test",
      "depends_on": [
        "build"
      ],
      "request": {
        "kind": "command",
        "program": "cargo"
      },
      "sandbox": false,
      "state": "failed",
      "exit_status": null,
      "duration_seconds": 0.2,
      "error": "entity not found",
      "stdout": "tasks/test.stdout",
      "stderr": "tasks/test.stderr"
    }
  ]
}"#
        );
    }

    #[test]
    fn reads_back_the_name_it_gives_a_run() {
        let id = run_id(SystemTime::now());

        assert!(is_run_name(&id), "{id}");
    }

    #[test]
    fn tells_a_run_from_anything_else_in_the_directory() {
        assert!(!is_run_name("latest"));
        assert!(!is_run_name("20260910T030000004Z"));
        assert!(!is_run_name("20260910T030000004Z-"));
        assert!(!is_run_name("20260910T030000004Z-abc"));
        assert!(!is_run_name("2026-09-10"));
    }

    #[test]
    fn resolves_an_existing_directory_above_the_working_directory() {
        let directory = TemporaryDirectory::new("log-existing-root").unwrap();
        let root = directory.join(".beez");
        fs::create_dir(&root).unwrap();
        let deep = directory.join("crates/beez-cli");
        fs::create_dir_all(&deep).unwrap();

        assert_eq!(resolve_root(&deep), root);
    }

    #[test]
    fn resolves_the_repository_root_when_no_directory_exists_yet() {
        let directory = TemporaryDirectory::new("log-repository-root").unwrap();
        fs::create_dir(directory.join(".git")).unwrap();
        let deep = directory.join("crates/beez-cli");
        fs::create_dir_all(&deep).unwrap();

        assert_eq!(resolve_root(&deep), directory.join(".beez"));
    }

    /// A submodule and a worktree keep a `.git` file, not a directory.
    #[test]
    fn resolves_a_repository_root_that_keeps_git_in_a_file() {
        let directory = TemporaryDirectory::new("log-git-file").unwrap();
        fs::write(directory.join(".git"), "gitdir: /elsewhere\n").unwrap();

        assert_eq!(resolve_root(directory.path()), directory.join(".beez"));
    }

    #[test]
    fn falls_back_to_the_working_directory() {
        let working_directory = Path::new("/");

        assert_eq!(resolve_root(working_directory), Path::new("/.beez"));
    }

    /// The names in `root` besides `latest`, such as a leftover temporary link.
    fn other_entries(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "latest")
            .collect();
        names.sort();
        names
    }

    #[cfg(unix)]
    #[test]
    fn replaces_a_link_to_an_older_run() {
        let directory = TemporaryDirectory::new("log-latest-replace").unwrap();
        let root = directory.path();

        link_latest(root, "20260910T030000004Z-1").unwrap();
        link_latest(root, "20260910T030000005Z-2").unwrap();

        assert_eq!(
            fs::read_link(root.join("latest")).unwrap(),
            Path::new("runs/20260910T030000005Z-2")
        );
        assert!(other_entries(root).is_empty(), "{:?}", other_entries(root));
    }

    /// Two runs that start together must never leave a moment without `latest`.
    #[cfg(unix)]
    #[test]
    fn keeps_a_valid_latest_link_while_runs_race_to_replace_it() {
        const WRITERS: usize = 8;
        const ROUNDS: usize = 200;

        let directory = TemporaryDirectory::new("log-latest-race").unwrap();
        let root = directory.path().to_path_buf();
        link_latest(&root, "20260910T030000000Z-0").unwrap();
        let ids: Vec<String> = (1..=WRITERS)
            .map(|writer| format!("20260910T030000000Z-{writer}"))
            .collect();
        let targets: Vec<PathBuf> = ids.iter().map(|id| Path::new("runs").join(id)).collect();
        let done = AtomicBool::new(false);

        let missing = thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let mut missing = 0_usize;
                while !done.load(Ordering::Relaxed) {
                    // macOS may fail to follow a link while a rename replaces
                    // it, so this checks only that the entry is there.
                    if fs::symlink_metadata(root.join("latest")).is_err() {
                        missing += 1;
                    }
                }
                missing
            });
            let writers: Vec<_> = ids
                .iter()
                .map(|id| {
                    let root = &root;
                    scope.spawn(move || {
                        for _ in 0..ROUNDS {
                            let _ = link_latest(root, id);
                        }
                    })
                })
                .collect();
            for writer in writers {
                writer.join().unwrap();
            }
            done.store(true, Ordering::Relaxed);
            reader.join().unwrap()
        });

        assert_eq!(missing, 0, "`latest` went missing {missing} times");
        let latest = fs::read_link(root.join("latest")).unwrap();
        assert!(targets.contains(&latest), "{latest:?}");
        assert!(
            other_entries(&root).is_empty(),
            "{:?}",
            other_entries(&root)
        );
    }
}
