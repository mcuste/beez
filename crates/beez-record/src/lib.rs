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
    Labels, VERB_WIDTH, seconds, stamped_status_line, status_text, stream_mark, trim_newline,
};
pub use log::{LogSettings, RUNS, RunRecorder, RunTarget, ignore_everything, is_run_name};
pub use outcome::{TaskOutcome, TaskState};
pub use recorder::OpenLog;
pub use tally::RunTally;
pub use time::utc;
