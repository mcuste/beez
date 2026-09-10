//! Writes one run's artifacts from the events of that run.
//!
//! Loom writes the same lines whether a person started the run or the daemon
//! did, because both send their events through here.

use std::io;
use std::path::Path;

use loom_core::TaskIndex;
use loom_runner::RunEvent;

use crate::format::{seconds, status_text};
use crate::log::{LogSettings, RunLog, RunTarget, TaskOutcome};

/// Turns the events of one run into its artifacts.
#[derive(Debug)]
pub struct RunRecorder {
    log: RunLog,
    labels: Vec<String>,
}

impl RunRecorder {
    /// Opens the artifacts of one run. Returns nothing when they are off.
    pub fn create(
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
        match event {
            RunEvent::Started { task } => {
                let index = position(*task);
                self.log.outcome(index, TaskOutcome::Started);
                let label = self.label(index);
                self.log.status("Running", &label)
            }
            RunEvent::Output { task, stream, line } => {
                self.log.output(position(*task), *stream, line)
            }
            RunEvent::Finished {
                task,
                output,
                elapsed,
            } => {
                let index = position(*task);
                self.log.outcome(
                    index,
                    TaskOutcome::Finished {
                        exit_status: output.status_code(),
                        elapsed: *elapsed,
                    },
                );
                let verb = if output.succeeded() {
                    "Finished"
                } else {
                    "Failed"
                };
                let message = format!(
                    "{} in {} ({})",
                    self.label(index),
                    seconds(*elapsed),
                    status_text(output.status_code())
                );
                self.log.status(verb, &message)
            }
            RunEvent::Failed {
                task,
                error_kind,
                elapsed,
            } => {
                let index = position(*task);
                self.log.outcome(
                    index,
                    TaskOutcome::Failed {
                        error: *error_kind,
                        elapsed: *elapsed,
                    },
                );
                let message = format!(
                    "{} in {} ({error_kind})",
                    self.label(index),
                    seconds(*elapsed)
                );
                self.log.status("Failed", &message)
            }
            RunEvent::Blocked { task } => {
                let index = task.position();
                self.log.outcome(index, TaskOutcome::Blocked);
                let label = self.label(index);
                self.log.status("Blocked", &label)
            }
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

/// The task an event belongs to. A direct request has one task of its own.
fn position(task: Option<TaskIndex>) -> usize {
    task.map_or(0, TaskIndex::position)
}
