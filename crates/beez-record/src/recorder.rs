//! Keeps the artifacts of one run open, and reports their first failure.
//!
//! Beez writes the same lines whether a person started the run or the daemon
//! did, because both send their events through here.

use std::io;
use std::path::Path;
use std::time::Duration;

use beez_runner::RunEvent;

use crate::log::{LogSettings, RunRecorder, RunTarget};
use crate::tally::RunTally;

/// Why Beez could not write the artifacts of a run.
///
/// Beez shows it as a one-line warning, so the message names its cause.
#[derive(Debug, thiserror::Error)]
#[error("run log: {0}")]
pub struct LogFailure(#[source] io::Error);

/// The artifacts of one run, which may be absent, and the counts of its tasks.
///
/// A file that fails to write takes no more writes, and the run goes on,
/// because the result of a run matters more than its record. The rest of the
/// artifacts, `run.json` among them, are still written. The counts go on
/// without any artifacts at all.
#[derive(Debug)]
pub struct OpenLog {
    recorder: Option<RunRecorder>,
    warned: bool,
    tally: RunTally,
}

impl OpenLog {
    /// Opens the artifacts of one run, with the warning to show when they
    /// cannot open.
    ///
    /// The run goes on either way, so the caller shows the warning and keeps
    /// going.
    pub fn open(
        settings: LogSettings<'_>,
        target: &RunTarget<'_>,
        working_directory: &Path,
    ) -> (Self, Option<LogFailure>) {
        let (recorder, failure) = match RunRecorder::create(settings, target, working_directory) {
            Ok(recorder) => {
                if let Some(recorder) = &recorder {
                    tracing::debug!(
                        directory = %recorder.directory().display(),
                        "opened the run log"
                    );
                } else {
                    tracing::debug!("run log disabled");
                }
                (recorder, None)
            }
            Err(error) => {
                tracing::debug!(%error, "cannot open the run log");
                (None, Some(LogFailure(error)))
            }
        };
        let log = Self {
            recorder,
            warned: failure.is_some(),
            tally: RunTally::default(),
        };
        (log, failure)
    }

    /// The directory that holds the artifacts, when the run writes any.
    #[must_use]
    pub fn directory(&self) -> Option<&Path> {
        self.recorder.as_ref().map(RunRecorder::directory)
    }

    /// Counts one event and records it.
    pub fn event(&mut self, event: &RunEvent<'_>) -> Option<LogFailure> {
        self.tally.record(event);
        self.write(|recorder| recorder.event(event))
    }

    /// How many tasks failed or could not run so far.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.tally.failed()
    }

    /// Writes the line that closes the run, then the record of the run, and
    /// returns the line's message.
    ///
    /// `exit_status` is the status Beez itself returns. It is absent when the
    /// run ended in an execution error.
    pub fn finish(
        &mut self,
        elapsed: Duration,
        exit_status: Option<i32>,
    ) -> (String, Option<LogFailure>) {
        let summary = self.tally.summary(elapsed);
        let failure = self.write(|recorder| recorder.finish(&summary, exit_status));
        (summary, failure)
    }

    /// Writes to the artifacts, and returns a warning for the first failure
    /// only, so the caller shows it once. Every later failure stays quiet.
    pub fn write(
        &mut self,
        action: impl FnOnce(&mut RunRecorder) -> io::Result<()>,
    ) -> Option<LogFailure> {
        let recorder = self.recorder.as_mut()?;
        let failure = action(recorder).err()?;
        if self.warned {
            // The caller warned about the first failure only.
            tracing::warn!(error = %failure, "run log write failed");
            return None;
        }
        tracing::debug!(error = %failure, "run log write failed, warning once");
        self.warned = true;

        Some(LogFailure(failure))
    }
}

#[cfg(test)]
mod tests {
    use super::OpenLog;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use beez_process::OutputStream;
    use beez_runner::RunEvent;
    use beez_test_support::TemporaryDirectory;

    use crate::log::{LogSettings, RunRecorder, RunTarget};

    const TARGET: RunTarget<'static> = RunTarget::Request { name: "command" };

