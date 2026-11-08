//! One way to give a command, whether the daemon runs or not.
//!
//! A command that only changes the files takes effect without a daemon, so a
//! manifest can be registered before the daemon starts. A command that needs
//! a running daemon says so.

use std::io;
use std::path::{Path, PathBuf};

use crate::control::{self, DaemonStatus, JobId, JobReport, Request, Response};
use crate::message::{self, Hold, Refusal};
use crate::paths::DaemonPaths;
use crate::store::{self, Registry, Watched};
use crate::{apply, job};

/// The daemon and its jobs, or the jobs alone when no daemon runs.
#[derive(Debug)]
pub enum Status {
    /// A daemon answered.
    Running(DaemonStatus),
    /// No daemon answered, so the jobs come from the files.
    Stopped(Vec<JobReport>),
}

/// Reads the state of the daemon of one Beez root.
pub fn status(paths: &DaemonPaths) -> io::Result<Status> {
    if control::is_running(paths.socket()) {
        return match control::send(paths.socket(), &Request::Status)? {
            Response::Status(status) => Ok(Status::Running(status)),
            Response::Done { message } | Response::Error { message } => {
                Err(io::Error::other(message))
            }
        };
    }

    job::reports(paths).map(Status::Stopped)
}

/// Sends one command, or applies it to the files when no daemon runs.
pub fn command(paths: &DaemonPaths, request: Request) -> io::Result<Response> {
    if control::is_running(paths.socket()) {
        // The daemon runs elsewhere, so the path it gets must not depend on
        // the directory the command runs in.
        let request = match request {
            Request::Remove { manifest } => Request::Remove {
                manifest: watched_path(&Registry::load(&paths.manifests())?, &manifest),
            },
            request => request,
        };
        return control::send(paths.socket(), &request);
    }
    match request {
        Request::Remove { manifest } => remove(paths, &manifest),
        Request::Pause { job } => hold(paths, &job, Hold::Pause),
        Request::Resume { job } => hold(paths, &job, Hold::Resume),
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
    if let Err(refusal) = job::admit(&jobs, others) {
        return Ok(refusal.into());
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

fn remove(paths: &DaemonPaths, manifest: &Path) -> io::Result<Response> {
    let (mut registry, mut states) = store::load_all(paths)?;
    let path = watched_path(&registry, manifest);
    let answer = apply::unwatch(&mut registry, paths, &path)?;
    // The same rule as the daemon's, so the state of the manifest goes too.
    if answer.is_ok() && states.keep_watched(&registry) {
        states.save(&paths.state())?;
    }

    Ok(answer.into())
}

/// The path the list holds for `manifest`: the resolved one when the list
/// watches it, and the path as typed otherwise.
///
/// A manifest that is gone cannot be resolved, so the path as typed counts too.
fn watched_path(registry: &Registry, manifest: &Path) -> PathBuf {
    match std::fs::canonicalize(manifest) {
        Ok(canonical) if registry.watches(&canonical) => canonical,
        _ => manifest.to_path_buf(),
    }
}

fn hold(paths: &DaemonPaths, id: &JobId, hold: Hold) -> io::Result<Response> {
    let (registry, mut states) = store::load_all(paths)?;
    let (_, jobs) = job::build(&registry, &states);
    if !jobs.iter().any(|job| job.id == *id) {
        return Ok(Refusal::UnknownJob(id.clone()).into());
    }
    let mut state = states.get(id);
    state.paused = hold.paused();
    states.set(id, state);
    paths.create()?;
    states.save(&paths.state())?;

    Ok(Response::done(message::held(id, hold)))
}

fn needs_daemon() -> Response {
    Response::error("the daemon is not running")
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::thread;

    use beez_test_support::TemporaryDirectory;

    use super::command;
    use crate::control::{self, Request, Response};
    use crate::paths::DaemonPaths;
    use crate::store::{Registry, States, Watched, testing};

    #[test]
    fn removing_a_manifest_without_a_daemon_drops_only_its_own_state() {
        let directory = TemporaryDirectory::new("daemon-client-remove-state").unwrap();
        let paths = DaemonPaths::new(directory.join("root"));
        let watching = testing::watch_three(&directory, &paths);

        let answer = command(
            &paths,
            Request::Remove {
                manifest: watching.weekly,
            },
        )
        .unwrap();

        assert!(matches!(answer, Response::Done { .. }), "{answer:?}");
        testing::assert_only_weekly_dropped(&States::load(&paths.state()).unwrap());
    }

    #[test]
    fn sends_the_watched_path_of_a_manifest_to_a_running_daemon() {
        let directory = TemporaryDirectory::new("daemon-client-remove").unwrap();
        let manifest = directory.write("nightly.yaml", "").unwrap();
        std::fs::create_dir(directory.join("nested")).unwrap();
        let canonical = std::fs::canonicalize(&manifest).unwrap();
        let paths = DaemonPaths::new(directory.join("root"));
        paths.create().unwrap();
        let mut registry = Registry::default();
        registry.add(Watched {
            path: canonical.clone(),
            working_directory: directory.path().to_path_buf(),
        });
        registry.save(&paths.manifests()).unwrap();
        let listener = UnixListener::bind(paths.socket()).unwrap();
        // A stand-in daemon that answers the first command it reads. The
        // client looks for a daemon first, and that connection holds none.
        let daemon = thread::spawn(move || {
            loop {
                let (stream, _) = listener.accept().unwrap();
                if let Ok(request) = control::read_request(&stream) {
                    control::write_line(&stream, &Response::done("removed")).unwrap();
                    return request;
                }
            }
        });

        let answer = command(
            &paths,
            Request::Remove {
                manifest: directory.join("nested/../nightly.yaml"),
            },
        )
        .unwrap();

        assert_eq!(answer, Response::done("removed"));
        assert_eq!(
            daemon.join().unwrap(),
            Request::Remove {
                manifest: canonical
            }
        );
    }
}
