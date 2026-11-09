//! One way to give a command, whether the daemon runs or not.
//!
//! A command that only changes the files takes effect without a daemon, so a
//! manifest can be registered before the daemon starts. A command that needs
//! a running daemon says so.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::control::{self, DaemonStatus, JobId, JobReport, Request, Response};
use crate::error::DaemonError;
use crate::job::CatchUp;
use crate::job_set::JobSet;
use crate::message::{self, Hold, Refusal};
use crate::paths::DaemonPaths;
use crate::store::{self, Registry, Watched};

/// The daemon and its jobs, or the jobs alone when no daemon runs.
#[derive(Debug)]
pub enum Status {
    /// A daemon answered.
    Running(DaemonStatus),
    /// No daemon answered, so the jobs come from the files.
    Stopped(Vec<JobReport>),
}

/// A command that changes what the daemon watches or does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Stops watching the manifest at this path.
    Remove(PathBuf),
    /// Holds one job back until it resumes.
    Pause(JobId),
    /// Lets a paused job fire again.
    Resume(JobId),
    /// Runs one job now, beside its schedule.
    Trigger(JobId),
    /// Reads every watched manifest again.
    Reload,
    /// Stops the daemon once its running runs end.
    Stop,
}

impl From<Command> for Request {
    fn from(command: Command) -> Self {
        match command {
            Command::Remove(manifest) => Self::Remove { manifest },
            Command::Pause(job) => Self::Pause { job },
            Command::Resume(job) => Self::Resume { job },
            Command::Trigger(job) => Self::Trigger { job },
            Command::Reload => Self::Reload,
            Command::Stop => Self::Stop,
        }
    }
}

/// What a command answered: one line for the person who gave it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Answer {
    /// The command took effect.
    Done(String),
    /// The command did not take effect, and the line says why.
    Refused(String),
}

impl From<Refusal> for Answer {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal.to_string())
    }
}

impl From<Result<String, Refusal>> for Answer {
    fn from(answer: Result<String, Refusal>) -> Self {
        answer.map_or_else(Self::from, Self::Done)
    }
}

impl TryFrom<Response> for Answer {
    type Error = DaemonError;

    /// A command is answered with a line. A daemon that answers with its
    /// status instead did not understand the command.
    fn try_from(response: Response) -> Result<Self, DaemonError> {
        match response {
            Response::Done { message } => Ok(Self::Done(message)),
            Response::Error { message } => Ok(Self::Refused(message)),
            Response::Status(_) => Err(DaemonError::UnexpectedAnswer),
        }
    }
}

impl TryFrom<Response> for DaemonStatus {
    type Error = DaemonError;

    /// A status request is answered with the status, and any line in its place
    /// says why there is none.
    fn try_from(response: Response) -> Result<Self, DaemonError> {
        match response {
            Response::Status(status) => Ok(status),
            Response::Done { message } | Response::Error { message } => {
                Err(DaemonError::NoStatus(message))
            }
        }
    }
}

/// Reads the state of the daemon of one Beez root.
pub fn status(paths: &DaemonPaths) -> Result<Status, DaemonError> {
    if control::is_running(paths.socket()) {
        let status = DaemonStatus::try_from(control::send(paths.socket(), &Request::Status)?)?;
        return Ok(Status::Running(status));
    }

    reports(paths).map(Status::Stopped)
}

/// Sends one command, or applies it to the files when no daemon runs.
pub fn command(paths: &DaemonPaths, command: Command) -> Result<Answer, DaemonError> {
    if control::is_running(paths.socket()) {
        // The daemon runs elsewhere, so the path it gets must not depend on
        // the directory the command runs in.
        let command = match command {
            Command::Remove(manifest) => Command::Remove(watched_path(
                &Registry::load(&paths.manifests())?,
                &manifest,
            )),
            command => command,
        };
        return send(paths, &command.into());
    }
    match command {
        Command::Remove(manifest) => remove(paths, &manifest),
        Command::Pause(job) => hold(paths, &job, Hold::Pause),
        Command::Resume(job) => hold(paths, &job, Hold::Resume),
        Command::Trigger(_) | Command::Reload | Command::Stop => Ok(Refusal::NeedsDaemon.into()),
    }
}

