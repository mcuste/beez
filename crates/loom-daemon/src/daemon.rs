//! The daemon: a timer, a set of jobs, and a socket that carries commands.
//!
//! One thread holds every decision. The socket listener and the signal
//! handler only send events, and each run reports its result the same way, so
//! the job set needs no lock.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use loom_manifest::{JobSchedule, Overlap};
use loom_schedule::format_instant;
use serde::Serialize;

use crate::control::{self, JobReport, Request, Response};
use crate::job::{self, Job, WatchedManifest};
use crate::paths::DaemonPaths;
use crate::run::{self, JobRun, RunOutcome};
use crate::store::{self, Registry, States, Watched};
use crate::{apply, message, report};

/// How many runs the daemon starts at the same time.
///
/// A harness run costs tokens and processor time, so the daemon holds the
/// number down and lets the rest wait.
pub const DEFAULT_RUN_LIMIT: usize = 2;

/// How many runs the daemon keeps in the Loom root.
///
/// A job that fires every hour writes a run every hour, so the daemon sweeps
/// the oldest away. Zero keeps every run.
pub const DEFAULT_KEPT_RUNS: usize = 200;

/// Layout version of `daemon.json`.
const SCHEMA: u32 = 1;

/// Longest the timer sleeps. A clock change or a machine that slept cannot
/// hide a fire for longer than this.
const MAX_SLEEP: Duration = Duration::from_secs(60);

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
        job: String,
        outcome: Box<RunOutcome>,
    },
    /// The daemon was asked to stop.
    Signal,
}

