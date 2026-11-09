//! The files the daemon keeps between runs.
//!
//! `manifests.json` lists the manifests the daemon watches. The manifests
//! themselves hold the schedules, so nothing here can say when a job runs.
//! `state.json` holds what already happened, so a restart does not repeat a
//! fire or forget a paused job.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::control::JobId;
use crate::error::DaemonError;
use crate::message::{self, Refusal, Watch};
use crate::paths::DaemonPaths;

/// Layout version of the files.
const SCHEMA: u32 = 1;

/// Reads both files of one daemon, treating a missing file as an empty one.
pub(crate) fn load_all(paths: &DaemonPaths) -> Result<(Registry, States), DaemonError> {
    Ok((
        Registry::load(&paths.manifests())?,
        States::load(&paths.state())?,
    ))
}

/// The manifests the daemon watches.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Registry {
    schema: u32,
    manifests: Vec<Watched>,
    /// The file the registry was read from, and is written to.
    #[serde(skip)]
    path: PathBuf,
}

/// One watched manifest and the directory its tasks run in.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Watched {
    /// Absolute path of the manifest.
    pub(crate) path: PathBuf,
    /// Absolute directory the tasks run in.
    pub(crate) working_directory: PathBuf,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            manifests: Vec::new(),
            path: PathBuf::new(),
        }
    }
}

impl Registry {
    /// Reads the registry, treating a missing file as an empty one.
    ///
    /// The registry remembers `path`, so [`Registry::save`] writes it back.
    pub(crate) fn load(path: &Path) -> Result<Self, DaemonError> {
        let mut registry: Self = load_json(path)?;
        registry.path = path.to_path_buf();

        Ok(registry)
    }

    /// Every watched manifest.
    #[must_use]
    pub(crate) fn manifests(&self) -> &[Watched] {
        &self.manifests
    }

    /// True when the manifest is already watched.
    #[must_use]
    pub(crate) fn watches(&self, path: &Path) -> bool {
        self.manifests.iter().any(|watched| watched.path == path)
    }

    /// Adds a manifest, replacing an entry for the same file.
    ///
    /// Says whether the daemon already watched the manifest.
    pub(crate) fn add(&mut self, watched: Watched) -> Watch {
        let watch = if self.watches(&watched.path) {
            Watch::Again
        } else {
            Watch::New
        };
        self.manifests.retain(|entry| entry.path != watched.path);
        self.manifests.push(watched);

        watch
    }

    /// Removes a manifest and reports whether it was there.
    pub(crate) fn remove(&mut self, path: &Path) -> bool {
        let before = self.manifests.len();
        self.manifests.retain(|entry| entry.path != path);

        before != self.manifests.len()
    }

    /// Writes the registry to the file it was read from.
    pub(crate) fn save(&self) -> io::Result<()> {
        save_json(&self.path, self)
    }

    /// Adds a manifest to the list and writes the list.
    ///
    /// Answers with the manifest, and whether the daemon is new to it. A
    /// command takes effect with or without a running daemon, and both paths
    /// change the list through here, so they answer in the same words.
    pub(crate) fn watch(&mut self, watched: Watched) -> io::Result<String> {
        let path = watched.path.clone();
        let watch = self.add(watched);
        self.save()?;

        Ok(message::watching(&path, watch))
    }

    /// Removes a manifest from the list and writes the list.
    ///
    /// Refuses a manifest that was not watched.
    pub(crate) fn unwatch(&mut self, manifest: &Path) -> io::Result<Result<String, Refusal>> {
        if !self.remove(manifest) {
            return Ok(Err(Refusal::NotWatched(manifest.to_path_buf())));
        }
        self.save()?;

        Ok(Ok(message::stopped_watching(manifest)))
    }
}

/// What already happened to each job.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct States {
    schema: u32,
    jobs: BTreeMap<JobId, JobState>,
    /// The file the state was read from, and is written to.
    #[serde(skip)]
    path: PathBuf,
}

/// What already happened to one job.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct JobState {
    /// True while a person holds the job back.
    #[serde(default)]
    pub(crate) paused: bool,
    /// The last time the job fired.
    #[serde(default, with = "instant")]
    pub(crate) last_fire: Option<SystemTime>,
    /// The directory of the last run the job started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_run: Option<String>,
    /// The status of the last run. `None` means it never ran to the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_status: Option<i32>,
}

impl Default for States {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            jobs: BTreeMap::new(),
            path: PathBuf::new(),
        }
    }
}

impl States {
    /// Reads the state, treating a missing file as an empty one.
    ///
    /// The state remembers `path`, so [`States::save`] writes it back.
    pub(crate) fn load(path: &Path) -> Result<Self, DaemonError> {
        let mut states: Self = load_json(path)?;
        states.path = path.to_path_buf();

        Ok(states)
    }

