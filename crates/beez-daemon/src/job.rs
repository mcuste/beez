//! The jobs the daemon fires, built from the manifests it watches.
//!
//! A manifest holds its own schedules, so the daemon reads them again on every
//! rescan. An edited manifest changes the job it describes without a command.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use beez_manifest::{JobSchedule, Manifest, load};
use beez_schedule::format_instant;

use crate::control::{Condition, JobId, JobReport};
use crate::message::Refusal;
use crate::paths::DaemonPaths;
use crate::run::RunOutcome;
use crate::store::{self, JobState, Registry, States, Watched};

/// One job: one schedule of one manifest.
#[derive(Debug)]
pub(crate) struct Job {
    pub(crate) id: JobId,
    pub(crate) manifest: PathBuf,
    pub(crate) working_directory: PathBuf,
    pub(crate) definition: Definition,
    pub(crate) next_fire: Option<SystemTime>,
    /// How many runs of the job are going. A job with `on_overlap: parallel`
    /// can have more than one.
    running: usize,
    /// A fire that waits for the running run to end.
    pub(crate) queued: bool,
    pub(crate) state: JobState,
}

/// What the manifest says about one job.
#[derive(Debug)]
pub(crate) enum Definition {
    /// The manifest does not load, or names no schedule. The text says why.
    Unloadable(String),
    /// The manifest holds the schedule.
    Schedule {
        schedule: JobSchedule,
        /// Why the job cannot run, when it cannot.
        refused: Option<String>,
    },
}

/// Whether planning may run a fire the daemon passed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatchUp {
    /// The daemon just started, so a passed fire may still run, late.
    Allow,
    /// A passed fire only goes in the log.
    Refuse,
}

/// What planning the next fire found.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Plan {
    /// The daemon was down over a fire, and the job runs it late.
    CatchUp(SystemTime),
    /// The daemon passed a fire and drops it.
    Missed(SystemTime),
}

impl Job {
    /// A job of one watched manifest that has not fired yet.
    fn new(id: JobId, watched: &Watched, definition: Definition, states: &States) -> Self {
        Self {
            state: states.get(&id),
            id,
            manifest: watched.path.clone(),
            working_directory: watched.working_directory.clone(),
            definition,
            next_fire: None,
            running: 0,
            queued: false,
        }
    }

    /// The schedule, when the manifest loads.
    pub(crate) fn schedule(&self) -> Option<&JobSchedule> {
        match &self.definition {
            Definition::Unloadable(_) => None,
            Definition::Schedule { schedule, .. } => Some(schedule),
        }
    }

    /// Why the job cannot run, when it cannot.
    pub(crate) fn error(&self) -> Option<&str> {
        match &self.definition {
            Definition::Unloadable(error) => Some(error),
            Definition::Schedule { refused, .. } => refused.as_deref(),
        }
    }

    /// True when the job may fire.
    pub(crate) fn is_ready(&self) -> bool {
        self.error().is_none() && self.enabled() && !self.state.paused
    }

    /// True while a run of the job is going.
    pub(crate) fn is_running(&self) -> bool {
        self.running > 0
    }

    /// Notes that one run of the job started, which takes the waiting fire.
    pub(crate) fn run_started(&mut self) {
        self.running += 1;
        self.queued = false;
    }

    /// Notes that one run of the job ended, and what it left.
    pub(crate) fn run_ended(&mut self, outcome: &RunOutcome) {
        self.running = self.running.saturating_sub(1);
        self.state.last_run.clone_from(&outcome.directory);
        self.state.last_status = outcome.result.as_ref().ok().copied();
    }

    /// Keeps what is going on from the job `old` this one replaces.
    pub(crate) fn carry_from(&mut self, old: &Self) {
        self.running = old.running;
        self.queued = old.queued;
        // A schedule that did not change keeps the fire it planned.
        if old.schedule() == self.schedule() {
            self.next_fire = old.next_fire;
        }
    }

    /// True when the manifest keeps the schedule on.
    fn enabled(&self) -> bool {
        self.schedule().is_some_and(JobSchedule::enabled)
    }

    /// What `beez schedule list` prints for the job.
    fn condition(&self) -> Condition {
        if self.error().is_some() {
            Condition::Broken
        } else if self.is_running() {
            Condition::Running
        } else if self.state.paused {
            Condition::Paused
        } else if !self.enabled() {
            Condition::Disabled
        } else if self.next_fire.is_none() {
            Condition::Done
        } else if self.queued {
            Condition::Queued
        } else {
            Condition::Waiting
        }
    }

    pub(crate) fn report(&self) -> JobReport {
        JobReport {
            id: self.id.to_string(),
            manifest: self.manifest.display().to_string(),
            schedule: self
                .schedule()
                .map(|schedule| schedule.schedule().to_string()),
            condition: self.condition(),
            next_fire: self.next_fire.map(format_instant),
            last_fire: self.state.last_fire.map(format_instant),
            last_run: self.state.last_run.clone(),
            last_status: self.state.last_status,
            error: self.error().map(str::to_owned),
        }
    }

