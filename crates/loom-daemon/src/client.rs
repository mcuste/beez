//! One way to give a command, whether the daemon runs or not.
//!
//! A command that only changes the files takes effect without a daemon, so a
//! manifest can be registered before the daemon starts. A command that needs
//! a running daemon says so.

use std::io;
use std::path::{Path, PathBuf};

use crate::control::{self, JobReport, Request, Response};
use crate::daemon::reports;
use crate::paths::DaemonPaths;
use crate::store::{self, Registry, Watched};
use crate::{apply, job, message};

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
    // A running daemon reads the manifest itself and checks the job IDs
    // against the jobs it holds.
    if control::is_running(paths.socket()) {
        return control::send(
            paths.socket(),
            &Request::Add {
                manifest: path,
                working_directory,
            },
        );
    }
    let watched = Watched {
        path,
        working_directory,
    };
    let (registry, states) = store::load_all(paths)?;
    let jobs = job::jobs_of(&watched, &states);
    let others = registry
        .manifests()
        .iter()
        .filter(|other| other.path != watched.path)
        .flat_map(|other| job::ids_of(other, &states));
    if let Err(error) = job::admit(&jobs, others) {
        return Ok(Response::error(error));
    }
    write_registry(paths, registry, watched)
}

fn write_registry(
    paths: &DaemonPaths,
    mut registry: Registry,
    watched: Watched,
) -> io::Result<Response> {
    paths.create()?;
    let watching = apply::watch(&mut registry, paths, watched)?;

    Ok(Response::done(format!(
        "{watching}, and the daemon reads it when it starts"
    )))
}

fn remove(paths: &DaemonPaths, manifest: &PathBuf) -> io::Result<Response> {
    let mut registry = Registry::load(&paths.manifests())?;
    // A manifest that is gone cannot be resolved, so the path as typed counts too.
    let canonical = std::fs::canonicalize(manifest).unwrap_or_else(|_| manifest.clone());
    let path = if registry.watches(&canonical) {
        canonical
    } else {
        manifest.clone()
    };

    apply::unwatch(&mut registry, paths, &path)
}

fn hold(paths: &DaemonPaths, id: &str, paused: bool) -> io::Result<Response> {
    let (registry, mut states) = store::load_all(paths)?;
    let (_, jobs) = job::build(&registry, &states);
    if !jobs.iter().any(|job| job.id == id) {
        return Ok(Response::unknown_job(id));
    }
    let mut state = states.get(id);
    state.paused = paused;
    states.set(id, state);
    paths.create()?;
    states.save(&paths.state())?;

    Ok(Response::done(message::held(id, paused)))
}

fn needs_daemon() -> Response {
    Response::error("the daemon is not running")
}