    /// The state of one job, or the default for a job that never ran.
    #[must_use]
    pub(crate) fn get(&self, job: &JobId) -> JobState {
        self.jobs.get(job).cloned().unwrap_or_default()
    }

    /// Replaces the state of one job.
    pub(crate) fn set(&mut self, job: &JobId, state: JobState) {
        self.jobs.insert(job.clone(), state);
    }

    /// Replaces the state of one job and writes the state.
    pub(crate) fn record(&mut self, job: &JobId, state: JobState) -> io::Result<()> {
        self.set(job, state);
        self.save()
    }

    /// Changes the state of one job and writes the state.
    pub(crate) fn update(
        &mut self,
        job: &JobId,
        change: impl FnOnce(&mut JobState),
    ) -> io::Result<()> {
        let mut state = self.get(job);
        change(&mut state);
        self.record(job, state)
    }

    /// Drops the state of every job no manifest of `registry` may hold, and
    /// says whether any went.
    ///
    /// Ownership follows the stem of each watched manifest, not the jobs it
    /// holds right now: a manifest that does not load holds only the job
    /// named by its stem, and the state of its named schedules must outlast
    /// the mistake.
    ///
    /// Stems are not unique: `admit` refuses only an equal job ID, so
    /// `a/nightly.yaml` and `b/nightly.yaml` may both be watched with
    /// differently named schedules. The state of a removed manifest whose
    /// stem another watched manifest shares stays until that one goes too.
    pub(crate) fn keep_watched(&mut self, registry: &Registry) -> bool {
        let stems: Vec<String> = registry
            .manifests()
            .iter()
            .map(|watched| crate::job::stem(&watched.path))
            .collect();
        let before = self.jobs.len();
        self.jobs
            .retain(|id, _| stems.iter().any(|stem| id.belongs_to(stem)));

        before != self.jobs.len()
    }

    /// Writes the state to the file it was read from.
    pub(crate) fn save(&self) -> io::Result<()> {
        save_json(&self.path, self)
    }
}

fn load_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T, DaemonError> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => return Err(error.into()),
    };

    serde_json::from_str(&source).map_err(|source| DaemonError::Unreadable {
        path: path.to_path_buf(),
        source,
    })
}

/// Writes a file in one step, so a stopped daemon never leaves half a file.
pub(crate) fn save_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let temporary = path.with_extension("writing");
    let mut file = File::create(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    writeln!(file)?;
    file.sync_all()?;
    drop(file);

    fs::rename(temporary, path)
}

/// A Beez root that watches three manifests, for tests of what removing one
/// keeps.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::PathBuf;

    use beez_test_support::TemporaryDirectory;

    use super::{JobState, Registry, States, Watched};
    use crate::control::JobId;
    use crate::paths::DaemonPaths;

    const TASKS: &str = "tasks:\n  - id: mark\n    sandbox: false\n    command: [\"true\"]\n";

    /// The manifests [`watch_three`] writes.
    pub(crate) struct Watching {
        /// A manifest with the named schedule `a`, the one tests remove.
        pub(crate) weekly: PathBuf,
    }

    /// Watches three manifests and gives each job paused state:
    ///
    /// - `nightly.yaml` no longer loads, so it holds the job `nightly`, yet
    ///   its schedule `nightly:docs` has state from when it loaded.
    /// - `weekly.yaml` holds `weekly:a`, and `weekly` has state from before
    ///   the schedule had a name.
    /// - `monthly.yaml` holds `monthly`.
    pub(crate) fn watch_three(directory: &TemporaryDirectory, paths: &DaemonPaths) -> Watching {
        let broken = format!("schedule:\n  cron: \"not a cron\"\n{TASKS}");
        let named = format!(
            "schedule:\n  - name: a\n    cron: \"0 3 * * *\"\n    allow_unsandboxed: true\n{TASKS}"
        );
        let single =
            format!("schedule:\n  cron: \"0 3 1 * *\"\n  allow_unsandboxed: true\n{TASKS}");
        paths.create().unwrap();
        let mut registry = Registry::load(&paths.manifests()).unwrap();
        let mut weekly = PathBuf::new();
        for (file, text) in [
            ("nightly.yaml", broken),
            ("weekly.yaml", named),
            ("monthly.yaml", single),
        ] {
            let path = directory.write(file, text).unwrap();
            if file == "weekly.yaml" {
                weekly.clone_from(&path);
            }
            registry.add(Watched {
                path,
                working_directory: directory.path().to_path_buf(),
            });
        }
        let mut states = States::load(&paths.state()).unwrap();
        for id in ["nightly:docs", "weekly", "weekly:a", "monthly"] {
            states.set(&id_of(id), paused());
        }
        registry.save().unwrap();
        states.save().unwrap();

        Watching { weekly }
    }

    /// Asserts that `states` lost the state of `weekly.yaml` and kept the rest.
    pub(crate) fn assert_only_weekly_dropped(states: &States) {
        let kept: Vec<String> = states.jobs.keys().map(JobId::to_string).collect();

        assert_eq!(kept, ["monthly", "nightly:docs"]);
        assert!(states.get(&id_of("nightly:docs")).paused);
        assert!(states.get(&id_of("monthly")).paused);
    }

    fn id_of(text: &str) -> JobId {
        JobId::from(text.to_owned())
    }

    fn paused() -> JobState {
        JobState {
            paused: true,
            ..JobState::default()
        }
    }
}

