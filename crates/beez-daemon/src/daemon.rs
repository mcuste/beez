//! The daemon: a timer, a set of jobs, and a socket that carries commands.
//!
//! One thread holds every decision. The socket listener and the signal
//! handler only send events, and each run reports its result the same way, so
//! the job set needs no lock.

use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime};

use beez_manifest::Overlap;
use beez_schedule::Utc;
use serde::Serialize;

use crate::control::{self, DaemonStatus, JobId, Request, Response};
use crate::error::{DaemonError, one_line};
use crate::job::{self, CatchUp, Job};
use crate::job_set::{JobSet, plan};
use crate::message::{self, Hold, Refusal};
use crate::paths::DaemonPaths;
use crate::report;
use crate::run::{self, JobRun, RunOutcome};
use crate::store::{self, Registry, States, Watched};

/// How many runs the daemon starts at the same time.
///
/// A harness run costs tokens and processor time, so the daemon holds the
/// number down and lets the rest wait.
pub const DEFAULT_RUN_LIMIT: usize = 2;

/// How many runs the daemon keeps in the Beez root.
///
/// A job that fires every hour writes a run every hour, so the daemon sweeps
/// the oldest away. Zero keeps every run.
pub const DEFAULT_KEPT_RUNS: usize = 200;

/// Layout version of `daemon.json`.
const SCHEMA: u32 = 1;

/// Longest the timer sleeps. A clock change or a machine that slept cannot
/// hide a fire for longer than this.
const MAX_SLEEP: Duration = Duration::from_mins(1);

/// How many runs a daemon starts and keeps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunLimits {
    /// How many runs the daemon starts at the same time. Zero counts as one.
    pub limit: usize,
    /// How many runs stay in the Beez root. Zero keeps every run.
    pub keep_runs: usize,
}

/// The record of a running daemon, as `daemon.json` holds it.
#[derive(Debug, Serialize)]
struct DaemonRecord {
    schema: u32,
    pid: u32,
    socket: PathBuf,
    started: String,
}

/// What wakes the timer.
enum Event {
    /// A command arrived, with the connection that waits for the answer.
    Command(Request, UnixStream),
    /// A run ended.
    Finished {
        job: JobId,
        outcome: Box<RunOutcome>,
    },
    /// The daemon was asked to stop.
    Signal,
}

/// What a fire leads to.
#[derive(Debug, Eq, PartialEq)]
enum Action {
    Start,
    /// Waits for room to run. The reason goes in the log.
    Queue(QueueReason),
    Skip,
}

/// Why a fire waits instead of starting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueReason {
    /// The daemon runs as many workflows as it may.
    AtLimit,
    /// The job's last run has not ended, and the job waits for it.
    Overlap,
}

impl fmt::Display for QueueReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AtLimit => "the daemon is at its run limit",
            Self::Overlap => "its last run has not ended",
        })
    }
}

/// How far the daemon is on its way out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    /// Fires jobs and takes commands.
    Serving,
    /// Fires nothing more, and waits for its running runs to end.
    Draining,
    /// Gives up waiting, and leaves its runs behind.
    Exiting,
}