/// What a fire leads to.
enum Action {
    Start,
    /// Waits for a running run to end. The reason goes in the log.
    Queue(&'static str),
    Skip,
}

/// What planning the next fire found.
enum Plan {
    /// The daemon was down over a fire, and the job runs it late.
    CatchUp(SystemTime),
    /// The daemon passed a fire and drops it.
    Missed(SystemTime),
}

/// Runs the daemon until it is asked to stop.
///
/// One Loom root holds one daemon, and the socket itself keeps a second one
/// out.
pub fn run(paths: &DaemonPaths, limit: usize, keep: usize) -> io::Result<()> {
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
    let mut daemon = Daemon::open(paths, limit.max(1), keep, started)?;
    write_record(paths, started)?;
    daemon.listen(listener);
    daemon.watch_signals()?;
    loom_sandbox::set_diagnostic_sink(Box::new(|message| report::line("Sandbox", message)));

    report::line(
        "Daemon",
        &format!(
            "started with {} jobs, {} at a time, on {}",
            daemon.jobs.len(),
            daemon.limit,
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

/// Reports every job of a Loom root without a running daemon.
pub(crate) fn reports(paths: &DaemonPaths) -> io::Result<Vec<JobReport>> {
    let (registry, states) = store::load_all(paths)?;
    let (_, mut jobs) = job::build(&registry, &states);
    let now = SystemTime::now();
    for job in &mut jobs {
        plan(job, now, false);
    }

    Ok(jobs.iter().map(Job::report).collect())
}

struct Daemon {
    paths: DaemonPaths,
    registry: Registry,
    states: States,
    manifests: Vec<WatchedManifest>,
    jobs: Vec<Job>,
    running: usize,
    limit: usize,
    /// How many runs stay in the Loom root.
    keep: usize,
    started: SystemTime,
    sender: Sender<Event>,
    receiver: Receiver<Event>,
    /// True once the daemon waits for its running runs to end.
    stopping: bool,
    /// True once the daemon gives up waiting.
    exiting: bool,
}

impl Daemon {
    fn open(
        paths: &DaemonPaths,
        limit: usize,
        keep: usize,
        started: SystemTime,
    ) -> io::Result<Self> {
        let (registry, states) = store::load_all(paths)?;
        let (manifests, jobs) = job::build(&registry, &states);
        let (sender, receiver) = mpsc::channel();
        let mut daemon = Self {
            paths: paths.clone(),
            registry,
            states,
            manifests,
            jobs,
            running: 0,
            limit,
            keep,
            started,
            sender,
            receiver,
            stopping: false,
            exiting: false,
        };
        // A daemon that was down may have passed a fire, so this is the one
        // place a job may still run it.
        daemon.plan_all(SystemTime::now(), true);

        Ok(daemon)
    }

    /// Takes commands off the socket on their own thread.
    fn listen(&self, listener: UnixListener) {
        let sender = self.sender.clone();
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
        let sender = self.sender.clone();
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
        while !(self.exiting || self.stopping && self.running == 0) {
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
        if self.stopping {
            self.exiting = true;
            report::line("Daemon", "exiting now, leaving its runs behind");
            return;
        }
        self.begin_stopping();
    }

    /// Stops firing, and says how many runs the daemon waits for.
    fn begin_stopping(&mut self) -> String {
        self.stopping = true;
        let waiting = waiting_message(self.running);
        report::line("Daemon", &waiting);
        waiting
    }

    fn command(&mut self, request: Request) -> Response {
        let now = SystemTime::now();
        match request {
            Request::Status => Response::Status {
                pid: std::process::id(),
                started: format_instant(self.started),
                running: self.running,
                jobs: self.jobs.iter().map(Job::report).collect(),
            },
            Request::Add {
                manifest,
                working_directory,
            } => self.add(manifest, working_directory, now),
            Request::Remove { manifest } => self.remove(&manifest),
            Request::Pause { job } => self.hold(&job, true, now),
            Request::Resume { job } => self.hold(&job, false, now),
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
        let names = match job::admit(&jobs, |ids| self.taken_id(ids, &watched.path)) {
            Ok(ids) => ids.join(", "),
            Err(error) => return Response::error(error),
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

    fn remove(&mut self, manifest: &PathBuf) -> Response {
        let response = match apply::unwatch(&mut self.registry, &self.paths, manifest) {
            Ok(response @ Response::Done { .. }) => response,
            Ok(response) => return response,
            Err(error) => return Response::error(message::unwritable_registry(&error)),
        };
        // A run of this manifest keeps going, and reports itself as it ends.
        self.jobs.retain(|job| &job.manifest != manifest);
        self.manifests
            .retain(|entry| &entry.watched.path != manifest);
        self.prune_states();
        report::line("Watching", &format!("dropped {}", manifest.display()));

        response
    }

    fn hold(&mut self, id: &str, paused: bool, now: SystemTime) -> Response {
        let Some((index, job)) = self.job_mut(id) else {
            return unknown_job(id);
        };
        job.state.paused = paused;
        self.save_state(index);
        self.plan_job(index, now, false);
        let held = message::held(id, paused);
        report::line("Job", &held);

        Response::done(held)
    }

    fn trigger(&mut self, id: &str, now: SystemTime) -> Response {
        let Some((index, job)) = self.job(id) else {
            return unknown_job(id);
        };
        if let Some(error) = &job.error {
            return Response::error(format!("{id} cannot run: {error}"));
        }
        if job.running {
            return Response::error(format!("{id} is already running"));
        }
        if self.running >= self.limit {
            return Response::error(format!("the daemon already runs {} workflows", self.limit));
        }
        // A triggered run stands beside the schedule, so it moves no fire.
        self.start(index, now);

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
        let holds = |path: &PathBuf| watched.iter().any(|entry| &entry.path == path);
        // A manifest that left the list keeps its running run, and nothing else.
        self.jobs.retain(|job| holds(&job.manifest));
        self.manifests.retain(|entry| holds(&entry.watched.path));
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
    fn install(&mut self, watched: &Watched, fresh: Vec<Job>, now: SystemTime) {
        let mut carried = Vec::with_capacity(fresh.len());
        for mut job in fresh {
            if let Some(old) = self.jobs.iter().find(|old| old.id == job.id) {
                job.running = old.running;
                job.queued = old.queued;
                // A schedule that did not change keeps the fire it planned.
                if old.schedule == job.schedule {
                    job.next_fire = old.next_fire;
                }
            }
            carried.push(job);
        }
        self.jobs.retain(|job| job.manifest != watched.path);
        let first = self.jobs.len();
        self.jobs.extend(carried);
        for index in first..self.jobs.len() {
            if self
                .jobs
                .get(index)
                .is_some_and(|job| job.next_fire.is_none())
            {
                self.plan_job(index, now, false);
            }
        }
        self.remember_modified(watched);
        // The state stays, because a manifest that stopped loading holds no
        // jobs for a moment, and its history must survive the mistake.
        for job in self.jobs.iter().filter(|job| job.manifest == watched.path) {
            if let Some(error) = &job.error {
                report::line("Broken", &format!("{}: {error}", job.id));
            }
        }
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
        if self.stopping {
            return;
        }
        let due: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, job)| job.is_ready())
            .filter(|(_, job)| job.next_fire.is_some_and(|fire| fire <= now))
            .map(|(index, _)| index)
            .collect();
        for index in due {
            self.fire(index, now);
        }
    }

    /// Starts, queues or drops one due fire, then plans the fire after it.
    fn fire(&mut self, index: usize, now: SystemTime) {
        let Some(job) = self.jobs.get(index) else {
            return;
        };
        let fire = job.next_fire.unwrap_or(now);
        let id = job.id.clone();
        let action = self.action_for(job);
        // The fire happened, whatever comes of it, so the schedule moves on.
        if let Some(job) = self.jobs.get_mut(index) {
            job.state.last_fire = Some(fire);
        }
        match action {
            Action::Start => self.start(index, fire),
            Action::Queue(reason) => self.queue(index, reason),
            Action::Skip => report::line(
                "Skipped",
                &format!(
                    "{id} scheduled for {}, its last run has not ended",
                    format_instant(fire)
                ),
            ),
        }
        self.save_state(index);
        self.plan_job(index, now, false);
    }

    fn action_for(&self, job: &Job) -> Action {
        if self.running >= self.limit {
            // The limit never drops work, so a fire waits instead.
            return Action::Queue("the daemon is at its run limit");
        }
        if !job.running {
            return Action::Start;
        }
        match job
            .schedule
            .as_ref()
            .map_or(Overlap::Skip, JobSchedule::on_overlap)
        {
            Overlap::Skip => Action::Skip,
            Overlap::Queue => Action::Queue("its last run has not ended"),
            Overlap::Parallel => Action::Start,
        }
    }

    fn queue(&mut self, index: usize, reason: &str) {
        let Some(job) = self.jobs.get_mut(index) else {
            return;
        };
        // One waiting fire per job, so a stuck job cannot pile them up.
        if job.queued {
            report::line("Dropped", &format!("{}, a fire is already waiting", job.id));
            return;
        }
        job.queued = true;
        report::line("Queued", &format!("{}, {reason}", job.id));
    }

    fn start(&mut self, index: usize, fire: SystemTime) {
        let Some(job) = self.jobs.get_mut(index) else {
            return;
        };
        job.running = true;
        job.queued = false;
        let run = JobRun {
            id: job.id.clone(),
            manifest: job.manifest.clone(),
            working_directory: job.working_directory.clone(),
            log_directory: self.paths.root().to_path_buf(),
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

    fn finished(&mut self, id: &str, outcome: &RunOutcome) {
        self.running = self.running.saturating_sub(1);
        match &outcome.error {
            Some(error) => report::line("Failed", &format!("{id}: {error}")),
            None if outcome.status == Some(0) => {
                report::line("Finished", &format!("{id}: {}", outcome.summary));
            }
            None => report::line(
                "Failed",
                &format!(
                    "{id}: {} ({})",
                    outcome.summary,
                    loom_record::status_text(outcome.status)
                ),
            ),
        }
        // A manifest that stopped being watched while it ran has no job left.
        if let Some((index, job)) = self.job_mut(id) {
            job.running = false;
            job.state.last_run.clone_from(&outcome.directory);
            job.state.last_status = outcome.status;
            self.save_state(index);
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
        if self.stopping {
            return;
        }
        while self.running < self.limit {
            let Some(index) = self
                .jobs
                .iter()
                .position(|job| job.queued && job.is_ready() && !job.running)
            else {
                return;
            };
            let fire = self
                .jobs
                .get(index)
                .and_then(|job| job.state.last_fire)
                .unwrap_or_else(SystemTime::now);
            self.start(index, fire);
        }
    }

    fn plan_all(&mut self, now: SystemTime, allow_catch_up: bool) {
        for index in 0..self.jobs.len() {
            self.plan_job(index, now, allow_catch_up);
        }
    }

    fn plan_job(&mut self, index: usize, now: SystemTime, allow_catch_up: bool) {
        let Some(job) = self.jobs.get_mut(index) else {
            return;
        };
        let planned = plan(job, now, allow_catch_up);
        let id = job.id.clone();
        match planned {
            Some(Plan::CatchUp(missed)) => report::line(
                "Catch-up",
                &format!("{id} missed {}, running it now", format_instant(missed)),
            ),
            Some(Plan::Missed(missed)) => {
                report::line("Missed", &format!("{id} missed {}", format_instant(missed)));
            }
            None => {}
        }
    }

    /// The job with `id` and its position, when the daemon holds it.
    fn job(&self, id: &str) -> Option<(usize, &Job)> {
        self.jobs.iter().enumerate().find(|(_, job)| job.id == id)
    }

    fn job_mut(&mut self, id: &str) -> Option<(usize, &mut Job)> {
        self.jobs
            .iter_mut()
            .enumerate()
            .find(|(_, job)| job.id == id)
    }

    /// The first ID that another manifest already uses.
    fn taken_id(&self, ids: &[String], manifest: &PathBuf) -> Option<String> {
        self.jobs
            .iter()
            .find(|job| &job.manifest != manifest && ids.contains(&job.id))
            .map(|job| job.id.clone())
    }

    fn save_state(&mut self, index: usize) {
        let Some(job) = self.jobs.get(index) else {
            return;
        };
        self.states.set(&job.id, job.state.clone());
        self.write_states();
    }

    fn prune_states(&mut self) {
        let ids: Vec<String> = self.jobs.iter().map(|job| job.id.clone()).collect();
        self.states.keep_only(&ids);
        self.write_states();
    }

    fn write_states(&self) {
        if let Err(error) = self.states.save(&self.paths.state()) {
            report::line("Warning", &format!("cannot write the state: {error}"));
        }
    }
}

/// Plans the next fire of one job.
///
/// A fire the daemon passed runs late only when the schedule asks for it, and
/// only once. Every other passed fire goes in the log and nowhere else.
fn plan(job: &mut Job, now: SystemTime, allow_catch_up: bool) -> Option<Plan> {
    job.next_fire = None;
    if !job.is_ready() {
        return None;
    }
    let schedule = job.schedule.as_ref()?;
    // A one-time schedule that never fired counts every past instant as missed.
    let base = job
        .state
        .last_fire
        .unwrap_or(if schedule.schedule().is_recurring() {
            now
        } else {
            UNIX_EPOCH
        });
    let next = schedule.schedule().next_after(base)?;
    if next > now {
        job.next_fire = Some(next);
        return None;
    }
    if allow_catch_up && schedule.catch_up() {
        job.next_fire = Some(now);
        return Some(Plan::CatchUp(next));
    }
    job.next_fire = schedule.schedule().next_after(now);

    Some(Plan::Missed(next))
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

fn unknown_job(id: &str) -> Response {
    Response::error(message::unknown_job(id))
}
