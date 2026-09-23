//! What happened to one task of a run.
//!
//! The run log, the terminal and the counts that close a run all read an
//! event through this type, so they always agree on what a task did.

use std::io;
use std::time::Duration;

use beez_runner::RunEvent;

use crate::format::{seconds, status_text};

/// What happened to one task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskOutcome {
    /// The task has started.
    Started,
    /// The task ran to the end. `None` means a signal ended it.
    Finished {
        /// Status the task exited with.
        exit_status: Option<i32>,
        /// Time the task took.
        elapsed: Duration,
    },
    /// The task could not start at all.
    Failed {
        /// Category of the error that stopped it.
        error: io::ErrorKind,
        /// Time until the task failed.
        elapsed: Duration,
    },
    /// The task never ran, because a dependency failed.
    Blocked,
}

impl TaskOutcome {
    /// What one event says about its task.
    ///
    /// An output line says nothing about how the task ends, so it has none.
    #[must_use]
    pub fn of(event: &RunEvent) -> Option<Self> {
        match event {
            RunEvent::Output { .. } => None,
            RunEvent::Started { .. } => Some(Self::Started),
            RunEvent::Blocked { .. } => Some(Self::Blocked),
            RunEvent::Finished {
                output, elapsed, ..
            } => Some(Self::Finished {
                exit_status: output.status_code(),
                elapsed: *elapsed,
            }),
            RunEvent::Failed {
                error_kind,
                elapsed,
                ..
            } => Some(Self::Failed {
                error: *error_kind,
                elapsed: *elapsed,
            }),
        }
    }

    /// True when the task ran to the end and exited with status zero.
    #[must_use]
    pub fn succeeded(self) -> bool {
        matches!(
            self,
            Self::Finished {
                exit_status: Some(0),
                ..
            }
        )
    }

    /// How the outcome reads as a status line: the status word, then the
    /// message.
    #[must_use]
    pub fn status(self, label: &str) -> (&'static str, String) {
        match self {
            Self::Started => ("Running", label.to_owned()),
            Self::Blocked => ("Blocked", label.to_owned()),
            Self::Finished {
                exit_status,
                elapsed,
            } => {
                let verb = if self.succeeded() {
                    "Finished"
                } else {
                    "Failed"
                };
                let message = format!(
                    "{label} in {} ({})",
                    seconds(elapsed),
                    status_text(exit_status)
                );
                (verb, message)
            }
            Self::Failed { error, elapsed } => (
                "Failed",
                format!("{label} in {} ({error})", seconds(elapsed)),
            ),
        }
    }

    /// The state the run record names for the task.
    pub(crate) fn state(self) -> &'static str {
        match self {
            Self::Started => "running",
            Self::Finished { .. } if self.succeeded() => "finished",
            Self::Finished { .. } | Self::Failed { .. } => "failed",
            Self::Blocked => "blocked",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use super::TaskOutcome;

    #[test]
    fn names_a_task_that_ran_to_the_end_by_its_status() {
        let ok = TaskOutcome::Finished {
            exit_status: Some(0),
            elapsed: Duration::from_millis(1_500),
        };
        let failed = TaskOutcome::Finished {
            exit_status: Some(3),
            elapsed: Duration::from_millis(1_500),
        };

        assert_eq!(
            ok.status("build"),
            ("Finished", "build in 1.5s (ok)".into())
        );
        assert_eq!(
            failed.status("build"),
            ("Failed", "build in 1.5s (exit 3)".into())
        );
        assert_eq!((ok.state(), failed.state()), ("finished", "failed"));
    }

    #[test]
    fn names_a_task_that_never_ran_and_one_that_could_not_start() {
        let blocked = TaskOutcome::Blocked;
        let failed = TaskOutcome::Failed {
            error: io::ErrorKind::NotFound,
            elapsed: Duration::from_millis(200),
        };

        assert_eq!(blocked.status("test"), ("Blocked", "test".into()));
        assert_eq!(
            failed.status("test"),
            ("Failed", "test in 0.2s (entity not found)".into())
        );
        assert_eq!((blocked.state(), failed.state()), ("blocked", "failed"));
    }
}