    /// Plans the next fire.
    ///
    /// A fire the daemon passed runs late only when the schedule asks for it,
    /// and only once. Every other passed fire goes in the log and nowhere
    /// else.
    pub(crate) fn plan(&mut self, now: SystemTime, catch_up: CatchUp) -> Option<Plan> {
        self.next_fire = None;
        if !self.is_ready() {
            return None;
        }
        let schedule = self.schedule()?;
        // A one-time schedule that never fired counts every past instant as missed.
        let base = self
            .state
            .last_fire
            .unwrap_or(if schedule.schedule().is_recurring() {
                now
            } else {
                UNIX_EPOCH
            });
        let next = schedule.schedule().next_after(base)?;
        if next > now {
            self.next_fire = Some(next);
            return None;
        }
        if catch_up == CatchUp::Allow && schedule.catch_up() {
            self.next_fire = Some(now);
            return Some(Plan::CatchUp(next));
        }
        self.next_fire = schedule.schedule().next_after(now);

        Some(Plan::Missed(next))
    }
}

/// One watched manifest and what the daemon last read from it.
#[derive(Debug)]
pub(crate) struct WatchedManifest {
    pub(crate) watched: Watched,
    /// Time the file last changed, so a rescan reads it only once.
    pub(crate) modified: Option<SystemTime>,
}

/// Reads every manifest of the registry and builds its jobs.
///
/// A manifest that does not load keeps its jobs out of the set and reports the
/// reason against every job it used to hold.
pub(crate) fn build(registry: &Registry, states: &States) -> (Vec<WatchedManifest>, Vec<Job>) {
    let mut manifests = Vec::new();
    let mut jobs = Vec::new();
    for watched in registry.manifests() {
        let modified = modified_at(&watched.path);
        jobs.extend(jobs_of(watched, states));
        manifests.push(WatchedManifest {
            watched: watched.clone(),
            modified,
        });
    }

    (manifests, jobs)
}

/// Reports every job of a Beez root without a running daemon.
pub(crate) fn reports(paths: &DaemonPaths) -> io::Result<Vec<JobReport>> {
    let (registry, states) = store::load_all(paths)?;
    let (_, mut jobs) = build(&registry, &states);
    let now = SystemTime::now();
    for job in &mut jobs {
        job.plan(now, CatchUp::Refuse);
    }

    Ok(jobs.iter().map(Job::report).collect())
}

/// Builds the jobs of one manifest.
pub(crate) fn jobs_of(watched: &Watched, states: &States) -> Vec<Job> {
    let stem = stem(&watched.path);
    let manifest = match load(&watched.path) {
        Ok(manifest) => manifest,
        Err(error) => {
            return vec![unloadable(&stem, watched, states, error.to_string())];
        }
    };
    if manifest.schedules().is_empty() {
        return vec![unloadable(
            &stem,
            watched,
            states,
            "manifest has no schedule section".to_owned(),
        )];
    }
    let sandboxed = every_task_sandboxed(&manifest);

    manifest
        .schedules()
        .iter()
        .map(|schedule| {
            let id = JobId::new(&stem, schedule.name());
            let refused = (!sandboxed && !schedule.allow_unsandboxed()).then(|| {
                "every task must set a sandbox, or the schedule must set allow_unsandboxed: true"
                    .to_owned()
            });
            let definition = Definition::Schedule {
                schedule: schedule.clone(),
                refused,
            };
            Job::new(id, watched, definition, states)
        })
        .collect()
}

/// The job IDs a manifest holds, without keeping the jobs.
pub(crate) fn ids_of(watched: &Watched, states: &States) -> Vec<JobId> {
    ids(&jobs_of(watched, states))
}

/// The IDs of `jobs`, in order.
pub(crate) fn ids(jobs: &[Job]) -> Vec<JobId> {
    jobs.iter().map(|job| job.id.clone()).collect()
}

/// Checks the jobs of a manifest a command wants to watch, and names their IDs.
///
/// `others` holds the job IDs of every other watched manifest. An ID one of
/// them already uses is refused.
pub(crate) fn admit(
    jobs: &[Job],
    others: impl IntoIterator<Item = JobId>,
) -> Result<Vec<JobId>, Refusal> {
    if let Some(error) = manifest_error(jobs) {
        return Err(Refusal::Broken(error.to_owned()));
    }
    let ids = ids(jobs);
    if let Some(taken) = others.into_iter().find(|id| ids.contains(id)) {
        return Err(Refusal::Taken(taken));
    }

    Ok(ids)
}