/// Reads and writes an instant as a UTC date and time.
mod instant {
    #![allow(
        clippy::ref_option,
        reason = "serde hands a reference to the field it writes"
    )]

    use std::time::SystemTime;

    use beez_schedule::{Utc, parse_instant};
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        time: &Option<SystemTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match time.as_ref() {
            Some(time) => serializer.collect_str(&Utc(*time)),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        let Some(text) = Option::<String>::deserialize(deserializer)? else {
            return Ok(None);
        };

        parse_instant(&text)
            .map(Some)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{JobState, Registry, States, Watched};
    use crate::control::JobId;
    use beez_schedule::parse_instant;
    use beez_test_support::TemporaryDirectory;

    fn id(text: &str) -> JobId {
        JobId::from(text.to_owned())
    }

    #[test]
    fn reads_a_missing_file_as_an_empty_one() {
        let directory = TemporaryDirectory::new("daemon-store-missing").unwrap();

        assert!(
            Registry::load(&directory.join("manifests.json"))
                .unwrap()
                .manifests()
                .is_empty()
        );
        assert!(
            !States::load(&directory.join("state.json"))
                .unwrap()
                .get(&id("nightly"))
                .paused
        );
    }

    #[test]
    fn writes_the_layout_version_of_a_new_file() {
        let directory = TemporaryDirectory::new("daemon-store-schema").unwrap();
        let path = directory.join("state.json");
        States::load(&path).unwrap().save().unwrap();

        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"schema\": 1")
        );
    }

    #[test]
    fn keeps_one_entry_for_one_manifest() {
        let mut registry = Registry::default();
        let watched = Watched {
            path: "/work/nightly.yaml".into(),
            working_directory: "/work".into(),
        };
        registry.add(watched.clone());
        registry.add(watched);

        assert_eq!(registry.manifests().len(), 1);
        assert!(registry.watches("/work/nightly.yaml".as_ref()));
        assert!(registry.remove("/work/nightly.yaml".as_ref()));
        assert!(!registry.remove("/work/nightly.yaml".as_ref()));
    }

    #[test]
    fn writes_and_reads_the_state_of_a_job() {
        let directory = TemporaryDirectory::new("daemon-store-state").unwrap();
        let path = directory.join("state.json");
        let mut states = States::load(&path).unwrap();
        states
            .record(
                &id("nightly"),
                JobState {
                    paused: true,
                    last_fire: Some(parse_instant("2026-09-10T03:00:00Z").unwrap()),
                    last_run: Some("20260910T030000000Z-11".to_owned()),
                    last_status: Some(0),
                },
            )
            .unwrap();

        let read = States::load(&path).unwrap().get(&id("nightly"));

        assert!(read.paused);
        assert_eq!(read.last_status, Some(0));
        assert_eq!(
            read.last_fire,
            Some(parse_instant("2026-09-10T03:00:00Z").unwrap())
        );
    }

    #[test]
    fn changes_the_state_of_one_job_and_writes_it_where_it_was_read() {
        let directory = TemporaryDirectory::new("daemon-store-update").unwrap();
        let path = directory.join("state.json");
        let mut states = States::load(&path).unwrap();

        states
            .update(&id("nightly"), |state| state.paused = true)
            .unwrap();

        assert!(states.get(&id("nightly")).paused);
        assert!(States::load(&path).unwrap().get(&id("nightly")).paused);
    }

    #[test]
    fn drops_the_state_of_every_job_of_a_manifest_that_is_gone() {
        let mut registry = Registry::default();
        registry.add(Watched {
            path: "/work/weekly.yaml".into(),
            working_directory: "/work".into(),
        });
        let mut states = States::default();
        for job in ["nightly", "nightly:docs", "weekly", "weekly:a"] {
            states.set(
                &id(job),
                JobState {
                    paused: true,
                    ..JobState::default()
                },
            );
        }

        assert!(states.keep_watched(&registry));
        assert!(!states.keep_watched(&registry));

        let kept: Vec<String> = states.jobs.keys().map(JobId::to_string).collect();
        assert_eq!(kept, ["weekly", "weekly:a"]);
        assert!(states.get(&id("weekly:a")).paused);
    }
}
