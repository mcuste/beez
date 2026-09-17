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

use crate::paths::DaemonPaths;

/// Layout version of the files.
const SCHEMA: u32 = 1;

/// Reads both files of one daemon, treating a missing file as an empty one.
pub(crate) fn load_all(paths: &DaemonPaths) -> io::Result<(Registry, States)> {
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
        }
    }
}

impl Registry {
    /// Reads the registry, treating a missing file as an empty one.
    pub(crate) fn load(path: &Path) -> io::Result<Self> {
        load_json(path)
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
    /// Returns false when the daemon already watched the manifest.
    pub(crate) fn add(&mut self, watched: Watched) -> bool {
        let added = !self.watches(&watched.path);
        self.manifests.retain(|entry| entry.path != watched.path);
        self.manifests.push(watched);

        added
    }

    /// Removes a manifest and reports whether it was there.
    pub(crate) fn remove(&mut self, path: &Path) -> bool {
        let before = self.manifests.len();
        self.manifests.retain(|entry| entry.path != path);

        before != self.manifests.len()
    }

    /// Writes the registry.
    pub(crate) fn save(&self, path: &Path) -> io::Result<()> {
        save_json(path, self)
    }
}

/// What already happened to each job.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct States {
    schema: u32,
    jobs: BTreeMap<String, JobState>,
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
        }
    }
}

impl States {
    /// Reads the state, treating a missing file as an empty one.
    pub(crate) fn load(path: &Path) -> io::Result<Self> {
        load_json(path)
    }

    /// The state of one job, or the default for a job that never ran.
    #[must_use]
    pub(crate) fn get(&self, job: &str) -> JobState {
        self.jobs.get(job).cloned().unwrap_or_default()
    }

    /// Replaces the state of one job.
    pub(crate) fn set(&mut self, job: &str, state: JobState) {
        self.jobs.insert(job.to_owned(), state);
    }

    /// Drops the state of every job that is no longer there.
    pub(crate) fn keep_only(&mut self, jobs: &[String]) {
        self.jobs.retain(|id, _| jobs.iter().any(|job| job == id));
    }

    /// Writes the state.
    pub(crate) fn save(&self, path: &Path) -> io::Result<()> {
        save_json(path, self)
    }
}

fn load_json<T: DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => return Err(error),
    };

    serde_json::from_str(&source).map_err(io::Error::other)
}

/// Writes a file in one step, so a stopped daemon never leaves half a file.
fn save_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let temporary = path.with_extension("writing");
    let mut file = File::create(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value).map_err(io::Error::other)?;
    writeln!(file)?;
    file.sync_all()?;
    drop(file);

    fs::rename(temporary, path)
}

/// Reads and writes an instant as a UTC date and time.
mod instant {
    #![allow(
        clippy::ref_option,
        reason = "serde hands a reference to the field it writes"
    )]

    use std::time::SystemTime;

    use loom_schedule::{format_instant, parse_instant};
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        time: &Option<SystemTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match time.as_ref() {
            Some(time) => serializer.serialize_str(&format_instant(*time)),
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
    use loom_schedule::parse_instant;
    use loom_test_support::TemporaryDirectory;

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
                .get("nightly")
                .paused
        );
    }

    #[test]
    fn writes_the_layout_version_of_a_new_file() {
        let directory = TemporaryDirectory::new("daemon-store-schema").unwrap();
        let path = directory.join("state.json");
        States::default().save(&path).unwrap();

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
        let mut states = States::default();
        states.set(
            "nightly",
            JobState {
                paused: true,
                last_fire: Some(parse_instant("2026-09-10T03:00:00Z").unwrap()),
                last_run: Some("20260910T030000000Z-11".to_owned()),
                last_status: Some(0),
            },
        );
        states.save(&path).unwrap();

        let read = States::load(&path).unwrap().get("nightly");

        assert!(read.paused);
        assert_eq!(read.last_status, Some(0));
        assert_eq!(
            read.last_fire,
            Some(parse_instant("2026-09-10T03:00:00Z").unwrap())
        );
    }

    #[test]
    fn drops_the_state_of_a_job_that_is_gone() {
        let mut states = States::default();
        states.set("nightly", JobState::default());
        states.set("weekly", JobState::default());
        states.set(
            "weekly",
            JobState {
                paused: true,
                ..JobState::default()
            },
        );

        states.keep_only(&["weekly".to_owned()]);

        assert!(states.get("weekly").paused);
        assert!(!states.get("nightly").paused);
    }
}
