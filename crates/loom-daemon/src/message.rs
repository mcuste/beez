//! The lines a command answers with.
//!
//! A command takes effect with or without a running daemon, so both paths
//! answer in the same words.

use std::path::Path;

pub(crate) fn not_watched(manifest: &Path) -> String {
    format!("{} is not watched", manifest.display())
}

/// What `add` answers: the manifest, and whether the daemon is new to it.
pub(crate) fn watching(manifest: &Path, added: bool) -> String {
    format!(
        "{} {}",
        if added { "watching" } else { "read again" },
        manifest.display()
    )
}

pub(crate) fn stopped_watching(manifest: &Path) -> String {
    format!("stopped watching {}", manifest.display())
}

/// What `pause` and `resume` answer, and what the daemon logs.
pub(crate) fn held(job: &str, paused: bool) -> String {
    format!("{} {job}", if paused { "paused" } else { "resumed" })
}

pub(crate) fn taken_id(job: &str) -> String {
    format!("job {job} already comes from another manifest")
}

pub(crate) fn unknown_job(job: &str) -> String {
    format!("no job named {job}")
}

pub(crate) fn unwritable_registry(error: &std::io::Error) -> String {
    format!("cannot write the manifest list: {error}")
}
