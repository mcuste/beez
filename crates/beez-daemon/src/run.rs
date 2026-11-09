//! One run of one job.
//!
//! The daemon reads the manifest again here, so a run uses the tasks the file
//! holds at the moment it fires. The artifacts land in `.beez/runs` beside the
//! runs a person starts, and they hold the same lines.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use beez_manifest::load;
use beez_record::{LogFailure, LogSettings, OpenLog, RunTarget};
use beez_runner::Runner;
use beez_schedule::Utc;

use crate::control::JobId;
use crate::error::one_line;
use crate::report;

/// What one job needs to run.
#[derive(Clone, Debug)]
pub(crate) struct JobRun {
    pub(crate) id: JobId,
    pub(crate) manifest: PathBuf,
    pub(crate) working_directory: PathBuf,
    /// Beez root that holds the artifacts of every run the daemon starts.
    pub(crate) log_directory: PathBuf,
    /// The time the job was due, which the run log names.
    pub(crate) fire: SystemTime,
}

/// What one run produced.
#[derive(Clone, Debug)]
pub(crate) struct RunOutcome {
    /// Directory name of the run, inside `.beez/runs`.
    pub(crate) directory: Option<String>,
    /// Status the workflow ended with, or why the run could not start or
    /// finish.
    pub(crate) result: Result<i32, String>,
    /// The counts and the time, as the run log writes them.
    pub(crate) summary: String,
}

/// Runs one job to the end.
pub(crate) fn execute(job: &JobRun) -> RunOutcome {
    let started = Instant::now();
    let manifest = match load(&job.manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(manifest = %job.manifest.display(), %error, "cannot load the manifest of a run");
            return failure(&one_line(&error));
        }
    };
    let workflow = manifest.into_workflow();
    let target = RunTarget::Workflow {
        path: &job.manifest,
        workflow: &workflow,
    };
    // Every run the daemon starts lands in its own root, so one directory
    // holds them all, even when a job runs in another repository.
    let settings = LogSettings::At(&job.log_directory);
    let (mut log, failure) = OpenLog::open(settings, &target, &job.working_directory);
    warn(&job.id, failure);
    let directory = log
        .directory()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned());
    if let Some(directory) = &directory {
        report::line("Logging", &format!("{}: runs/{directory}", job.id));
    }
    // The run log says which fire started the run, so a run explains itself.
    let trigger = format!("{} scheduled for {}", job.id, Utc(job.fire));
    warn(
        &job.id,
        log.write(|recorder| recorder.status("Trigger", &trigger)),
    );

    let outcome = Runner::new(&job.working_directory).run_workflow(&workflow, &mut |event| {
        warn(&job.id, log.event(event));
        Ok(())
    });
    let (summary, failure) = log.finish(started.elapsed(), outcome.as_ref().ok().copied());
    warn(&job.id, failure);

    RunOutcome {
        directory,
        result: outcome.map_err(|error| one_line(&error)),
        summary,
    }
}

/// Shows a run log failure, named after its job.
fn warn(job: &JobId, failure: Option<LogFailure>) {
    if let Some(failure) = failure {
        report::line("Warning", &format!("{job}: {failure}"));
    }
}

fn failure(error: &str) -> RunOutcome {
    RunOutcome {
        directory: None,
        result: Err(error.to_owned()),
        summary: "0 passed".to_owned(),
    }
}
