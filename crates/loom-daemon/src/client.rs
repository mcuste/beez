//! One way to give a command, whether the daemon runs or not.
//!
//! A command that only changes the files takes effect without a daemon, so a
//! manifest can be registered before the daemon starts. A command that needs
//! a running daemon says so.

use std::io;
use std::path::{Path, PathBuf};

use loom_manifest::load;

use crate::control::{self, JobReport, Request, Response};
use crate::daemon::reports;
use crate::job;
use crate::message;
use crate::paths::DaemonPaths;
use crate::store::{Registry, States, Watched};

/// The daemon and its jobs, or the jobs alone when no daemon runs.
#[derive(Debug)]
pub enum Status {
    /// A daemon answered.
    Running(Response),
    /// No daemon answered, so the jobs come from the files.
    Stopped(Vec<JobReport>),
}

/// Reads the state of the daemon of one Loom root.
pub fn status(paths: &DaemonPaths) -> io::Result<Status> {
    if control::is_running(paths.socket()) {
        return control::send(paths.socket(), &Request::Status).map(Status::Running);
    }

    reports(paths).map(Status::Stopped)
}

/// Sends one command, or applies it to the files when no daemon runs.
pub fn command(paths: &DaemonPaths, request: Request) -> io::Result<Response> {
    if control::is_running(paths.socket()) {
        return control::send(paths.socket(), &request);
    }
    match request {
        Request::Remove { manifest } => remove(paths, &manifest),
        Request::Pause { job } => hold(paths, &job, true),
        Request::Resume { job } => hold(paths, &job, false),
        Request::Add { .. }
        | Request::Status
        | Request::Reload
        | Request::Trigger { .. }
        | Request::Stop => Ok(needs_daemon()),
    }
}

/// Watches one more manifest.
///
/// The manifest must load and hold a schedule, so a mistake shows up here and
/// not at three in the morning.
pub fn add(paths: &DaemonPaths, manifest: &Path) -> io::Result<Response> {
    let path = std::fs::canonicalize(manifest)?;
    let Some(parent) = path.parent() else {
        return Ok(Response::error(format!(
            "{} has no directory",
            path.display()
        )));
    };
    let working_directory = parent.to_path_buf();
    if let Err(problem) = check(&path) {
        return Ok(Response::error(problem));
    }
    let watched = Watched {
        path: path.clone(),
        working_directory: working_directory.clone(),
    };
    let states = States::load(&paths.state())?;
    let registry = Registry::load(&paths.manifests())?;
    let ids = job::ids_of(&watched, &states);
    if let Some(taken) = job::conflicting_id(&registry, &states, &path, &ids) {
        return Ok(Response::error(message::taken_id(&taken)));
    }
    if control::is_running(paths.socket()) {
        return control::send(
            paths.socket(),
            &Request::Add {
                manifest: path,
                working_directory,
            },
        );
    }
    write_registry(paths, registry, watched)
}

fn write_registry(
    paths: &DaemonPaths,
    mut registry: Registry,
    watched: Watched,
) -> io::Result<Response> {
    paths.create()?;
    let path = watched.path.clone();
    let added = registry.add(watched);
    registry.save(&paths.manifests())?;

    Ok(Response::done(format!(
        "{}, and the daemon reads it when it starts",
        message::watching(&path, added)
    )))
}

fn remove(paths: &DaemonPaths, manifest: &PathBuf) -> io::Result<Response> {
    // A manifest that is gone cannot be resolved, so both forms are tried.
    let path = std::fs::canonicalize(manifest).unwrap_or_else(|_| manifest.clone());
    let mut registry = Registry::load(&paths.manifests())?;
    if !registry.remove(&path) && !registry.remove(manifest) {
        return Ok(Response::error(message::not_watched(manifest)));
    }
    registry.save(&paths.manifests())?;

    Ok(Response::done(message::stopped_watching(&path)))
}

fn hold(paths: &DaemonPaths, id: &str, paused: bool) -> io::Result<Response> {
    if !reports(paths)?.iter().any(|report| report.id == id) {
        return Ok(Response::error(message::unknown_job(id)));
    }
    let mut states = States::load(&paths.state())?;
    let mut state = states.get(id);
    state.paused = paused;
    states.set(id, state);
    paths.create()?;
    states.save(&paths.state())?;

    Ok(Response::done(message::held(id, paused)))
}

/// Reports why a manifest cannot become a job.
fn check(path: &Path) -> Result<(), String> {
    let manifest = load(path).map_err(|error| error.to_string())?;
    if manifest.schedules().is_empty() {
        return Err(format!(
            "{} has no schedule section, so nothing says when it runs",
            path.display()
        ));
    }

    Ok(())
}

fn needs_daemon() -> Response {
    Response::error("the daemon is not running")
}
