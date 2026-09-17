//! The jobs the daemon fires, built from the manifests it watches.
//!
//! A manifest holds its own schedules, so the daemon reads them again on every
//! rescan. An edited manifest changes the job it describes without a command.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use loom_manifest::{JobSchedule, Manifest, load};
use loom_schedule::format_instant;

use crate::control::JobReport;
use crate::store::{JobState, Registry, States, Watched};

/// One job: one schedule of one manifest.
#[derive(Debug)]
pub(crate) struct Job {
    pub(crate) id: String,
    pub(crate) manifest: PathBuf,
    pub(crate) working_directory: PathBuf,
    /// Absent when the manifest does not load.
    pub(crate) schedule: Option<JobSchedule>,
    /// Why the job cannot run, when it cannot.
    pub(crate) error: Option<String>,
    pub(crate) next_fire: Option<SystemTime>,
    pub(crate) running: bool,
    /// A fire that waits for the running run to end.
    pub(crate) queued: bool,
    pub(crate) state: JobState,
}

impl Job {
    /// A job of one watched manifest that has not fired yet.
    fn new(
        id: String,
        watched: &Watched,
        schedule: Option<JobSchedule>,
        error: Option<String>,
        states: &States,
    ) -> Self {
        Self {
            state: states.get(&id),
            id,
            manifest: watched.path.clone(),
            working_directory: watched.working_directory.clone(),
            schedule,
            error,
            next_fire: None,
            running: false,
            queued: false,
        }
    }

    /// True when the job may fire.
    pub(crate) fn is_ready(&self) -> bool {
        self.error.is_none()
            && self.schedule.as_ref().is_some_and(JobSchedule::enabled)
            && !self.state.paused
    }

    /// The word `loom schedule list` prints for the job.
    fn condition(&self) -> &'static str {
        if self.error.is_some() {
            "broken"
        } else if self.running {
            "running"
        } else if self.state.paused {
            "paused"
        } else if !self.schedule.as_ref().is_some_and(JobSchedule::enabled) {
            "disabled"
        } else if self.next_fire.is_none() {
            "done"
        } else if self.queued {
            "queued"
        } else {
            "waiting"
        }
    }

    pub(crate) fn report(&self) -> JobReport {
        JobReport {
            id: self.id.clone(),
            manifest: self.manifest.display().to_string(),
            schedule: self
                .schedule
                .as_ref()
                .map(|schedule| schedule.schedule().to_string()),
            condition: self.condition().to_owned(),
            next_fire: self.next_fire.map(format_instant),
            last_fire: self.state.last_fire.map(format_instant),
            last_run: self.state.last_run.clone(),
            last_status: self.state.last_status,
            error: self.error.clone(),
        }
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

/// Builds the jobs of one manifest.
pub(crate) fn jobs_of(watched: &Watched, states: &States) -> Vec<Job> {
    let stem = stem(&watched.path);
    let manifest = match load(&watched.path) {
        Ok(manifest) => manifest,
        Err(error) => {
            return vec![broken(&stem, watched, states, &error.to_string())];
        }
    };
    if manifest.schedules().is_empty() {
        return vec![broken(
            &stem,
            watched,
            states,
            "manifest has no schedule section",
        )];
    }
    let sandboxed = every_task_sandboxed(&manifest);

    manifest
        .schedules()
        .iter()
        .map(|schedule| {
            let id = match schedule.name() {
                Some(name) => format!("{stem}:{name}"),
                None => stem.clone(),
            };
            let error = (!sandboxed && !schedule.allow_unsandboxed()).then(|| {
                "every task must set a sandbox, or the schedule must set allow_unsandboxed: true"
                    .to_owned()
            });
            Job::new(id, watched, Some(schedule.clone()), error, states)
        })
        .collect()
}

/// The job IDs a manifest holds, without keeping the jobs.
pub(crate) fn ids_of(watched: &Watched, states: &States) -> Vec<String> {
    ids(jobs_of(watched, states))
}

/// The IDs of `jobs`, in order.
pub(crate) fn ids(jobs: Vec<Job>) -> Vec<String> {
    jobs.into_iter().map(|job| job.id).collect()
}

/// Why a manifest cannot become a job at all: it does not load, or it names
/// no schedule. A job that loads but cannot run keeps its own error instead.
pub(crate) fn manifest_error(jobs: &[Job]) -> Option<&str> {
    jobs.iter()
        .find(|job| job.schedule.is_none())
        .and_then(|job| job.error.as_deref())
}

/// The first job ID of `ids` that another watched manifest already uses.
pub(crate) fn conflicting_id(
    registry: &Registry,
    states: &States,
    manifest: &Path,
    ids: &[String],
) -> Option<String> {
    registry
        .manifests()
        .iter()
        .filter(|watched| watched.path != manifest)
        .flat_map(|watched| ids_of(watched, states))
        .find(|id| ids.contains(id))
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
fn broken(stem: &str, watched: &Watched, states: &States, error: &str) -> Job {
    Job::new(
        stem.to_owned(),
        watched,
        None,
        Some(error.to_owned()),
        states,
    )
}
