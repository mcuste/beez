//! The shape of one line of `run.log`, shared with Loom's terminal output.

use std::io;
use std::time::Duration;

/// Width of the status word column, as wide as the longest word.
pub const VERB_WIDTH: usize = 8;

/// Solid marks a task's standard output, dashed its standard error.
pub const STDOUT_MARK: &str = "\u{2502}";
/// Dashed marks a task's standard error.
pub const STDERR_MARK: &str = "\u{250a}";

/// One of Loom's own lines: the status word in its own column, then the message.
#[must_use]
pub fn status_line(verb: &str, message: &str) -> String {
    format!("{verb:<VERB_WIDTH$}  {message}")
}

/// Removes one trailing line ending, so a line can go in a column.
#[must_use]
pub fn trim_newline(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// A duration as Loom writes it in a status line.
#[must_use]
pub fn seconds(elapsed: Duration) -> String {
    format!("{:.1}s", elapsed.as_secs_f64())
}

/// How a task ended, in one word.
#[must_use]
pub fn status_text(exit_status: Option<i32>) -> String {
    match exit_status {
        Some(0) => "ok".to_owned(),
        Some(code) => format!("exit {code}"),
        None => "signalled".to_owned(),
    }
}

/// What Loom reports when it cannot write the artifacts of a run.
#[must_use]
pub fn log_failure(error: &io::Error) -> String {
    format!("run log: {error}")
}

/// The counts that close a run.
pub(crate) fn counts(passed: usize, failed: usize, blocked: usize) -> String {
    let mut counts = vec![format!("{passed} passed")];
    if failed > 0 {
        counts.push(format!("{failed} failed"));
    }
    if blocked > 0 {
        counts.push(format!("{blocked} blocked"));
    }

    counts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::{counts, status_text};

    #[test]
    fn names_only_the_counts_a_run_reached() {
        assert_eq!(counts(2, 0, 0), "2 passed");
        assert_eq!(counts(0, 1, 1), "0 passed, 1 failed, 1 blocked");
    }

    #[test]
    fn names_how_a_task_ended() {
        assert_eq!(status_text(Some(0)), "ok");
        assert_eq!(status_text(Some(23)), "exit 23");
        assert_eq!(status_text(None), "signalled");
    }
}
