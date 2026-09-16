//! Writes one run's artifacts from the events of that run.
//!
//! Loom writes the same lines whether a person started the run or the daemon
//! did, because both send their events through here.

use std::io;
use std::path::Path;

use loom_runner::RunEvent;

use crate::format::event_status;
use crate::log::{LogSettings, RunLog, RunTarget, TaskOutcome};

/// Turns the events of one run into its artifacts.
#[derive(Debug)]
pub struct RunRecorder {
    log: RunLog,
    labels: Vec<String>,
}

impl RunRecorder {
    /// Opens the artifacts of one run. Returns nothing when they are off.
    pub(crate) fn create(
        settings: LogSettings<'_>,
        target: &RunTarget<'_>,
        working_directory: &Path,
    ) -> Option<io::Result<Self>> {
        let labels = target.labels();
        Some(
            settings
                .create(target, working_directory)?
                .map(|log| Self { log, labels }),
        )
    }

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
        if let Some(outcome) = TaskOutcome::of(event) {
            self.log.outcome(index, outcome);
        }

        match event_status(event, &self.label(index)) {
            Some((verb, message)) => self.log.status(verb, &message),
            None => Ok(()),
        }
    }

    /// Records one of Loom's own status lines, such as a sandbox note.
    pub fn status(&mut self, verb: &str, message: &str) -> io::Result<()> {
        self.log.status(verb, message)
    }

    /// Writes the record of the run, with Loom's own exit status.
    pub fn finish(&mut self, exit_status: Option<i32>) -> io::Result<()> {
        self.log.finish(exit_status)
    }

    fn label(&self, index: usize) -> String {
        self.labels
            .get(index)
            .cloned()
            .unwrap_or_else(|| index.to_string())
    }
}

/// The run log of one run, which may be absent or closed.
///
/// A write failure closes the log and the run goes on, because the result of
/// a run matters more than its record.
#[derive(Debug)]
pub struct OpenLog(Option<RunRecorder>);

impl OpenLog {
    /// Opens the artifacts of one run, with the error when they cannot open.
    ///
    /// The run goes on either way, so the caller reports the error and keeps
    /// going.
    pub fn open(
        settings: LogSettings<'_>,
        target: &RunTarget<'_>,
        working_directory: &Path,
    ) -> (Self, Option<io::Error>) {
        match RunRecorder::create(settings, target, working_directory) {
            Some(Ok(recorder)) => (Self(Some(recorder)), None),
            Some(Err(error)) => (Self(None), Some(error)),
            None => (Self(None), None),
        }
    }

    /// The directory that holds the artifacts, while the log is open.
    #[must_use]
    pub fn directory(&self) -> Option<&Path> {
        self.0.as_ref().map(RunRecorder::directory)
    }

    /// Writes to the log. Returns the error and closes the log after a
    /// failure, so the caller reports it once.
    pub fn write(
        &mut self,
        action: impl FnOnce(&mut RunRecorder) -> io::Result<()>,
    ) -> Option<io::Error> {
        let error = action(self.0.as_mut()?).err()?;
        self.0 = None;

        Some(error)
    }
}
