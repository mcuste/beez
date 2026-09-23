//! The changes a command makes to the list of watched manifests.
//!
//! A command takes effect with or without a running daemon. Both paths change
//! the list through these steps, so they write the same file and answer in
//! the same words.

use std::io;
use std::path::Path;

use crate::control::Response;
use crate::message;
use crate::paths::DaemonPaths;
use crate::store::{Registry, Watched};

/// Adds a manifest to the list and writes the list.
///
/// Answers with the manifest, and whether the daemon is new to it.
pub(crate) fn watch(
    registry: &mut Registry,
    paths: &DaemonPaths,
    watched: Watched,
) -> io::Result<String> {
    let path = watched.path.clone();
    let added = registry.add(watched);
    registry.save(&paths.manifests())?;

    Ok(message::watching(&path, added))
}

/// Removes a manifest from the list and writes the list.
///
/// Answers with an error when the manifest was not watched.
pub(crate) fn unwatch(
    registry: &mut Registry,
    paths: &DaemonPaths,
    manifest: &Path,
) -> io::Result<Response> {
    if !registry.remove(manifest) {
        return Ok(Response::error(message::not_watched(manifest)));
    }
    registry.save(&paths.manifests())?;

    Ok(Response::done(message::stopped_watching(manifest)))
}
