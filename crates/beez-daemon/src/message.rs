//! The lines a command answers with.
//!
//! A command takes effect with or without a running daemon, so both paths
//! answer in the same words.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::control::JobId;

/// Why a command that names a manifest or a job did not take effect.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Refusal {
    /// The manifest does not load, or names no schedule.
    Broken(String),
    /// Another manifest already gives a job this ID.
    Taken(JobId),
    /// The manifest is not in the list.
    NotWatched(PathBuf),
    /// No job has this ID.
    UnknownJob(JobId),
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

pub(crate) fn unwritable_registry(error: &std::io::Error) -> String {
    format!("cannot write the manifest list: {error}")
}
