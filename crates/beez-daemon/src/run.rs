//! One run of one job.
//!
//! The daemon reads the manifest again here, so a run uses the tasks the file
//! holds at the moment it fires. The artifacts land in `.beez/runs` beside the
//! runs a person starts, and they hold the same lines.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use beez_manifest::load;
use beez_record::{LogSettings, OpenLog, RunRecorder, RunTally, RunTarget};
use beez_runner::Runner;
use beez_schedule::format_instant;

use crate::report;

/// What one job needs to run.
#[derive(Clone, Debug)]
pub(crate) struct JobRun {
    pub(crate) id: String,
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
    /// Status the workflow ended with. `None` means it could not run.
    pub(crate) status: Option<i32>,
    /// The counts and the time, as the run log writes them.
    pub(crate) summary: String,
    /// Why the run could not start or finish.
    pub(crate) error: Option<String>,
}

/// Runs one job to the end.
pub(crate) fn execute(job: &JobRun) -> RunOutcome {
    let started = Instant::now();
    let manifest = match load(&job.manifest) {
        Ok(manifest) => manifest,
        Err(error) => return failure(&error.to_string()),
    };
    let workflow = manifest.into_workflow();
    let target = RunTarget::Workflow {
        path: &job.manifest,
        workflow: &workflow,
    };
    // Every run the daemon starts lands in its own root, so one directory
    // holds them all, even when a job runs in another repository.
    let settings = LogSettings {
        enabled: true,
        directory: Some(&job.log_directory),
    };
    let (mut log, failure) = OpenLog::open(settings, &target, &job.working_directory);
    if let Some(failure) = failure {
        warn(&job.id, &failure);
    }
    let directory = log
        .directory()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned());
    if let Some(directory) = &directory {
        report::line("Logging", &format!("{}: runs/{directory}", job.id));
    }
    // The run log says which fire started the run, so a run explains itself.
    record(
        &mut log,
        |recorder| {
            recorder.status(
                "Trigger",
                &format!("{} scheduled for {}", job.id, format_instant(job.fire)),
            )
        },
        &job.id,
    );

    let mut tally = RunTally::default();
    let outcome = Runner::new(&job.working_directory).run_workflow(&workflow, &mut |event| {
        tally.record(event);
        record(&mut log, |recorder| recorder.event(event), &job.id);
        Ok(())
    });
    let summary = tally.summary(started.elapsed());
    let status = outcome.as_ref().ok().copied();
    record(
        &mut log,
        |recorder| recorder.finish(&summary, status),
        &job.id,
    );

    RunOutcome {
        directory,
        status,
        summary,
        error: outcome.err().map(|error| error.to_string()),
    }
}

/// Writes to the run log, and names the job of a log that could not be written.
fn record(
    log: &mut OpenLog,
    action: impl FnOnce(&mut RunRecorder) -> std::io::Result<()>,
    job: &str,
) {
    if let Some(failure) = log.write(action) {
        warn(job, &failure);
    }
}

fn warn(job: &str, failure: &str) {
    report::line("Warning", &format!("{job}: {failure}"));
}

fn failure(error: &str) -> RunOutcome {
    RunOutcome {
        directory: None,
        status: None,
        summary: "0 passed".to_owned(),
        error: Some(error.to_owned()),
    }
}
