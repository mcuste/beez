//! Every job the daemon holds, and the manifests they come from.
//!
//! The jobs keep the order the manifests were read in, and a manifest read
//! again moves its jobs to the end, so `beez schedule list` prints its rows
//! in that order.

use std::path::Path;
use std::time::SystemTime;

use beez_schedule::Utc;

use crate::control::{JobId, JobReport};
use crate::job::{self, CatchUp, Definition, Job, Plan, WatchedManifest};
use crate::message::Refusal;
use crate::report;
use crate::store::{Registry, States, Watched};

/// The watched manifests and the jobs built from them.
#[derive(Debug)]
pub(crate) struct JobSet {
    manifests: Vec<WatchedManifest>,
    jobs: Vec<Job>,
}

impl JobSet {
    /// Reads every manifest of the registry and builds its jobs.
    ///
    /// A manifest that does not load still holds one job, which names the
    /// reason instead of running.
    pub(crate) fn build(registry: &Registry, states: &States) -> Self {
        let mut manifests = Vec::new();
        let mut jobs = Vec::new();
        for watched in registry.manifests() {
            let modified = job::modified_at(&watched.path);
            jobs.extend(job::jobs_of(watched, states));
            manifests.push(WatchedManifest {
                watched: watched.clone(),
                modified,
            });
        }

        Self { manifests, jobs }
    }

    /// How many jobs the set holds.
    pub(crate) fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Every job, in order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter()
    }

    /// Every job, in order, to change.
    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Job> {
        self.jobs.iter_mut()
    }

    /// True when the set holds a job with `id`.
    pub(crate) fn contains(&self, id: &JobId) -> bool {
        self.jobs.iter().any(|job| job.id == *id)
    }

    /// The job with `id`, when the set holds it.
    pub(crate) fn find_mut(&mut self, id: &JobId) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|job| job.id == *id)
    }

    /// Builds the jobs of a manifest a command wants to watch, and checks them.
    ///
    /// A manifest that does not load, or names no schedule, is refused, and
    /// so is a job ID a job of another watched manifest already uses.
    pub(crate) fn admit(&self, watched: &Watched, states: &States) -> Result<Vec<Job>, Refusal> {
        let fresh = job::jobs_of(watched, states);
        if let Some(error) = manifest_error(&fresh) {
            return Err(Refusal::Broken(error.to_owned()));
        }
        if let Some(taken) = self
            .jobs
            .iter()
            .filter(|other| other.manifest != watched.path)
            .find(|other| fresh.iter().any(|job| job.id == other.id))
        {
            return Err(Refusal::Taken(taken.id.clone()));
        }

        Ok(fresh)
    }

    /// Replaces the jobs of one manifest with `fresh`, keeping what is going on.
    pub(crate) fn install(&mut self, watched: &Watched, mut fresh: Vec<Job>, now: SystemTime) {
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
    pub(crate) fn retain(&mut self, keep: impl Fn(&Path) -> bool) {
        self.jobs.retain(|job| keep(&job.manifest));
        self.manifests.retain(|entry| keep(&entry.watched.path));
    }

    /// The manifests whose file changed since the set last read them.
    pub(crate) fn changed(&self) -> Vec<Watched> {
        self.manifests
            .iter()
            .filter(|entry| job::modified_at(&entry.watched.path) != entry.modified)
            .map(|entry| entry.watched.clone())
            .collect()
    }

    /// What a command reports for every job, in order.
    pub(crate) fn reports(&self) -> Vec<JobReport> {
        self.jobs.iter().map(Job::report).collect()
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
}

/// Plans the next fire of one job, and logs a fire it passed.
pub(crate) fn plan(job: &mut Job, now: SystemTime, catch_up: CatchUp) {
    let planned = job.plan(now, catch_up);
    tracing::debug!(
        job = %job.id,
        next_fire = %job.next_fire.map_or_else(|| "none".to_owned(), |fire| Utc(fire).to_string()),
        ?catch_up,
        "planned the next fire"
    );
    match planned {
        Some(Plan::CatchUp(missed)) => report::line(
            "Catch-up",
            &format!("{} missed {}, running it now", job.id, Utc(missed)),
        ),
        Some(Plan::Missed(missed)) => {
            report::line("Missed", &format!("{} missed {}", job.id, Utc(missed)));
        }
        None => {}
    }
}

/// Why a manifest cannot become a job at all: it does not load, or it names
/// no schedule. A job that loads but cannot run keeps its own error instead.
fn manifest_error(jobs: &[Job]) -> Option<&str> {
    jobs.iter().find_map(|job| match &job.definition {
        Definition::Unloadable(error) => Some(error.as_str()),
        Definition::Schedule { .. } => None,
    })
}

#[cfg(test)]
mod tests {
    use beez_test_support::TemporaryDirectory;

    use super::JobSet;
    use crate::control::JobId;
    use crate::job::Job;
    use crate::message::Refusal;
    use crate::store::{Registry, States, Watched};

    const TASKS: &str = "tasks:\n  - id: mark\n    sandbox: false\n    command: [\"true\"]\n";
    const NIGHTLY: &str = "schedule:\n  cron: \"0 3 * * *\"\n  allow_unsandboxed: true\n";

    fn watched(directory: &TemporaryDirectory, file: &str, schedule: &str) -> Watched {
        std::fs::create_dir_all(directory.join(file).parent().unwrap()).unwrap();
        Watched {
            path: directory.write(file, format!("{schedule}{TASKS}")).unwrap(),
            working_directory: directory.path().to_path_buf(),
        }
    }

    fn ids(jobs: &[Job]) -> Vec<JobId> {
        jobs.iter().map(|job| job.id.clone()).collect()
    }

    #[test]
    fn refuses_a_manifest_that_does_not_load_or_an_id_another_uses() {
        let directory = TemporaryDirectory::new("daemon-job-set-admit").unwrap();
        let mut registry = Registry::default();
        registry.add(watched(&directory, "a/nightly.yaml", NIGHTLY));
        let states = States::default();
        let set = JobSet::build(&registry, &states);

        let broken = watched(
            &directory,
            "broken.yaml",
            "schedule:\n  cron: \"not a cron\"\n",
        );
        let taken = watched(&directory, "b/nightly.yaml", NIGHTLY);
        let free = watched(&directory, "weekly.yaml", NIGHTLY);
        let again = watched(&directory, "a/nightly.yaml", NIGHTLY);

        assert!(matches!(
            set.admit(&broken, &states),
            Err(Refusal::Broken(_))
        ));
        assert!(matches!(
            set.admit(&taken, &states),
            Err(Refusal::Taken(id)) if id == JobId::new("nightly", None)
        ));
        assert_eq!(
            ids(&set.admit(&free, &states).unwrap()),
            [JobId::new("weekly", None)]
        );
        // A manifest read again does not collide with its own jobs.
        assert_eq!(
            ids(&set.admit(&again, &states).unwrap()),
            [JobId::new("nightly", None)]
        );
    }
}