/// Runs the daemon until it is asked to stop.
///
/// One Beez root holds one daemon, and the socket itself keeps a second one
/// out.
pub fn run(paths: &DaemonPaths, limits: RunLimits) -> Result<(), DaemonError> {
    // A service manager gives the daemon a working directory of its own, so
    // the root it resolved from it goes in the message.
    paths
        .create()
        .map_err(|source| DaemonError::CreateDirectory {
            path: paths.directory().clone(),
            source,
        })?;
    let _daemon = tracing::info_span!("daemon", pid = std::process::id()).entered();
    tracing::info!(root = %paths.root().display(), limit = limits.limit, keep_runs = limits.keep_runs, "daemon starting");
    let listener = bind(paths)?;
    tracing::debug!(socket = %paths.socket().display(), "bound the socket");
    let started = SystemTime::now();
    let mut daemon = Daemon::open(paths, limits, started)?;
    write_record(paths, started)?;
    daemon.listen(listener);
    daemon.watch_signals()?;
    beez_sandbox::set_diagnostic_sink(Box::new(|message| report::line("Sandbox", message)));

    report::line(
        "Daemon",
        &format!(
            "started with {} jobs, {} at a time, on {}",
            daemon.jobs.len(),
            daemon.runtime.limit,
            paths.socket().display()
        ),
    );
    daemon.sweep();
    daemon.serve();
    report::line("Daemon", "stopped");

    for file in [paths.socket().to_path_buf(), paths.record()] {
        if let Err(error) = std::fs::remove_file(&file) {
            tracing::debug!(file = %file.display(), %error, "cannot remove on shutdown");
        }
    }
    tracing::info!("daemon stopped");
    Ok(())
}

struct Daemon {
    paths: DaemonPaths,
    registry: Registry,
    states: States,
    jobs: JobSet,
    runtime: Runtime,
    /// How many runs stay in the Beez root. None keeps every run.
    keep: Option<NonZeroUsize>,
    started: SystemTime,
    receiver: Receiver<Event>,
    phase: Phase,
}

/// What starting a run needs, apart from the job.
struct Runtime {
    /// How many runs are going.
    running: usize,
    limit: NonZeroUsize,
    /// Where a run reports that it ended.
    sender: Sender<Event>,
    /// Beez root that holds the artifacts of every run the daemon starts.
    root: PathBuf,
}

impl Runtime {
    /// True while the daemon may start one more run.
    fn has_room(&self) -> bool {
        self.running < self.limit.get()
    }

    /// Starts one run of `job` on its own thread.
    fn start(&mut self, job: &mut Job, fire: SystemTime) {
        job.run_started();
        let run = JobRun {
            id: job.id.clone(),
            manifest: job.manifest.clone(),
            working_directory: job.working_directory.clone(),
            log_directory: self.root.clone(),
            fire,
        };
        self.running += 1;
        report::line("Firing", &format!("{} scheduled for {}", run.id, Utc(fire)));
        tracing::info!(job = %run.id, fire = %Utc(fire), running = self.running, "run starting");

        let sender = self.sender.clone();
        let id = run.id.clone();
        let span = tracing::info_span!("run", job = %run.id);
        thread::spawn(move || {
            let _span = span.entered();
            let outcome = run::execute(&run);
            tracing::info!(result = ?outcome.result, "run ended");
            if sender
                .send(Event::Finished {
                    job: id,
                    outcome: Box::new(outcome),
                })
                .is_err()
            {
                tracing::warn!("the daemon loop is gone, the run's end goes unrecorded");
            }
        });
    }
}

impl Daemon {
    fn open(
        paths: &DaemonPaths,
        limits: RunLimits,
        started: SystemTime,
    ) -> Result<Self, DaemonError> {
        let (registry, states) = store::load_all(paths)?;
        let jobs = JobSet::build(&registry, &states);
        let (sender, receiver) = mpsc::channel();
        let mut daemon = Self {
            paths: paths.clone(),
            registry,
            states,
            jobs,
            runtime: Runtime {
                running: 0,
                limit: NonZeroUsize::new(limits.limit).unwrap_or(NonZeroUsize::MIN),
                sender,
                root: paths.root().to_path_buf(),
            },
            keep: NonZeroUsize::new(limits.keep_runs),
            started,
            receiver,
            phase: Phase::Serving,
        };
        // A daemon that was down may have passed a fire, so this is the one
        // place a job may still run it.
        let now = SystemTime::now();
        for job in daemon.jobs.iter_mut() {
            plan(job, now, CatchUp::Allow);
        }

        Ok(daemon)
    }