/// Why a manifest cannot become a job at all: it does not load, or it names
/// no schedule. A job that loads but cannot run keeps its own error instead.
fn manifest_error(jobs: &[Job]) -> Option<&str> {
    jobs.iter().find_map(|job| match &job.definition {
        Definition::Unloadable(error) => Some(error.as_str()),
        Definition::Schedule { .. } => None,
    })
}

/// The name a manifest gives its jobs.
pub(crate) fn stem(manifest: &Path) -> String {
    manifest.file_stem().map_or_else(
        || "workflow".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}

/// When the file last changed, or nothing when it cannot be read.
pub(crate) fn modified_at(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn every_task_sandboxed(manifest: &Manifest) -> bool {
    manifest
        .workflow()
        .tasks()
        .iter()
        .all(|task| task.sandbox().is_some())
}

/// A job that names its manifest's problem instead of running.
fn unloadable(stem: &str, watched: &Watched, states: &States, error: String) -> Job {
    Job::new(
        JobId::new(stem, None),
        watched,
        Definition::Unloadable(error),
        states,
    )
}

/// Jobs built from manifests a test writes.
#[cfg(test)]
pub(crate) mod testing {
    use std::time::SystemTime;

    use beez_schedule::parse_instant;
    use beez_test_support::TemporaryDirectory;

    use super::{Job, jobs_of};
    use crate::store::{States, Watched};

    /// The jobs of a manifest whose `schedule` section holds `schedule`.
    pub(crate) fn jobs(schedule: &str) -> Vec<Job> {
        let directory = TemporaryDirectory::new("daemon-job").unwrap();
        let path = directory
            .write(
                "nightly.yaml",
                format!(
                    "schedule:\n{schedule}tasks:\n  - id: mark\n    sandbox: false\n    command: [\"true\"]\n"
                ),
            )
            .unwrap();
        let watched = Watched {
            path,
            working_directory: directory.path().to_path_buf(),
        };

        jobs_of(&watched, &States::default())
    }

    /// The only job of a manifest whose `schedule` section holds `schedule`.
    pub(crate) fn job(schedule: &str) -> Job {
        jobs(schedule).pop().unwrap()
    }

    pub(crate) fn at(text: &str) -> SystemTime {
        parse_instant(text).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{at, job, jobs};
    use super::{CatchUp, Plan, admit};
    use crate::control::{Condition, JobId};
    use crate::message::Refusal;
    use crate::run::RunOutcome;

    const NIGHTLY: &str = "  cron: \"0 3 * * *\"\n  allow_unsandboxed: true\n";

    fn outcome(status: i32) -> RunOutcome {
        RunOutcome {
            directory: Some("run".to_owned()),
            result: Ok(status),
            summary: "1 passed".to_owned(),
        }
    }

    #[test]
    fn plans_the_next_fire_of_a_job_that_never_fired() {
        let mut job = job(NIGHTLY);

        assert_eq!(job.plan(at("2026-09-10T12:00:00Z"), CatchUp::Allow), None);
        assert_eq!(job.next_fire, Some(at("2026-09-11T03:00:00Z")));
        assert_eq!(job.report().condition, Condition::Waiting);
    }

    #[test]
    fn plans_the_fire_after_the_last_one() {
        let mut job = job(NIGHTLY);
        job.state.last_fire = Some(at("2026-09-10T03:00:00Z"));

        assert_eq!(job.plan(at("2026-09-10T12:00:00Z"), CatchUp::Refuse), None);
        assert_eq!(job.next_fire, Some(at("2026-09-11T03:00:00Z")));
    }

    #[test]
    fn logs_a_passed_fire_without_running_it() {
        let mut job = job(NIGHTLY);
        job.state.last_fire = Some(at("2026-09-08T03:00:00Z"));

        assert_eq!(
            job.plan(at("2026-09-10T12:00:00Z"), CatchUp::Allow),
            Some(Plan::Missed(at("2026-09-09T03:00:00Z")))
        );
        assert_eq!(job.next_fire, Some(at("2026-09-11T03:00:00Z")));
    }

    #[test]
    fn runs_a_passed_fire_late_only_when_both_allow_it() {
        let schedule = "  cron: \"0 3 * * *\"\n  catch_up: true\n  allow_unsandboxed: true\n";
        let now = at("2026-09-10T12:00:00Z");
        let mut caught = job(schedule);
        caught.state.last_fire = Some(at("2026-09-08T03:00:00Z"));
        let mut refused = job(schedule);
        refused.state.last_fire = Some(at("2026-09-08T03:00:00Z"));

        assert_eq!(
            caught.plan(now, CatchUp::Allow),
            Some(Plan::CatchUp(at("2026-09-09T03:00:00Z")))
        );
        assert_eq!(caught.next_fire, Some(now));
        assert_eq!(
            refused.plan(now, CatchUp::Refuse),
            Some(Plan::Missed(at("2026-09-09T03:00:00Z")))
        );
        assert_eq!(refused.next_fire, Some(at("2026-09-11T03:00:00Z")));
    }

    #[test]
    fn plans_a_one_time_schedule_once() {
        let schedule = "  at: \"2026-09-10T03:00:00Z\"\n  allow_unsandboxed: true\n";
        let mut early = job(schedule);
        let mut late = job(schedule);
        let mut fired = job(schedule);
        fired.state.last_fire = Some(at("2026-09-10T03:00:00Z"));

        assert_eq!(early.plan(at("2026-09-09T00:00:00Z"), CatchUp::Allow), None);
        assert_eq!(early.next_fire, Some(at("2026-09-10T03:00:00Z")));
        // A one-time schedule that never fired missed its instant.
        assert_eq!(
            late.plan(at("2026-09-11T00:00:00Z"), CatchUp::Allow),
            Some(Plan::Missed(at("2026-09-10T03:00:00Z")))
        );
        assert_eq!(late.next_fire, None);
        assert_eq!(late.report().condition, Condition::Done);
        assert_eq!(fired.plan(at("2026-09-11T00:00:00Z"), CatchUp::Allow), None);
        assert_eq!(fired.report().condition, Condition::Done);
    }

    #[test]
    fn runs_a_one_time_schedule_it_missed_when_it_asks_for_it() {
        let mut job =
            job("  at: \"2026-09-10T03:00:00Z\"\n  catch_up: true\n  allow_unsandboxed: true\n");
        let now = at("2026-09-11T00:00:00Z");

        assert_eq!(
            job.plan(now, CatchUp::Allow),
            Some(Plan::CatchUp(at("2026-09-10T03:00:00Z")))
        );
        assert_eq!(job.next_fire, Some(now));
    }

    #[test]
    fn plans_no_fire_for_a_job_that_cannot_fire() {
        let mut paused = job(NIGHTLY);
        paused.state.paused = true;
        let mut disabled =
            job("  cron: \"0 3 * * *\"\n  enabled: false\n  allow_unsandboxed: true\n");
        let mut refused = job("  cron: \"0 3 * * *\"\n");
        let now = at("2026-09-10T12:00:00Z");

        for (job, condition) in [
            (&mut paused, Condition::Paused),
            (&mut disabled, Condition::Disabled),
            (&mut refused, Condition::Broken),
        ] {
            assert_eq!(job.plan(now, CatchUp::Allow), None);
            assert_eq!(job.next_fire, None);
            assert_eq!(job.report().condition, condition);
        }
    }

    #[test]
    fn reports_a_queued_and_a_running_job() {
        let mut job = job(NIGHTLY);
        job.plan(at("2026-09-10T12:00:00Z"), CatchUp::Refuse);
        job.queued = true;

        assert_eq!(job.report().condition, Condition::Queued);
        job.run_started();
        assert!(!job.queued);
        assert_eq!(job.report().condition, Condition::Running);
        job.run_ended(&outcome(3));
        assert_eq!(job.report().condition, Condition::Waiting);
        assert_eq!(job.state.last_status, Some(3));
    }

    #[test]
    fn keeps_running_while_one_of_two_parallel_runs_goes_on() {
        let mut job =
            job("  cron: \"0 3 * * *\"\n  on_overlap: parallel\n  allow_unsandboxed: true\n");
        job.plan(at("2026-09-10T12:00:00Z"), CatchUp::Refuse);
        job.run_started();
        job.run_started();

        job.run_ended(&outcome(0));

        assert!(job.is_running());
        assert_eq!(job.report().condition, Condition::Running);
        job.run_ended(&outcome(0));
        assert!(!job.is_running());
    }

    #[test]
    fn names_each_schedule_of_a_manifest() {
        let jobs = jobs(
            "  - name: docs\n    cron: \"0 3 * * *\"\n    allow_unsandboxed: true\n  - name: tests\n    cron: \"0 4 * * *\"\n    allow_unsandboxed: true\n",
        );
        let ids: Vec<String> = jobs.iter().map(|job| job.id.to_string()).collect();

        assert_eq!(ids, ["nightly:docs", "nightly:tests"]);
    }

    #[test]
    fn refuses_a_manifest_that_does_not_load_or_an_id_another_uses() {
        let broken = jobs("  cron: \"not a cron\"\n");
        let nightly = jobs(NIGHTLY);

        assert!(matches!(admit(&broken, []), Err(Refusal::Broken(_))));
        assert_eq!(
            admit(&nightly, [JobId::new("nightly", None)]),
            Err(Refusal::Taken(JobId::new("nightly", None)))
        );
        assert_eq!(
            admit(&nightly, [JobId::new("weekly", None)]),
            Ok(vec![JobId::new("nightly", None)])
        );
    }
}
