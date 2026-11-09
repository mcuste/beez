//! The lines a command answers with.
//!
//! A command takes effect with or without a running daemon, so both paths
//! answer in the same words.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::control::JobId;

/// Why a command did not take effect.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// The manifest does not load, or names no schedule.
    Broken(String),
    /// Another manifest already gives a job this ID.
    Taken(JobId),
    /// The manifest is not in the list.
    NotWatched(PathBuf),
    /// No job has this ID.
    UnknownJob(JobId),
    /// The job cannot run, for the reason given.
    CannotRun { job: JobId, reason: String },
    /// A run of the job is going, and a triggered run waits for none.
    AlreadyRunning(JobId),
    /// The daemon already runs as many workflows as it may.
    AtLimit(usize),
    /// The command needs a running daemon, and none answers.
    NeedsDaemon,
    /// The list of watched manifests could not be written.
    Unwritable(io::Error),
    /// The daemon is on its way out, and starts no run.
    Stopping,
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Broken(error) => formatter.write_str(error),
            Self::Taken(job) => write!(formatter, "job {job} already comes from another manifest"),
            Self::NotWatched(manifest) => {
                write!(formatter, "{} is not watched", manifest.display())
            }
            Self::UnknownJob(job) => write!(formatter, "no job named {job}"),
            Self::CannotRun { job, reason } => write!(formatter, "{job} cannot run: {reason}"),
            Self::AlreadyRunning(job) => write!(formatter, "{job} is already running"),
            Self::AtLimit(limit) => write!(formatter, "the daemon already runs {limit} workflows"),
            Self::NeedsDaemon => formatter.write_str("the daemon is not running"),
            Self::Unwritable(error) => write!(formatter, "cannot write the manifest list: {error}"),
            Self::Stopping => formatter.write_str("the daemon is stopping"),
        }
    }
}

/// Whether `add` met a manifest the daemon did not watch yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Watch {
    /// The manifest is new to the list.
    New,
    /// The manifest was in the list already, and is read again.
    Again,
}

/// What `add` answers: the manifest, and whether the daemon is new to it.
pub(crate) fn watching(manifest: &Path, watch: Watch) -> String {
    format!(
        "{} {}",
        match watch {
            Watch::New => "watching",
            Watch::Again => "read again",
        },
        manifest.display()
    )
}

pub(crate) fn stopped_watching(manifest: &Path) -> String {
    format!("stopped watching {}", manifest.display())
}

/// Whether a person holds a job back or lets it go.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Hold {
    Pause,
    Resume,
}

impl Hold {
    /// True when the job stays paused.
    pub(crate) fn paused(self) -> bool {
        self == Self::Pause
    }
}

/// What `pause` and `resume` answer, and what the daemon logs.
pub(crate) fn held(job: &JobId, hold: Hold) -> String {
    format!(
        "{} {job}",
        match hold {
            Hold::Pause => "paused",
            Hold::Resume => "resumed",
        }
    )
}

pub(crate) fn already_running(root: &Path) -> String {
    format!("a daemon already runs for {}", root.display())
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::Refusal;
    use crate::control::JobId;

    #[test]
    fn says_why_a_command_did_not_take_effect() {
        let job = || JobId::new("nightly", None);
        for (refusal, words) in [
            (
                Refusal::CannotRun {
                    job: job(),
                    reason: "no sandbox".to_owned(),
                },
                "nightly cannot run: no sandbox",
            ),
            (Refusal::AlreadyRunning(job()), "nightly is already running"),
            (Refusal::AtLimit(2), "the daemon already runs 2 workflows"),
            (Refusal::NeedsDaemon, "the daemon is not running"),
            (
                Refusal::Unwritable(io::Error::other("disk full")),
                "cannot write the manifest list: disk full",
            ),
            (Refusal::Stopping, "the daemon is stopping"),
        ] {
            assert_eq!(refusal.to_string(), words);
        }
    }
}
