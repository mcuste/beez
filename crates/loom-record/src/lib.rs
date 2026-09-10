//! UTC timestamps and the artifacts of one run.
//!
//! Every run writes the same artifacts, whether a person started it or the
//! daemon did, so a finished run stays open to inspection either way.

mod format;
mod log;
mod recorder;
mod time;

pub use format::{
    STDERR_MARK, STDOUT_MARK, VERB_WIDTH, counts, seconds, status_text, trim_newline,
};
pub use log::{LogSettings, RunLog, RunTarget, TaskOutcome};
pub use recorder::RunRecorder;
pub use time::{compact_utc, utc};
