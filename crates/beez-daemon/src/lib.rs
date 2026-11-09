//! Runs workflows on a schedule.
//!
//! The daemon watches manifests. Each manifest holds its own `schedule`
//! section, so the file that says what to run also says when to run it. The
//! daemon keeps only the list of manifests it watches and what already
//! happened, and it reads a manifest again every time the file changes and
//! every time a job fires.

mod client;
mod control;
mod daemon;
mod error;
mod job;
mod job_set;
mod message;
mod paths;
mod prune;
mod report;
mod run;
mod start;
mod store;

pub use client::{Answer, Command, Status, add, command, status};
pub use control::{Condition, DaemonStatus, JobId, JobReport, is_running};
pub use daemon::{DEFAULT_KEPT_RUNS, DEFAULT_RUN_LIMIT, RunLimits, run};
pub use error::DaemonError;
pub use paths::DaemonPaths;
pub use start::start;
