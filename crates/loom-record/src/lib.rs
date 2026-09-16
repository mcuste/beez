//! UTC timestamps and the artifacts of one run.
//!
//! Every run writes the same artifacts, whether a person started it or the
//! daemon did, so a finished run stays open to inspection either way.

mod format;
mod log;
mod outcome;
mod recorder;
mod tally;
mod time;

pub use format::{
    STDERR_MARK, STDOUT_MARK, VERB_WIDTH, log_failure, seconds, status_line, status_text,
    trim_newline,
};
pub use log::{LogSettings, RUNS, RunTarget, ignore_everything, is_run_name};
pub use outcome::TaskOutcome;
pub use recorder::{OpenLog, RunRecorder};
pub use tally::RunTally;
pub use time::{compact_utc, utc};