    /// Takes commands off the socket on their own thread.
    fn listen(&self, listener: UnixListener) {
        let sender = self.runtime.sender.clone();
        let span = tracing::Span::current();
        thread::spawn(move || {
            let _span = span.entered();
            for stream in listener.incoming() {
                let stream = match stream {
                    Ok(stream) => stream,
                    Err(error) => {
                        tracing::debug!(%error, "accept on the socket failed");
                        continue;
                    }
                };
                match control::read_request(&stream) {
                    Ok(request) => {
                        tracing::debug!(?request, "socket request");
                        if sender.send(Event::Command(request, stream)).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        // A liveness probe connects and sends nothing, so this is common.
                        tracing::trace!(%error, "unreadable socket request");
                        let response = Response::error(one_line(&error));
                        answer(&stream, &response);
                    }
                }
            }
        });
    }

    /// Stops on the signals a service manager sends.
    fn watch_signals(&self) -> io::Result<()> {
        use signal_hook::consts::signal::{SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;

        let mut signals = Signals::new([SIGINT, SIGTERM])?;
        let sender = self.runtime.sender.clone();
        thread::spawn(move || {
            for _ in &mut signals {
                if sender.send(Event::Signal).is_err() {
                    return;
                }
            }
        });

        Ok(())
    }

    fn serve(&mut self) {
        while !self.is_done() {
            let now = SystemTime::now();
            self.rescan(now);
            self.fire_due(now);

            let sleep = self.sleep(now);
            tracing::trace!(?sleep, running = self.runtime.running, "waiting");
            match self.receiver.recv_timeout(sleep) {
                Ok(event) => self.handle(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// True once the daemon may stop.
    fn is_done(&self) -> bool {
        match self.phase {
            Phase::Serving => false,
            Phase::Draining => self.runtime.running == 0,
            Phase::Exiting => true,
        }
    }

    /// How long to wait before the next fire, at most one minute.
    fn sleep(&self, now: SystemTime) -> Duration {
        let next = self
            .jobs
            .iter()
            .filter(|job| job.is_ready())
            .filter_map(|job| job.next_fire)
            .filter_map(|fire| fire.duration_since(now).ok())
            .min();

        next.unwrap_or(MAX_SLEEP).min(MAX_SLEEP)
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Command(request, stream) => {
                let response = self.command(request);
                tracing::trace!(?response, "socket reply");
                answer(&stream, &response);
            }
            Event::Finished { job, outcome } => self.finished(&job, &outcome),
            Event::Signal => self.signal(),
        }
    }

    fn signal(&mut self) {
        tracing::info!(phase = ?self.phase, "signal received");
        match self.phase {
            Phase::Serving => {
                self.begin_stopping();
            }
            Phase::Draining | Phase::Exiting => {
                self.phase = Phase::Exiting;
                report::line("Daemon", "exiting now, leaving its runs behind");
            }
        }
    }

    /// Stops firing, and says how many runs the daemon waits for.
    fn begin_stopping(&mut self) -> String {
        self.phase = Phase::Draining;
        let waiting = waiting_message(self.runtime.running);
        report::line("Daemon", &waiting);
        waiting
    }

    fn command(&mut self, request: Request) -> Response {
        let now = SystemTime::now();
        match request {
            Request::Status => Response::Status(DaemonStatus {
                pid: std::process::id(),
                started: Utc(self.started).to_string(),
                running: self.runtime.running,
                jobs: self.jobs.reports(),
            }),
            Request::Add {
                manifest,
                working_directory,
            } => self.add(manifest, working_directory, now),
            Request::Remove { manifest } => self.remove(&manifest),
            Request::Pause { job } => self.hold(&job, Hold::Pause, now),
            Request::Resume { job } => self.hold(&job, Hold::Resume, now),
            Request::Trigger { job } => self.trigger(&job, now),
            Request::Reload => {
                let count = self.reload_all(now);
                Response::done(format!("read {count} manifests again"))
            }
            Request::Stop => Response::done(self.begin_stopping()),
        }
    }

    fn add(&mut self, manifest: PathBuf, working_directory: PathBuf, now: SystemTime) -> Response {
        let watched = Watched {
            path: manifest,
            working_directory,
        };
        let jobs = match self.jobs.admit(&watched, &self.states) {
            Ok(jobs) => jobs,
            Err(refusal) => return refusal.into(),
        };
        let names = jobs
            .iter()
            .map(|job| job.id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let watching = match self.registry.watch(watched.clone()) {
            Ok(watching) => watching,
            Err(error) => return Refusal::Unwritable(error).into(),
        };
        self.jobs.install(&watched, jobs, now);
        report::line(
            "Watching",
            &format!("{} as {names}", watched.path.display()),
        );

        Response::done(format!("{watching} as {names}"))
    }

    fn remove(&mut self, manifest: &Path) -> Response {
        let stopped = match self.registry.unwatch(manifest) {
            Ok(Ok(stopped)) => stopped,
            Ok(Err(refusal)) => return refusal.into(),
            Err(error) => return Refusal::Unwritable(error).into(),
        };
        // A run of this manifest keeps going, and reports itself as it ends.
        self.jobs.retain(|path| path != manifest);
        self.prune_states();
        report::line("Watching", &format!("dropped {}", manifest.display()));

        Response::done(stopped)
    }

    fn hold(&mut self, id: &JobId, hold: Hold, now: SystemTime) -> Response {
        let Some(job) = self.jobs.find_mut(id) else {
            return Refusal::UnknownJob(id.clone()).into();
        };
        job.state.paused = hold.paused();
        save_state(&mut self.states, job);
        plan(job, now, CatchUp::Refuse);
        let answer = message::held(id, hold);
        report::line("Job", &answer);

        Response::done(answer)
    }

    fn trigger(&mut self, id: &JobId, now: SystemTime) -> Response {
        let Some(job) = self.jobs.find_mut(id) else {
            return Refusal::UnknownJob(id.clone()).into();
        };
        // A run started now would hold the daemon back from stopping.
        if self.phase != Phase::Serving {
            return Refusal::Stopping.into();
        }
        if let Some(error) = job.error() {
            return Refusal::CannotRun {
                job: id.clone(),
                reason: error.to_owned(),
            }
            .into();
        }
        if job.is_running() {
            return Refusal::AlreadyRunning(id.clone()).into();
        }
        if !self.runtime.has_room() {
            return Refusal::AtLimit(self.runtime.limit.get()).into();
        }
        // A triggered run stands beside the schedule, so it moves no fire.
        self.runtime.start(job, now);

        Response::done(format!("running {id} now"))
    }

    /// Reads a manifest again when the file changed.
    fn rescan(&mut self, now: SystemTime) {
        for watched in &self.jobs.changed() {
            tracing::debug!(manifest = %watched.path.display(), "manifest changed, reloading");
            report::line("Reload", &format!("{} changed", watched.path.display()));
            self.reload(watched, now);
        }
    }

    /// Reads the list of watched manifests again, and then each manifest.
    ///
    /// The list is a file, so something other than this daemon may have
    /// changed it.
    fn reload_all(&mut self, now: SystemTime) -> usize {
        match Registry::load(&self.paths.manifests()) {
            Ok(registry) => self.registry = registry,
            Err(error) => report::line(
                "Warning",
                &format!("cannot read the manifest list: {error}"),
            ),
        }
        let watched: Vec<Watched> = self.registry.manifests().to_vec();
        // A manifest that left the list keeps its running run, and nothing else.
        self.jobs
            .retain(|path| watched.iter().any(|entry| entry.path == path));
        for entry in &watched {
            self.reload(entry, now);
        }

        watched.len()
    }

    /// Reads one manifest again and replaces its jobs.
    fn reload(&mut self, watched: &Watched, now: SystemTime) {
        let fresh = job::jobs_of(watched, &self.states);
        tracing::debug!(manifest = %watched.path.display(), jobs = fresh.len(), "reloaded manifest");
        self.jobs.install(watched, fresh, now);
    }

    fn fire_due(&mut self, now: SystemTime) {
        if self.phase != Phase::Serving {
            return;
        }
        for job in self.jobs.iter_mut() {
            if job.is_ready() && job.next_fire.is_some_and(|fire| fire <= now) {
                fire(job, &mut self.runtime, &mut self.states, now);
            }
        }
    }

    fn finished(&mut self, id: &JobId, outcome: &RunOutcome) {
        self.runtime.running = self.runtime.running.saturating_sub(1);
        tracing::debug!(job = %id, running = self.runtime.running, "run finished");
        match &outcome.result {
            Err(error) => report::line("Failed", &format!("{id}: {error}")),
            Ok(0) => report::line("Finished", &format!("{id}: {}", outcome.summary)),
            Ok(status) => report::line(
                "Failed",
                &format!(
                    "{id}: {} ({})",
                    outcome.summary,
                    beez_record::TaskExit::Code(*status)
                ),
            ),
        }
        // A manifest that stopped being watched while it ran has no job left.
        if let Some(job) = self.jobs.find_mut(id) {
            job.run_ended(outcome);
            save_state(&mut self.states, job);
        }
        self.sweep();
        self.start_queued();
    }

    /// Removes the runs the root no longer keeps.
    fn sweep(&self) {
        match crate::prune::prune(self.paths.root(), self.keep) {
            Ok(0) => {}
            Ok(removed) => report::line("Swept", &format!("removed {removed} old runs")),
            Err(error) => report::line("Warning", &format!("cannot sweep the runs: {error}")),
        }
    }

    /// Starts waiting fires while the daemon has room for them.
    fn start_queued(&mut self) {
        if self.phase != Phase::Serving {
            return;
        }
        while self.runtime.has_room() {
            let Some((job, fire)) = self.jobs.iter_mut().find_map(|job| {
                let fire = job.queued?;
                (job.is_ready() && job.may_start()).then_some((job, fire))
            }) else {
                return;
            };
            tracing::debug!(job = %job.id, fire = %Utc(fire), "starting a queued fire");
            self.runtime.start(job, fire);
        }
    }

    fn prune_states(&mut self) {
        self.states.keep_watched(&self.registry);
        warn_unsaved(self.states.save());
    }
}

/// Starts, queues or drops one due fire, then plans the fire after it.
fn fire(job: &mut Job, runtime: &mut Runtime, states: &mut States, now: SystemTime) {
    let fire = job.next_fire.unwrap_or(now);
    let action = action(job, runtime.running, runtime.limit);
    tracing::debug!(job = %job.id, fire = %Utc(fire), ?action, "job due");
    // The fire happened, whatever comes of it, so the schedule moves on.
    job.state.last_fire = Some(fire);
    match action {
        Action::Start => runtime.start(job, fire),
        Action::Queue(reason) => queue(job, reason, fire),
        Action::Skip => report::line(
            "Skipped",
            &format!(
                "{} scheduled for {}, its last run has not ended",
                job.id,
                Utc(fire)
            ),
        ),
    }
    save_state(states, job);
    plan(job, now, CatchUp::Refuse);
}

/// What a fire of `job` leads to while `running` runs of at most `limit` go.
fn action(job: &Job, running: usize, limit: NonZeroUsize) -> Action {
    if running >= limit.get() {
        // The limit never drops work, so a fire waits instead.
        return Action::Queue(QueueReason::AtLimit);
    }
    if !job.is_running() {
        return Action::Start;
    }
    match job.on_overlap() {
        Overlap::Skip => Action::Skip,
        Overlap::Queue => Action::Queue(QueueReason::Overlap),
        Overlap::Parallel => Action::Start,
    }
}

fn queue(job: &mut Job, reason: QueueReason, fire: SystemTime) {
    // One waiting fire per job, so a stuck job cannot pile them up.
    if job.queued.is_some() {
        tracing::debug!(job = %job.id, %reason, "dropped a fire, one already waits");
        report::line("Dropped", &format!("{}, a fire is already waiting", job.id));
        return;
    }
    job.queued = Some(fire);
    tracing::debug!(job = %job.id, %reason, "queued a fire");
    report::line("Queued", &format!("{}, {reason}", job.id));
}

/// Records the state of one job, and logs a state that could not be written.
fn save_state(states: &mut States, job: &Job) {
    warn_unsaved(states.record(&job.id, job.state.clone()));
}

/// Logs a state that could not be written. The daemon goes on regardless.
fn warn_unsaved(saved: io::Result<()>) {
    if let Err(error) = saved {
        report::line("Warning", &format!("cannot write the state: {error}"));
    }
}

/// Writes the reply to one socket request. A client that hung up misses it.
fn answer(stream: &UnixStream, response: &Response) {
    if let Err(error) = control::write_line(stream, response) {
        tracing::trace!(%error, "cannot write the socket reply");
    }
}

/// Binds the socket, which also keeps a second daemon out.
fn bind(paths: &DaemonPaths) -> Result<UnixListener, DaemonError> {
    match UnixListener::bind(paths.socket()) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            if control::is_running(paths.socket()) {
                return Err(DaemonError::AlreadyRunning(paths.root().to_path_buf()));
            }
            // Nothing answers, so the socket belongs to a daemon that died.
            tracing::info!(socket = %paths.socket().display(), "removing a stale socket");
            std::fs::remove_file(paths.socket())?;
            Ok(UnixListener::bind(paths.socket())?)
        }
        Err(error) => Err(error.into()),
    }
}

fn write_record(paths: &DaemonPaths, started: SystemTime) -> io::Result<()> {
    let record = DaemonRecord {
        schema: SCHEMA,
        pid: std::process::id(),
        socket: paths.socket().to_path_buf(),
        started: Utc(started).to_string(),
    };

    store::save_json(&paths.record(), &record)
}

fn waiting_message(running: usize) -> String {
    match running {
        0 => "stopping".to_owned(),
        1 => "stopping after 1 running run ends".to_owned(),
        count => format!("stopping after {count} running runs end"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use beez_test_support::TemporaryDirectory;

    use std::num::NonZeroUsize;

    use super::{Action, Daemon, QueueReason, RunLimits, action};
    use crate::control::{Condition, JobId, Response};
    use crate::job::testing::{at, job};
    use crate::paths::DaemonPaths;
    use crate::store::{Registry, States, Watched, testing};

    const LIMIT: NonZeroUsize = NonZeroUsize::new(2).unwrap();

    /// What a fire leads to for a job with `on_overlap`, while `running` of
    /// the daemon's runs go and the job itself runs or not.
    fn act(on_overlap: &str, job_running: bool, running: usize) -> Action {
        let mut job = job(&format!(
            "  cron: \"0 3 * * *\"\n  on_overlap: {on_overlap}\n  allow_unsandboxed: true\n"
        ));
        if job_running {
            job.run_started();
        }

        action(&job, running, LIMIT)
    }

    #[test]
    fn starts_a_fire_of_a_job_that_does_not_run() {
        for overlap in ["skip", "queue", "parallel"] {
            assert_eq!(act(overlap, false, 0), Action::Start, "{overlap}");
            assert_eq!(
                act(overlap, false, LIMIT.get() - 1),
                Action::Start,
                "{overlap}"
            );
        }
    }

    #[test]
    fn follows_the_overlap_policy_of_a_job_that_runs() {
        assert_eq!(act("skip", true, 1), Action::Skip);
        assert_eq!(act("queue", true, 1), Action::Queue(QueueReason::Overlap));
        assert_eq!(act("parallel", true, 1), Action::Start);
    }

    #[test]
    fn queues_every_fire_at_the_run_limit() {
        for overlap in ["skip", "queue", "parallel"] {
            for job_running in [false, true] {
                assert_eq!(
                    act(overlap, job_running, LIMIT.get()),
                    Action::Queue(QueueReason::AtLimit),
                    "{overlap} {job_running}"
                );
            }
        }
    }

    #[test]
    fn says_why_a_fire_waits() {
        assert_eq!(
            QueueReason::AtLimit.to_string(),
            "the daemon is at its run limit"
        );
        assert_eq!(
            QueueReason::Overlap.to_string(),
            "its last run has not ended"
        );
    }

    #[test]
    fn skips_a_fire_of_a_running_job_that_names_no_policy() {
        let mut job = job("  cron: \"0 3 * * *\"\n  allow_unsandboxed: true\n");
        job.run_started();

        assert_eq!(action(&job, 1, LIMIT), Action::Skip);
    }

    const TASKS: &str = "tasks:\n  - id: mark\n    sandbox: false\n    command: [\"true\"]\n";

    /// A daemon that watches one manifest whose `schedule` section holds
    /// `schedule`, with the directory that holds both.
    fn daemon_with(name: &str, schedule: &str) -> (TemporaryDirectory, DaemonPaths, Daemon) {
        let directory = TemporaryDirectory::new(name).unwrap();
        let paths = DaemonPaths::new(directory.join("root"));
        paths.create().unwrap();
        let manifest = directory
            .write("nightly.yaml", format!("schedule:\n{schedule}{TASKS}"))
            .unwrap();
        let mut registry = Registry::load(&paths.manifests()).unwrap();
        registry.add(Watched {
            path: manifest,
            working_directory: directory.path().to_path_buf(),
        });
        registry.save().unwrap();
        let limits = RunLimits {
            limit: LIMIT.get(),
            keep_runs: 0,
        };
        let daemon = Daemon::open(&paths, limits, SystemTime::now()).unwrap();

        (directory, paths, daemon)
    }

    const NIGHTLY: &str = "  cron: \"0 3 * * *\"\n  allow_unsandboxed: true\n";

    fn nightly() -> JobId {
        JobId::from("nightly".to_owned())
    }

    fn error_of(response: Response) -> String {
        match response {
            Response::Error { message } => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_reload_keeps_what_is_going_on() {
        let (_directory, _paths, mut daemon) = daemon_with("daemon-reload-keeps", NIGHTLY);
        let fire = at("2030-01-01T03:00:00Z");
        let job = daemon.jobs.find_mut(&nightly()).unwrap();
        job.run_started();
        job.queued = Some(fire);
        job.next_fire = Some(fire);

        assert_eq!(daemon.reload_all(SystemTime::now()), 1);

        let job = daemon.jobs.find_mut(&nightly()).unwrap();
        assert!(job.is_running());
        assert_eq!(job.queued, Some(fire));
        assert_eq!(job.next_fire, Some(fire));
    }

    #[test]
    fn refuses_to_trigger_a_job_in_the_same_words() {
        let (_directory, _paths, mut daemon) = daemon_with("daemon-trigger-words", NIGHTLY);
        let now = SystemTime::now();

        assert_eq!(
            error_of(daemon.trigger(&JobId::from("weekly".to_owned()), now)),
            "no job named weekly"
        );
        daemon.runtime.running = LIMIT.get();
        assert_eq!(
            error_of(daemon.trigger(&nightly(), now)),
            "the daemon already runs 2 workflows"
        );
        daemon.jobs.find_mut(&nightly()).unwrap().run_started();
        assert_eq!(
            error_of(daemon.trigger(&nightly(), now)),
            "nightly is already running"
        );
    }

    #[test]
    fn refuses_to_trigger_a_broken_job_in_the_same_words() {
        let (_directory, _paths, mut daemon) =
            daemon_with("daemon-trigger-broken", "  cron: \"0 3 * * *\"\n");

        assert_eq!(
            error_of(daemon.trigger(&nightly(), SystemTime::now())),
            "nightly cannot run: every task must set a sandbox, or the schedule must set allow_unsandboxed: true"
        );
    }

    #[test]
    fn refuses_to_trigger_a_job_while_it_stops() {
        let (_directory, _paths, mut daemon) = daemon_with("daemon-trigger-stopping", NIGHTLY);
        daemon.begin_stopping();

        assert_eq!(
            error_of(daemon.trigger(&nightly(), SystemTime::now())),
            "the daemon is stopping"
        );
        assert_eq!(daemon.runtime.running, 0);
    }

    #[test]
    fn starts_a_waiting_fire_of_a_parallel_job_beside_its_run() {
        let (_directory, _paths, mut daemon) = daemon_with(
            "daemon-queued-parallel",
            "  cron: \"0 3 * * *\"\n  on_overlap: parallel\n  allow_unsandboxed: true\n",
        );
        let now = SystemTime::now();
        // The job runs, and a fire of it waits because the daemon was full.
        daemon.runtime.running = LIMIT.get();
        let job = daemon.jobs.find_mut(&nightly()).unwrap();
        job.run_started();
        job.next_fire = Some(now);
        super::fire(job, &mut daemon.runtime, &mut daemon.states, now);
        // Another run ends, which makes room.
        daemon.runtime.running = LIMIT.get() - 1;

        daemon.start_queued();

        assert_eq!(daemon.runtime.running, LIMIT.get());
        assert_eq!(
            daemon.jobs.find_mut(&nightly()).unwrap().report().condition,
            Condition::Running
        );
        assert!(daemon.jobs.find_mut(&nightly()).unwrap().queued.is_none());
    }

    #[test]
    fn a_waiting_fire_runs_as_the_fire_it_was() {
        let (_directory, paths, mut daemon) = daemon_with("daemon-queued-fire", NIGHTLY);
        let first = at("2026-09-10T03:00:00Z");
        let second = at("2026-09-11T03:00:00Z");
        daemon.runtime.running = LIMIT.get();
        // The first fire waits for room, and the second finds it waiting.
        for scheduled in [first, second] {
            let job = daemon.jobs.find_mut(&nightly()).unwrap();
            job.next_fire = Some(scheduled);
            super::fire(job, &mut daemon.runtime, &mut daemon.states, scheduled);
        }
        daemon.runtime.running = 0;

        daemon.start_queued();

        let Ok(super::Event::Finished { outcome, .. }) =
            daemon.receiver.recv_timeout(Duration::from_secs(30))
        else {
            panic!("the waiting fire did not run");
        };
        let directory = outcome.directory.unwrap();
        let log =
            std::fs::read_to_string(paths.root().join("runs").join(directory).join("run.log"))
                .unwrap();
        assert!(
            log.contains("nightly scheduled for 2026-09-10T03:00:00Z"),
            "{log}"
        );
    }

    #[test]
    fn removing_a_manifest_drops_only_its_own_state() {
        let directory = TemporaryDirectory::new("daemon-remove-state").unwrap();
        let paths = DaemonPaths::new(directory.join("root"));
        let watching = testing::watch_three(&directory, &paths);
        let limits = RunLimits {
            limit: 1,
            keep_runs: 0,
        };
        let mut daemon = Daemon::open(&paths, limits, SystemTime::now()).unwrap();

        daemon.remove(&watching.weekly);

        testing::assert_only_weekly_dropped(&daemon.states);
        testing::assert_only_weekly_dropped(&States::load(&paths.state()).unwrap());
    }
}
