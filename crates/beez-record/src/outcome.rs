//! What happened to one task of a run.
//!
//! The run log, the terminal and the counts that close a run all read an
//! event through this type, so they always agree on what a task did.

use std::fmt;
use std::io;
use std::time::Duration;

use beez_runner::RunEvent;
use serde::Serialize;

use crate::format::seconds;

/// How a task that ran to the end exited.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskExit {
    /// The task exited with this status.
    Code(i32),
    /// A signal ended the task.
    Signalled,
}

impl TaskExit {
    /// The status the task exited with, or `None` after a signal.
    #[must_use]
    pub fn code(self) -> Option<i32> {
        match self {
            Self::Code(code) => Some(code),
            Self::Signalled => None,
        }
    }

    /// True when the task exited with status zero.
    #[must_use]
    pub fn succeeded(self) -> bool {
        self == Self::Code(0)
    }
}

impl From<Option<i32>> for TaskExit {
    /// `None` means a signal ended the task.
    fn from(code: Option<i32>) -> Self {
        code.map_or(Self::Signalled, Self::Code)
    }
}

impl fmt::Display for TaskExit {
    /// How a task ended, in one word.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code(0) => formatter.write_str("ok"),
            Self::Code(code) => write!(formatter, "exit {code}"),
            Self::Signalled => formatter.write_str("signalled"),
        }
    }
}

/// What happened to one task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskOutcome {
    /// The task has started.
    Started,
    /// The task ran to the end.
    Finished {
        /// How the task exited.
        exit: TaskExit,
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
    pub fn of(event: &RunEvent<'_>) -> Option<Self> {
        match event {
            RunEvent::Output { .. } => None,
            RunEvent::Started { .. } => Some(Self::Started),
            RunEvent::Blocked { .. } => Some(Self::Blocked),
            RunEvent::Finished {
                output, elapsed, ..
            } => Some(Self::Finished {
                exit: output.status_code().into(),
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
        matches!(self, Self::Finished { exit, .. } if exit.succeeded())
    }

    /// How the outcome reads as a status line: the state, whose verb is the
    /// status word, then the message.
    #[must_use]
    pub fn status(self, label: &str) -> (TaskState, String) {
        let message = match self {
            Self::Started | Self::Blocked => label.to_owned(),
            Self::Finished { exit, elapsed } => {
                format!("{label} in {} ({exit})", seconds(elapsed))
            }
            Self::Failed { error, elapsed } => {
                format!("{label} in {} ({error})", seconds(elapsed))
            }
        };

        (self.state(), message)
    }

    /// The state the task is in after this outcome.
    #[must_use]
    pub fn state(self) -> TaskState {
        match self {
            Self::Started => TaskState::Running,
            Self::Finished { .. } if self.succeeded() => TaskState::Finished,
            Self::Finished { .. } | Self::Failed { .. } => TaskState::Failed,
            Self::Blocked => TaskState::Blocked,
        }
    }
}

/// Where one task of a run stands, as the run record names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    /// The task has not started yet.
    Pending,
    /// The task has started and not ended yet.
    Running,
    /// The task ran to the end and exited with status zero.
    Finished,
    /// The task exited with another status, or could not start at all.
    Failed,
    /// The task never ran, because a dependency failed.
    Blocked,
}

impl TaskState {
    /// The status word a status line shows for the state.
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Running => "Running",
            Self::Finished => "Finished",
            Self::Failed => "Failed",
            Self::Blocked => "Blocked",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use super::{TaskExit, TaskOutcome, TaskState};

    #[test]
    fn names_a_task_that_ran_to_the_end_by_its_status() {
        let ok = TaskOutcome::Finished {
            exit: TaskExit::Code(0),
            elapsed: Duration::from_millis(1_500),
        };
        let failed = TaskOutcome::Finished {
            exit: TaskExit::Code(3),
            elapsed: Duration::from_millis(1_500),
        };

        assert_eq!(
            ok.status("build"),
            (TaskState::Finished, "build in 1.5s (ok)".into())
        );
        assert_eq!(
            failed.status("build"),
            (TaskState::Failed, "build in 1.5s (exit 3)".into())
        );
        assert_eq!(
            (TaskState::Finished.verb(), TaskState::Failed.verb()),
            ("Finished", "Failed")
        );
    }

    #[test]
    fn names_a_task_that_never_ran_and_one_that_could_not_start() {
        let blocked = TaskOutcome::Blocked;
        let failed = TaskOutcome::Failed {
            error: io::ErrorKind::NotFound,
            elapsed: Duration::from_millis(200),
        };

        assert_eq!(blocked.status("test"), (TaskState::Blocked, "test".into()));
        assert_eq!(
            failed.status("test"),
            (TaskState::Failed, "test in 0.2s (entity not found)".into())
        );
        assert_eq!(TaskState::Blocked.verb(), "Blocked");
    }

    #[test]
    fn reads_a_missing_status_as_a_signal() {
        assert_eq!(TaskExit::from(Some(23)), TaskExit::Code(23));
        assert_eq!(TaskExit::from(None), TaskExit::Signalled);
        assert_eq!(TaskExit::Signalled.code(), None);
        assert_eq!(TaskExit::Code(23).code(), Some(23));
    }

    #[test]
    fn names_how_a_task_ended() {
        assert_eq!(TaskExit::Code(0).to_string(), "ok");
        assert_eq!(TaskExit::Code(23).to_string(), "exit 23");
        assert_eq!(TaskExit::Signalled.to_string(), "signalled");
    }

    #[test]
    fn names_a_running_task() {
        assert_eq!(
            TaskOutcome::Started.status("lint"),
            (TaskState::Running, "lint".into())
        );
        assert_eq!(TaskState::Running.verb(), "Running");
    }
}
