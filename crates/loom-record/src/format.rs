//! The shape of one line of `run.log`, shared with Loom's terminal output.

use std::io;
use std::time::{Duration, SystemTime};

use loom_process::OutputStream;

use crate::time::utc;

/// Width of the status word column, as wide as the longest word.
pub const VERB_WIDTH: usize = 8;

/// Solid marks a task's standard output, dashed its standard error.
const STDOUT_MARK: &str = "\u{2502}";
const STDERR_MARK: &str = "\u{250a}";

/// One of Loom's own lines: the status word in its own column, then the message.
#[must_use]
pub fn status_line(verb: &str, message: &str) -> String {
    format!("{verb:<VERB_WIDTH$}  {message}")
}

/// One of Loom's own lines with the time in front, as a log writes it.
#[must_use]
pub fn stamped_status_line(time: SystemTime, verb: &str, message: &str) -> String {
    format!("{} {}", utc(time), status_line(verb, message))
}

/// The mark that stands between a task label and a line of `stream`.
#[must_use]
pub fn stream_mark(stream: OutputStream) -> &'static str {
    match stream {
        OutputStream::Stdout => STDOUT_MARK,
        OutputStream::Stderr => STDERR_MARK,
    }
}

/// The task labels of one run, in declaration order.
///
/// The run log and the terminal both put a label in front of a task's lines,
/// so both take the label and the column width from here.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Labels {
    labels: Vec<String>,
    width: usize,
}

impl Labels {
    /// Labels in declaration order.
    #[must_use]
    pub fn new(labels: Vec<String>) -> Self {
        let width = labels.iter().map(String::len).max().unwrap_or(0);
        Self { labels, width }
    }

    /// How many tasks the run has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    /// True for a run without tasks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// Width of the label column, as wide as the longest label.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// The label of the task at `index`, or the index for a task it does not know.
    #[must_use]
    pub fn get(&self, index: usize) -> String {
        self.labels
            .get(index)
            .cloned()
            .unwrap_or_else(|| index.to_string())
    }
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
pub(crate) fn log_failure(error: &io::Error) -> String {
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
    use super::{Labels, counts, status_text};

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

    #[test]
    fn sizes_the_label_column_to_the_longest_label() {
        let sut = Labels::new(vec!["build".to_owned(), "test".to_owned()]);

        assert_eq!(sut.width(), 5);
        assert_eq!(Labels::default().width(), 0);
    }

    #[test]
    fn names_a_task_it_does_not_know_by_its_index() {
        let sut = Labels::new(vec!["build".to_owned()]);

        assert_eq!(sut.get(0), "build");
        assert_eq!(sut.get(3), "3");
    }
}
