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
mod job;
mod paths;
mod prune;
mod report;
mod run;
mod store;

pub use client::{Status, add, command, status};
pub use control::{JobReport, Request, Response, is_running};
pub use daemon::{DEFAULT_KEPT_RUNS, DEFAULT_RUN_LIMIT, run};
pub use paths::DaemonPaths;
