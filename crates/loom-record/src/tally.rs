//! Counts how the tasks of one run ended.

use std::time::Duration;

use loom_runner::RunEvent;

use crate::format::{counts, seconds};

/// How many tasks of one run passed, failed and never ran.
///
/// Every run counts its tasks the same way, whether a person started it or the
/// daemon did, so both close a run with the same line.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RunTally {
    passed: usize,
    failed: usize,
    blocked: usize,
}

impl RunTally {
    /// Counts one event. An event that does not end a task changes nothing.
    pub fn add(&mut self, event: &RunEvent) {
        match event {
            RunEvent::Finished { output, .. } if output.succeeded() => self.passed += 1,
            RunEvent::Finished { .. } | RunEvent::Failed { .. } => self.failed += 1,
            RunEvent::Blocked { .. } => self.blocked += 1,
            RunEvent::Started { .. } | RunEvent::Output { .. } => {}
        }
    }

    /// How many tasks failed or could not run.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.failed
    }

    /// The counts and the time, as the line that closes a run writes them.
    #[must_use]
    pub fn summary(&self, elapsed: Duration) -> String {
        format!(
            "{} in {}",
            counts(self.passed, self.failed, self.blocked),
            seconds(elapsed)
        )
    }
}
