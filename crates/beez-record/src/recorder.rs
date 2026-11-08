//! Keeps the artifacts of one run open, and reports their first failure.
//!
//! Beez writes the same lines whether a person started the run or the daemon
//! did, because both send their events through here.

use std::io;
use std::path::Path;

use crate::format::log_failure;
use crate::log::{LogSettings, RunRecorder, RunTarget};

/// The artifacts of one run, which may be absent.
///
/// A file that fails to write takes no more writes, and the run goes on,
/// because the result of a run matters more than its record. The rest of the
/// artifacts, `run.json` among them, are still written.
#[derive(Debug)]
pub struct OpenLog {
    recorder: Option<RunRecorder>,
    warned: bool,
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
    ) -> (Self, Option<String>) {
        match RunRecorder::create(settings, target, working_directory) {
            Ok(recorder) => (
                Self {
                    recorder,
                    warned: false,
                },
                None,
            ),
            Err(error) => (
                Self {
                    recorder: None,
                    warned: true,
                },
                Some(log_failure(&error)),
            ),
        }
    }

    /// The directory that holds the artifacts, when the run writes any.
    #[must_use]
    pub fn directory(&self) -> Option<&Path> {
        self.recorder.as_ref().map(RunRecorder::directory)
    }

    /// Writes to the artifacts, and returns a warning for the first failure
    /// only, so the caller shows it once. Every later failure stays quiet.
    pub fn write(
        &mut self,
        action: impl FnOnce(&mut RunRecorder) -> io::Result<()>,
    ) -> Option<String> {
        let recorder = self.recorder.as_mut()?;
        let failure = action(recorder).err()?;
        if self.warned {
            return None;
        }
        self.warned = true;

        Some(log_failure(&failure))
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
        assert_eq!(failure, None);
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
                line: b"hello\n".to_vec(),
            },
            RunEvent::Failed {
                task: None,
                error_kind: io::ErrorKind::NotFound,
                elapsed: Duration::from_millis(5),
            },
        ];
        let mut warnings: Vec<String> = events
            .iter()
            .filter_map(|event| log.write(|recorder| recorder.event(event)))
            .collect();
        warnings.extend(log.write(|recorder| recorder.finish("0 passed, 1 failed", Some(1))));
        warnings
    }

    fn record(directory: &Path) -> serde_json::Value {
        let text = fs::read_to_string(directory.join("run.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn keeps_the_run_record_after_the_run_log_fails() {
        let root = TemporaryDirectory::new("recorder-log-fails").unwrap();
        let (mut log, directory) = open(root.path());

        let warnings = play(&mut log, |log| {
            assert_eq!(log.write(RunRecorder::break_run_log), None);
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

        assert_eq!(
            log.write(|recorder| recorder.status("Running", "command")),
            None
        );
        assert_eq!(log.write(RunRecorder::break_run_log), None);
        let warning = log.write(|recorder| recorder.finish("0 passed", Some(0)));

        assert!(warning.is_some());
        assert_eq!(record(&directory).pointer("/exit_status"), Some(&0.into()));
    }

    #[test]
    fn warns_once_when_the_run_record_fails_too() {
        let root = TemporaryDirectory::new("recorder-everything-fails").unwrap();
        let (mut log, directory) = open(root.path());
        fs::create_dir(directory.join("run.json")).unwrap();

        let warnings = play(&mut log, |log| {
            assert_eq!(log.write(RunRecorder::break_run_log), None);
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
