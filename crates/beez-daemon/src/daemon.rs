//! The daemon: a timer, a set of jobs, and a socket that carries commands.
//!
//! One thread holds every decision. The socket listener and the signal
//! handler only send events, and each run reports its result the same way, so
//! the job set needs no lock.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime};

use beez_manifest::{JobSchedule, Overlap};
use beez_schedule::format_instant;
use serde::Serialize;

use crate::control::{self, DaemonStatus, JobId, Request, Response};
use crate::job::{self, CatchUp, Job, Plan, WatchedManifest};
use crate::message::{self, Hold, Refusal};
use crate::paths::DaemonPaths;
use crate::run::{self, JobRun, RunOutcome};
use crate::store::{self, Registry, States, Watched};
use crate::{apply, report};

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
    /// Waits for a running run to end. The reason goes in the log.
    Queue(&'static str),
    Skip,
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
pub fn run(paths: &DaemonPaths, limits: RunLimits) -> io::Result<()> {
    // A service manager gives the daemon a working directory of its own, so
    // the root it resolved from it goes in the message.
    paths.create().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot create {}: {error}", paths.directory().display()),
        )
    })?;
    let listener = bind(paths)?;
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

    let _ = std::fs::remove_file(paths.socket());
    let _ = std::fs::remove_file(paths.record());
    Ok(())
}

struct Daemon {
    paths: DaemonPaths,
    registry: Registry,
    states: States,
    manifests: Vec<WatchedManifest>,
    jobs: Vec<Job>,
    runtime: Runtime,
    /// How many runs stay in the Beez root.
    keep: usize,
    started: SystemTime,
    receiver: Receiver<Event>,
    phase: Phase,
}

/// What starting a run needs, apart from the job.
struct Runtime {
    /// How many runs are going.
    running: usize,
    limit: usize,
    /// Where a run reports that it ended.
    sender: Sender<Event>,
    /// Beez root that holds the artifacts of every run the daemon starts.
    root: PathBuf,
}

impl Runtime {
    /// True while the daemon may start one more run.
    fn has_room(&self) -> bool {
        self.running < self.limit
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
        report::line(
            "Firing",
            &format!("{} scheduled for {}", run.id, format_instant(fire)),
        );

        let sender = self.sender.clone();
        let id = run.id.clone();
        thread::spawn(move || {
            let outcome = run::execute(&run);
            let _ = sender.send(Event::Finished {
                job: id,
                outcome: Box::new(outcome),
            });
        });
    }
}

impl Daemon {
    fn open(paths: &DaemonPaths, limits: RunLimits, started: SystemTime) -> io::Result<Self> {
        let (registry, states) = store::load_all(paths)?;
        let (manifests, jobs) = job::build(&registry, &states);
        let (sender, receiver) = mpsc::channel();
        let mut daemon = Self {
            paths: paths.clone(),
            registry,
            states,
            manifests,
            jobs,
            runtime: Runtime {
                running: 0,
                limit: limits.limit.max(1),
                sender,
                root: paths.root().to_path_buf(),
            },
            keep: limits.keep_runs,
            started,
            receiver,
            phase: Phase::Serving,
        };
        // A daemon that was down may have passed a fire, so this is the one
        // place a job may still run it.
        let now = SystemTime::now();
        for job in &mut daemon.jobs {
            plan(job, now, CatchUp::Allow);
        }

        Ok(daemon)
    }