    fn open(root: &Path) -> (OpenLog, PathBuf) {
        let (log, failure) = OpenLog::open(LogSettings::At(root), &TARGET, root);
        assert!(failure.is_none(), "{failure:?}");
        let directory = log.directory().unwrap().to_path_buf();
        (log, directory)
    }

    /// Plays one run that starts, prints a line, and fails, collecting every
    /// warning the log returns. `before` runs ahead of the first event.
    fn play(log: &mut OpenLog, before: impl FnOnce(&mut OpenLog)) -> Vec<String> {
        before(log);
        let events = [
            RunEvent::Started { task: None },
            RunEvent::Output {
                task: None,
                stream: OutputStream::Stdout,
                line: b"hello\n",
            },
            RunEvent::Failed {
                task: None,
                error_kind: io::ErrorKind::NotFound,
                elapsed: Duration::from_millis(5),
            },
        ];
        let mut warnings: Vec<String> = events
            .iter()
            .filter_map(|event| log.event(event))
            .map(|failure| failure.to_string())
            .collect();
        let (_, failure) = log.finish(Duration::from_millis(5), Some(1));
        warnings.extend(failure.map(|failure| failure.to_string()));
        warnings
    }

    fn record(directory: &Path) -> serde_json::Value {
        let text = fs::read_to_string(directory.join("run.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn counts_the_tasks_of_a_run_without_artifacts() {
        let root = TemporaryDirectory::new("recorder-disabled").unwrap();
        let (mut log, failure) = OpenLog::open(LogSettings::Disabled, &TARGET, root.path());
        assert!(failure.is_none(), "{failure:?}");

        let warnings = play(&mut log, |_| {});
        let (summary, failure) = log.finish(Duration::from_millis(1_500), Some(1));

        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(failure.is_none(), "{failure:?}");
        assert_eq!(log.directory(), None);
        assert_eq!(log.failed(), 1);
        assert_eq!(summary, "0 passed, 1 failed in 1.5s");
    }

    #[test]
    fn keeps_the_run_record_after_the_run_log_fails() {
        let root = TemporaryDirectory::new("recorder-log-fails").unwrap();
        let (mut log, directory) = open(root.path());

        let warnings = play(&mut log, |log| {
            assert!(log.write(RunRecorder::break_run_log).is_none());
        });

        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings
                .iter()
                .all(|warning| warning.starts_with("run log: "))
        );
        let record = record(&directory);
        assert_eq!(record.pointer("/exit_status"), Some(&1.into()));
        assert_eq!(record.pointer("/tasks/0/state"), Some(&"failed".into()));
        assert_eq!(
            record.pointer("/tasks/0/error"),
            Some(&"entity not found".into())
        );
        // The stream file stays exact, even though the run log failed.
        assert_eq!(
            fs::read_to_string(directory.join("tasks/command.stdout")).unwrap(),
            "hello\n"
        );
    }

    #[test]
    fn keeps_the_run_record_after_the_summary_line_fails() {
        let root = TemporaryDirectory::new("recorder-summary-fails").unwrap();
        let (mut log, directory) = open(root.path());

        assert!(
            log.write(|recorder| recorder.status("Running", "command"))
                .is_none()
        );
        assert!(log.write(RunRecorder::break_run_log).is_none());
        let (_, warning) = log.finish(Duration::ZERO, Some(0));

        assert!(warning.is_some());
        assert_eq!(record(&directory).pointer("/exit_status"), Some(&0.into()));
    }

    #[test]
    fn warns_once_when_the_run_record_fails_too() {
        let root = TemporaryDirectory::new("recorder-everything-fails").unwrap();
        let (mut log, directory) = open(root.path());
        fs::create_dir(directory.join("run.json")).unwrap();

        let warnings = play(&mut log, |log| {
            assert!(log.write(RunRecorder::break_run_log).is_none());
        });

        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn warns_when_only_the_run_record_fails() {
        let root = TemporaryDirectory::new("recorder-record-fails").unwrap();
        let (mut log, directory) = open(root.path());
        fs::create_dir(directory.join("run.json")).unwrap();

        let warnings = play(&mut log, |_| {});

        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let run_log = fs::read_to_string(directory.join("run.log")).unwrap();
        assert!(run_log.contains("Summary"), "{run_log}");
    }
}