/// Watches one more manifest.
///
/// The manifest must load and hold a schedule, so a mistake shows up here and
/// not at three in the morning.
pub fn add(paths: &DaemonPaths, manifest: &Path) -> Result<Answer, DaemonError> {
    let path = std::fs::canonicalize(manifest)?;
    let Some(parent) = path.parent() else {
        return Ok(Answer::Refused(format!(
            "{} has no directory",
            path.display()
        )));
    };
    let working_directory = parent.to_path_buf();
    // A running daemon reads the manifest itself and checks the job IDs
    // against the jobs it holds.
    if control::is_running(paths.socket()) {
        return send(
            paths,
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
    if let Err(refusal) = JobSet::build(&registry, &states).admit(&watched, &states) {
        return Ok(refusal.into());
    }
    write_registry(paths, registry, watched)
}

/// Sends one request to the running daemon, and reads its line.
fn send(paths: &DaemonPaths, request: &Request) -> Result<Answer, DaemonError> {
    control::send(paths.socket(), request)?.try_into()
}

fn write_registry(
    paths: &DaemonPaths,
    mut registry: Registry,
    watched: Watched,
) -> Result<Answer, DaemonError> {
    paths.create()?;
    let watching = registry.watch(watched)?;

    Ok(Answer::Done(format!(
        "{watching}, and the daemon reads it when it starts"
    )))
}

fn remove(paths: &DaemonPaths, manifest: &Path) -> Result<Answer, DaemonError> {
    let (mut registry, mut states) = store::load_all(paths)?;
    let path = watched_path(&registry, manifest);
    let answer = registry.unwatch(&path)?;
    // The same rule as the daemon's, so the state of the manifest goes too.
    if answer.is_ok() && states.keep_watched(&registry) {
        states.save()?;
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

fn hold(paths: &DaemonPaths, id: &JobId, hold: Hold) -> Result<Answer, DaemonError> {
    let (registry, mut states) = store::load_all(paths)?;
    if !JobSet::build(&registry, &states).contains(id) {
        return Ok(Refusal::UnknownJob(id.clone()).into());
    }
    paths.create()?;
    states.update(id, |state| state.paused = hold.paused())?;

    Ok(Answer::Done(message::held(id, hold)))
}

/// Reports every job of a Beez root without a running daemon.
fn reports(paths: &DaemonPaths) -> Result<Vec<JobReport>, DaemonError> {
    let (registry, states) = store::load_all(paths)?;
    let mut jobs = JobSet::build(&registry, &states);
    let now = SystemTime::now();
    for job in jobs.iter_mut() {
        job.plan(now, CatchUp::Refuse);
    }

    Ok(jobs.reports())
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::thread;

    use beez_test_support::TemporaryDirectory;

    use super::{Answer, Command, command, status};
    use crate::control::{self, DaemonStatus, Request, Response};
    use crate::paths::DaemonPaths;
    use crate::store::{Registry, States, Watched, testing};

    /// A stand-in daemon on the socket of `paths` that answers the first
    /// command it reads with `response`, and hands back that command. The
    /// client looks for a daemon first, and that connection holds none.
    fn stand_in(paths: &DaemonPaths, response: Response) -> thread::JoinHandle<Request> {
        let listener = UnixListener::bind(paths.socket()).unwrap();
        thread::spawn(move || {
            loop {
                let (stream, _) = listener.accept().unwrap();
                if let Ok(request) = control::read_request(&stream) {
                    control::write_line(&stream, &response).unwrap();
                    return request;
                }
            }
        })
    }

    #[test]
    fn removing_a_manifest_without_a_daemon_drops_only_its_own_state() {
        let directory = TemporaryDirectory::new("daemon-client-remove-state").unwrap();
        let paths = DaemonPaths::new(directory.join("root"));
        let watching = testing::watch_three(&directory, &paths);

        let answer = command(&paths, Command::Remove(watching.weekly)).unwrap();

        assert!(matches!(answer, Answer::Done(_)), "{answer:?}");
        testing::assert_only_weekly_dropped(&States::load(&paths.state()).unwrap());
    }

    #[test]
    fn says_a_command_needs_the_daemon_in_the_same_words() {
        let directory = TemporaryDirectory::new("daemon-client-needs").unwrap();
        let paths = DaemonPaths::new(directory.join("root"));

        for request in [
            Command::Reload,
            Command::Stop,
            Command::Trigger("nightly".to_owned().into()),
        ] {
            assert_eq!(
                command(&paths, request).unwrap(),
                Answer::Refused("the daemon is not running".to_owned())
            );
        }
    }

    #[test]
    fn sends_the_watched_path_of_a_manifest_to_a_running_daemon() {
        let directory = TemporaryDirectory::new("daemon-client-remove").unwrap();
        let manifest = directory.write("nightly.yaml", "").unwrap();
        directory.subdirectory("nested").unwrap();
        let canonical = std::fs::canonicalize(&manifest).unwrap();
        let paths = DaemonPaths::new(directory.join("root"));
        paths.create().unwrap();
        let mut registry = Registry::load(&paths.manifests()).unwrap();
        registry.add(Watched {
            path: canonical.clone(),
            working_directory: directory.path().to_path_buf(),
        });
        registry.save().unwrap();
        let daemon = stand_in(&paths, Response::done("removed"));

        let answer = command(
            &paths,
            Command::Remove(directory.join("nested/../nightly.yaml")),
        )
        .unwrap();

        assert_eq!(answer, Answer::Done("removed".to_owned()));
        assert_eq!(
            daemon.join().unwrap(),
            Request::Remove {
                manifest: canonical
            }
        );
    }

    #[test]
    fn takes_a_status_in_answer_to_a_command_for_a_failure() {
        let directory = TemporaryDirectory::new("daemon-client-stray").unwrap();
        let paths = DaemonPaths::new(directory.join("root"));
        paths.create().unwrap();
        let daemon = stand_in(
            &paths,
            Response::Status(DaemonStatus {
                pid: 7,
                started: "2026-09-10T03:00:00Z".to_owned(),
                running: 0,
                jobs: Vec::new(),
            }),
        );

        let error = command(&paths, Command::Reload).unwrap_err();

        assert_eq!(error.to_string(), "unexpected answer from the daemon");
        assert_eq!(daemon.join().unwrap(), Request::Reload);
    }

    #[test]
    fn takes_a_line_in_answer_to_a_status_request_for_a_failure() {
        let directory = TemporaryDirectory::new("daemon-client-status").unwrap();
        let paths = DaemonPaths::new(directory.join("root"));
        paths.create().unwrap();
        let daemon = stand_in(&paths, Response::error("busy"));

        let error = status(&paths).unwrap_err();

        assert_eq!(error.to_string(), "busy");
        assert_eq!(daemon.join().unwrap(), Request::Status);
    }
}