    /// Takes commands off the socket on their own thread.
    fn listen(&self, listener: UnixListener) {
        let sender = self.runtime.sender.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                match control::read_request(&stream) {
                    Ok(request) => {
                        if sender.send(Event::Command(request, stream)).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let response = Response::error(error.to_string());
                        let _ = control::write_line(&stream, &response);
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

            match self.receiver.recv_timeout(self.sleep(now)) {
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
                let _ = control::write_line(&stream, &response);
            }
            Event::Finished { job, outcome } => self.finished(&job, &outcome),
            Event::Signal => self.signal(),
        }
    }

    fn signal(&mut self) {
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
                started: format_instant(self.started),
                running: self.runtime.running,
                jobs: self.jobs.iter().map(Job::report).collect(),
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
        let jobs = job::jobs_of(&watched, &self.states);
        let others = self
            .jobs
            .iter()
            .filter(|other| other.manifest != watched.path)
            .map(|other| other.id.clone());
        let names = match job::admit(&jobs, others) {
            Ok(ids) => ids
                .iter()
                .map(JobId::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            Err(refusal) => return refusal.into(),
        };
        let watching = match apply::watch(&mut self.registry, &self.paths, watched.clone()) {
            Ok(watching) => watching,
            Err(error) => return Response::error(message::unwritable_registry(&error)),
        };
        self.install(&watched, jobs, now);
        report::line(
            "Watching",
            &format!("{} as {names}", watched.path.display()),
        );

        Response::done(format!("{watching} as {names}"))
    }

    fn remove(&mut self, manifest: &Path) -> Response {
        let stopped = match apply::unwatch(&mut self.registry, &self.paths, manifest) {
            Ok(Ok(stopped)) => stopped,
            Ok(Err(refusal)) => return refusal.into(),
            Err(error) => return Response::error(message::unwritable_registry(&error)),
        };
        // A run of this manifest keeps going, and reports itself as it ends.
        self.retain_manifests(|path| path != manifest);
        self.prune_states();
        report::line("Watching", &format!("dropped {}", manifest.display()));

        Response::done(stopped)
    }

    fn hold(&mut self, id: &JobId, hold: Hold, now: SystemTime) -> Response {
        let Some(job) = find(&mut self.jobs, id) else {
            return Refusal::UnknownJob(id.clone()).into();
        };
        job.state.paused = hold.paused();
        save_state(&mut self.states, &self.paths, job);
        plan(job, now, CatchUp::Refuse);
        let answer = message::held(id, hold);
        report::line("Job", &answer);

        Response::done(answer)
    }

    fn trigger(&mut self, id: &JobId, now: SystemTime) -> Response {
        let Some(job) = find(&mut self.jobs, id) else {
            return Refusal::UnknownJob(id.clone()).into();
        };
        if let Some(error) = job.error() {
            return Response::error(format!("{id} cannot run: {error}"));
        }
        if job.is_running() {
            return Response::error(format!("{id} is already running"));
        }
        if !self.runtime.has_room() {
            return Response::error(format!(
                "the daemon already runs {} workflows",
                self.runtime.limit
            ));
        }
        // A triggered run stands beside the schedule, so it moves no fire.
        self.runtime.start(job, now);

        Response::done(format!("running {id} now"))
    }

    /// Reads a manifest again when the file changed.
    fn rescan(&mut self, now: SystemTime) {
        let changed: Vec<Watched> = self
            .manifests
            .iter()
            .filter(|entry| job::modified_at(&entry.watched.path) != entry.modified)
            .map(|entry| entry.watched.clone())
            .collect();
        for watched in &changed {
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
        self.retain_manifests(|path| watched.iter().any(|entry| entry.path == path));
        for entry in &watched {
            self.reload(entry, now);
        }

        watched.len()
    }

    /// Reads one manifest again and replaces its jobs.
    fn reload(&mut self, watched: &Watched, now: SystemTime) {
        let fresh = job::jobs_of(watched, &self.states);
        self.install(watched, fresh, now);
    }

    /// Replaces the jobs of one manifest with `fresh`, keeping what is going on.
    fn install(&mut self, watched: &Watched, mut fresh: Vec<Job>, now: SystemTime) {
        for job in &mut fresh {
            if let Some(old) = self.jobs.iter().find(|old| old.id == job.id) {
                job.carry_from(old);
            }
            if job.next_fire.is_none() {
                plan(job, now, CatchUp::Refuse);
            }
        }
        self.jobs.retain(|job| job.manifest != watched.path);
        self.jobs.extend(fresh);
        self.remember_modified(watched);
        // The state stays, because a manifest that stopped loading holds no
        // jobs for a moment, and its history must survive the mistake.
        for job in self.jobs.iter().filter(|job| job.manifest == watched.path) {
            if let Some(error) = job.error() {
                report::line("Broken", &format!("{}: {error}", job.id));
            }
        }
    }

    /// Drops the jobs and the watch of every manifest `keep` refuses.
    fn retain_manifests(&mut self, keep: impl Fn(&Path) -> bool) {
        self.jobs.retain(|job| keep(&job.manifest));
        self.manifests.retain(|entry| keep(&entry.watched.path));
    }

    fn remember_modified(&mut self, watched: &Watched) {
        let modified = job::modified_at(&watched.path);
        match self
            .manifests
            .iter_mut()
            .find(|entry| entry.watched.path == watched.path)
        {
            Some(entry) => {
                entry.watched = watched.clone();
                entry.modified = modified;
            }
            None => self.manifests.push(WatchedManifest {
                watched: watched.clone(),
                modified,
            }),
        }
    }

    fn fire_due(&mut self, now: SystemTime) {
        if self.phase != Phase::Serving {
            return;
        }
        for job in &mut self.jobs {
            if job.is_ready() && job.next_fire.is_some_and(|fire| fire <= now) {
                fire(job, &mut self.runtime, &mut self.states, &self.paths, now);
            }
        }
    }

    fn finished(&mut self, id: &JobId, outcome: &RunOutcome) {
        self.runtime.running = self.runtime.running.saturating_sub(1);
        match &outcome.result {
            Err(error) => report::line("Failed", &format!("{id}: {error}")),
            Ok(0) => report::line("Finished", &format!("{id}: {}", outcome.summary)),
            Ok(status) => report::line(
                "Failed",
                &format!(
                    "{id}: {} ({})",
                    outcome.summary,
                    beez_record::status_text(Some(*status))
                ),
            ),
        }
        // A manifest that stopped being watched while it ran has no job left.
        if let Some(job) = find(&mut self.jobs, id) {
            job.run_ended(outcome);
            save_state(&mut self.states, &self.paths, job);
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
            let Some(job) = self
                .jobs
                .iter_mut()
                .find(|job| job.queued && job.is_ready() && !job.is_running())
            else {
                return;
            };
            let fire = job.state.last_fire.unwrap_or_else(SystemTime::now);
            self.runtime.start(job, fire);
        }
    }

    fn prune_states(&mut self) {
        self.states.keep_watched(&self.registry);
        write_states(&self.states, &self.paths);
    }
}

/// The job with `id`, when the daemon holds it.
fn find<'jobs>(jobs: &'jobs mut [Job], id: &JobId) -> Option<&'jobs mut Job> {
    jobs.iter_mut().find(|job| job.id == *id)
}

/// Starts, queues or drops one due fire, then plans the fire after it.
fn fire(
    job: &mut Job,
    runtime: &mut Runtime,
    states: &mut States,
    paths: &DaemonPaths,
    now: SystemTime,
) {
    let fire = job.next_fire.unwrap_or(now);
    let action = action(job, runtime.running, runtime.limit);
    // The fire happened, whatever comes of it, so the schedule moves on.
    job.state.last_fire = Some(fire);
    match action {
        Action::Start => runtime.start(job, fire),
        Action::Queue(reason) => queue(job, reason),
        Action::Skip => report::line(
            "Skipped",
            &format!(
                "{} scheduled for {}, its last run has not ended",
                job.id,
                format_instant(fire)
            ),
        ),
    }
    save_state(states, paths, job);
    plan(job, now, CatchUp::Refuse);
}

/// What a fire of `job` leads to while `running` runs of at most `limit` go.
fn action(job: &Job, running: usize, limit: usize) -> Action {
    if running >= limit {
        // The limit never drops work, so a fire waits instead.
        return Action::Queue("the daemon is at its run limit");
    }
    if !job.is_running() {
        return Action::Start;
    }
    match job
        .schedule()
        .map_or(Overlap::Skip, JobSchedule::on_overlap)
    {
        Overlap::Skip => Action::Skip,
        Overlap::Queue => Action::Queue("its last run has not ended"),
        Overlap::Parallel => Action::Start,
    }
}

fn queue(job: &mut Job, reason: &str) {
    // One waiting fire per job, so a stuck job cannot pile them up.
    if job.queued {
        report::line("Dropped", &format!("{}, a fire is already waiting", job.id));
        return;
    }
    job.queued = true;
    report::line("Queued", &format!("{}, {reason}", job.id));
}

/// Plans the next fire of one job, and logs a fire it passed.
fn plan(job: &mut Job, now: SystemTime, catch_up: CatchUp) {
    match job.plan(now, catch_up) {
        Some(Plan::CatchUp(missed)) => report::line(
            "Catch-up",
            &format!(
                "{} missed {}, running it now",
                job.id,
                format_instant(missed)
            ),
        ),
        Some(Plan::Missed(missed)) => report::line(
            "Missed",
            &format!("{} missed {}", job.id, format_instant(missed)),
        ),
        None => {}
    }
}

fn save_state(states: &mut States, paths: &DaemonPaths, job: &Job) {
    states.set(&job.id, job.state.clone());
    write_states(states, paths);
}

fn write_states(states: &States, paths: &DaemonPaths) {
    if let Err(error) = states.save(&paths.state()) {
        report::line("Warning", &format!("cannot write the state: {error}"));
    }
}

/// Binds the socket, which also keeps a second daemon out.
fn bind(paths: &DaemonPaths) -> io::Result<UnixListener> {
    match UnixListener::bind(paths.socket()) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            if control::is_running(paths.socket()) {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    message::already_running(paths.root()),
                ));
            }
            // Nothing answers, so the socket belongs to a daemon that died.
            std::fs::remove_file(paths.socket())?;
            UnixListener::bind(paths.socket())
        }
        Err(error) => Err(error),
    }
}

