//! Writes one run's artifacts from the events of that run.
//!
//! Beez writes the same lines whether a person started the run or the daemon
//! did, because both send their events through here.

use std::io;
use std::path::Path;

use beez_runner::RunEvent;

use crate::format::log_failure;
use crate::log::{LogSettings, RunLog, RunTarget};
use crate::outcome::TaskOutcome;

/// Turns the events of one run into its artifacts.
#[derive(Debug)]
pub struct RunRecorder {
    log: RunLog,
}

impl RunRecorder {
    /// The directory that holds the artifacts.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.log.directory()
    }

    /// Records one lifecycle or output event.
    pub fn event(&mut self, event: &RunEvent) -> io::Result<()> {
        let index = event.position();
        if let RunEvent::Output { stream, line, .. } = event {
            return self.log.output(index, *stream, line);
        }
        let Some(outcome) = TaskOutcome::of(event) else {
            return Ok(());
        };
        self.log.outcome(index, outcome);
        let (verb, message) = outcome.status(&self.log.labels().get(index));

        self.log.status(verb, &message)
    }

    /// Records one of Beez's own status lines, such as a sandbox note.
    pub fn status(&mut self, verb: &str, message: &str) -> io::Result<()> {
        self.log.status(verb, message)
    }

    /// Writes the line that closes the run, then the record of the run.
    ///
    /// `exit_status` is the status Beez itself returns. It is absent when the
    /// run ended in an execution error.
    pub fn finish(&mut self, summary: &str, exit_status: Option<i32>) -> io::Result<()> {
        self.status("Summary", summary)?;

        self.log.finish(exit_status)
    }
}

/// The run log of one run, which may be absent or closed.
///
/// A write failure closes the log and the run goes on, because the result of
/// a run matters more than its record.
#[derive(Debug)]
pub struct OpenLog(Option<RunRecorder>);

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
        if !settings.enabled {
            return (Self(None), None);
        }
        match RunLog::create(settings.directory, target, working_directory) {
            Ok(log) => (Self(Some(RunRecorder { log })), None),
            Err(error) => (Self(None), Some(log_failure(&error))),
        }
    }

    /// The directory that holds the artifacts, while the log is open.
    #[must_use]
    pub fn directory(&self) -> Option<&Path> {
        self.0.as_ref().map(RunRecorder::directory)
    }

    /// Writes to the log, and closes it after a failure, so the caller shows
    /// the warning it returns once. A closed log ignores every later write.
    pub fn write(
        &mut self,
        action: impl FnOnce(&mut RunRecorder) -> io::Result<()>,
    ) -> Option<String> {
        let recorder = self.0.as_mut()?;
        let failure = action(recorder).err()?;
        self.0 = None;

        Some(log_failure(&failure))
    }
}
