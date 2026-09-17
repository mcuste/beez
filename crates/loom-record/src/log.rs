//! Writes the artifacts of one run under `.loom`.
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
use loom_core::{TaskRequest, Workflow};
use loom_process::OutputStream;
use serde::Serialize;

use crate::format::{Labels, stamped_status_line, stream_mark, trim_newline};
use crate::outcome::TaskOutcome;
use crate::time::{compact_utc, utc};

const DIRECTORY: &str = ".loom";

/// Directory that holds every run, inside the Loom root.
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
pub struct LogSettings<'run> {
    /// False writes no artifacts at all.
    pub enabled: bool,
    /// Directory that replaces the one Loom resolves itself.
    pub directory: Option<&'run Path>,
}

/// The artifacts of one run.
pub(crate) struct RunLog {
    directory: PathBuf,
    run: File,
    labels: Labels,
    tasks: Vec<TaskLog>,
    record: RunRecord,
    started: SystemTime,
}

impl std::fmt::Debug for RunLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunLog")
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}

struct TaskLog {
    stdout: File,
    stderr: File,
    record: TaskRecord,
}

#[derive(Debug, Serialize)]
struct RunRecord {
    schema: u32,
    id: String,
    arguments: Vec<String>,
    working_directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<String>,
    started: String,
    finished: String,
    duration_seconds: f64,
    exit_status: Option<i32>,
    tasks: Vec<TaskRecord>,
}

#[derive(Clone, Debug, Serialize)]
struct TaskRecord {
    id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request: Option<RequestRecord>,
    sandbox: bool,
    state: &'static str,
    exit_status: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    stdout: String,
    stderr: String,
}

#[derive(Clone, Debug, Serialize)]
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

impl RunLog {
    /// Creates the directory of one run and opens every file it writes.
    ///
    /// `root` replaces the directory Loom would resolve itself, and
    /// `working_directory` is the directory the run's tasks run in.
    pub(crate) fn create(
        root: Option<&Path>,
        target: &RunTarget<'_>,
        working_directory: &Path,
    ) -> io::Result<Self> {
        let resolved = root.map_or_else(|| resolve_root(working_directory), Path::to_path_buf);
        let started = SystemTime::now();
        let id = run_id(started);
        let directory = resolved.join(RUNS).join(&id);

        fs::create_dir_all(directory.join(TASKS))?;
        // Loom marks only a directory it resolved itself.
        if root.is_none() {
            let _ = ignore_everything(&resolved);
        }
        let _ = link_latest(&resolved, &id);

        let tasks = open_tasks(&directory, target)?;
        let record = RunRecord {
            schema: SCHEMA,
            id,
            arguments: std::env::args_os()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect(),
            working_directory: working_directory.display().to_string(),
            manifest: target
                .manifest()
                .map(|path| working_directory.join(path).display().to_string()),
            started: utc(started),
            finished: String::new(),
            duration_seconds: 0.0,
            exit_status: None,
            tasks: Vec::new(),
        };

        Ok(Self {
            run: File::create(directory.join(RUN_LOG))?,
            directory,
            labels: target.labels(),
            tasks,
            record,
            started,
        })
    }

    /// The directory that holds this run's artifacts.
    #[must_use]
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    /// The labels of the run's tasks.
    pub(crate) fn labels(&self) -> &Labels {
        &self.labels
    }

    /// Records one of Loom's own status lines.
    pub(crate) fn status(&mut self, verb: &str, message: &str) -> io::Result<()> {
        writeln!(
            self.run,
            "{}",
            stamped_status_line(SystemTime::now(), verb, message)
        )
    }

    /// Records one output line of a task.
    ///
    /// The stream file keeps the bytes as the task wrote them. The run log
    /// keeps the line without its colours, behind the task and the stream.
    pub(crate) fn output(
        &mut self,
        index: usize,
        stream: OutputStream,
        line: &[u8],
    ) -> io::Result<()> {
        let width = self.labels.width();
        let stamp = utc(SystemTime::now());
        let Some(task) = self.tasks.get_mut(index) else {
            return Ok(());
        };
        let file = match stream {
            OutputStream::Stdout => &mut task.stdout,
            OutputStream::Stderr => &mut task.stderr,
        };

        file.write_all(line)?;
        let text = strip_bytes(trim_newline(line)).into_vec();
        let text = String::from_utf8_lossy(&text);
        writeln!(
            self.run,
            "{stamp} {:<width$} {} {text}",
            task.record.id,
            stream_mark(stream)
        )
    }

    /// Records what happened to one task.
    pub(crate) fn outcome(&mut self, index: usize, outcome: TaskOutcome) {
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

    /// Writes the record of the run, with Loom's own exit status.
    pub(crate) fn finish(&mut self, exit_status: Option<i32>) -> io::Result<()> {
        let finished = SystemTime::now();
        self.record.finished = utc(finished);
        self.record.duration_seconds = finished
            .duration_since(self.started)
            .unwrap_or_default()
            .as_secs_f64();
        self.record.exit_status = exit_status;
        self.record.tasks = self.tasks.iter().map(|task| task.record.clone()).collect();

        let mut file = File::create(self.directory.join(RUN_RECORD))?;
        serde_json::to_writer_pretty(&mut file, &self.record).map_err(io::Error::other)?;
        writeln!(file)
    }
}

/// Finds the directory that holds the artifacts of every run.
///
/// The nearest ancestor with a `.loom` directory or a repository wins, so
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

/// True for a name Loom gives a run, such as `20260910T030000004Z-4123`.
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
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(target, link)
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
                stdout: File::create(directory.join(&record.stdout))?,
                stderr: File::create(directory.join(&record.stderr))?,
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
        state: "pending",
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
    use super::{is_run_name, resolve_root, run_id};
    use std::fs;
    use std::path::Path;
    use std::time::SystemTime;

    use loom_test_support::TemporaryDirectory;

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
        let root = directory.join(".loom");
        fs::create_dir(&root).unwrap();
        let deep = directory.join("crates/loom-cli");
        fs::create_dir_all(&deep).unwrap();

        assert_eq!(resolve_root(&deep), root);
    }

    #[test]
    fn resolves_the_repository_root_when_no_directory_exists_yet() {
        let directory = TemporaryDirectory::new("log-repository-root").unwrap();
        fs::create_dir(directory.join(".git")).unwrap();
        let deep = directory.join("crates/loom-cli");
        fs::create_dir_all(&deep).unwrap();

        assert_eq!(resolve_root(&deep), directory.join(".loom"));
    }

    /// A submodule and a worktree keep a `.git` file, not a directory.
    #[test]
    fn resolves_a_repository_root_that_keeps_git_in_a_file() {
        let directory = TemporaryDirectory::new("log-git-file").unwrap();
        fs::write(directory.join(".git"), "gitdir: /elsewhere\n").unwrap();

        assert_eq!(resolve_root(directory.path()), directory.join(".loom"));
    }

    #[test]
    fn falls_back_to_the_working_directory() {
        let working_directory = Path::new("/");

        assert_eq!(resolve_root(working_directory), Path::new("/.loom"));
    }
}