fn write_record(paths: &DaemonPaths, started: SystemTime) -> io::Result<()> {
    let record = DaemonRecord {
        schema: SCHEMA,
        pid: std::process::id(),
        socket: paths.socket().to_path_buf(),
        started: format_instant(started),
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
    use std::time::SystemTime;

    use beez_test_support::TemporaryDirectory;

    use super::{Action, Daemon, RunLimits, action};
    use crate::job::testing::job;
    use crate::paths::DaemonPaths;
    use crate::store::{States, testing};

    const LIMIT: usize = 2;

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
            assert_eq!(act(overlap, false, LIMIT - 1), Action::Start, "{overlap}");
        }
    }

    #[test]
    fn follows_the_overlap_policy_of_a_job_that_runs() {
        assert_eq!(act("skip", true, 1), Action::Skip);
        assert_eq!(
            act("queue", true, 1),
            Action::Queue("its last run has not ended")
        );
        assert_eq!(act("parallel", true, 1), Action::Start);
    }

    #[test]
    fn queues_every_fire_at_the_run_limit() {
        for overlap in ["skip", "queue", "parallel"] {
            for job_running in [false, true] {
                assert_eq!(
                    act(overlap, job_running, LIMIT),
                    Action::Queue("the daemon is at its run limit"),
                    "{overlap} {job_running}"
                );
            }
        }
    }

    #[test]
    fn skips_a_fire_of_a_running_job_that_names_no_policy() {
        let mut job = job("  cron: \"0 3 * * *\"\n  allow_unsandboxed: true\n");
        job.run_started();

        assert_eq!(action(&job, 1, LIMIT), Action::Skip);
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
