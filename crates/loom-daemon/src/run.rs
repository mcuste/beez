//! One run of one job.
//!
//! The daemon reads the manifest again here, so a run uses the tasks the file
//! holds at the moment it fires. The artifacts land in `.loom/runs` beside the
//! runs a person starts, and they hold the same lines.

use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use loom_manifest::load;
use loom_record::{LogSettings, RunRecorder, RunTarget, counts, seconds};
use loom_runner::{RunEvent, Runner};
use loom_schedule::format_instant;

use crate::report;

/// What one job needs to run.
#[derive(Clone, Debug)]
pub(crate) struct JobRun {
    pub(crate) id: String,
    pub(crate) manifest: PathBuf,
    pub(crate) working_directory: PathBuf,
    /// Loom root that holds the artifacts of every run the daemon starts.
    pub(crate) log_directory: PathBuf,
    /// The time the job was due, which the run log names.
    pub(crate) fire: SystemTime,
}

/// What one run produced.
#[derive(Clone, Debug)]
pub(crate) struct RunOutcome {
    /// Directory name of the run, inside `.loom/runs`.
    pub(crate) directory: Option<String>,
    /// Status the workflow ended with. `None` means it could not run.
    pub(crate) status: Option<i32>,
    /// The counts and the time, as the run log writes them.
    pub(crate) summary: String,
    /// Why the run could not start or finish.
    pub(crate) error: Option<String>,
}

/// Counts how the tasks of one run ended.
#[derive(Debug, Default)]
struct Counts {
    passed: usize,
    failed: usize,
    blocked: usize,
}

impl Counts {
    fn add(&mut self, event: &RunEvent) {
        match event {
            RunEvent::Finished { output, .. } if output.succeeded() => self.passed += 1,
            RunEvent::Finished { .. } | RunEvent::Failed { .. } => self.failed += 1,
            RunEvent::Blocked { .. } => self.blocked += 1,
            RunEvent::Started { .. } | RunEvent::Output { .. } => {}
        }
    }

    fn summary(&self, elapsed: std::time::Duration) -> String {
        format!(
            "{} in {}",
            counts(self.passed, self.failed, self.blocked),
            seconds(elapsed)
        )
    }
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
    let mut recorder = match RunRecorder::create(settings, &target, &job.working_directory) {
        Some(Ok(recorder)) => Some(recorder),
        Some(Err(error)) => {
            report::line("Warning", &format!("{}: run log: {error}", job.id));
            None
        }
        None => None,
    };
    let directory = recorder.as_ref().and_then(|recorder| {
        recorder
            .directory()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    if let Some(directory) = &directory {
        report::line("Logging", &format!("{}: runs/{directory}", job.id));
    }
    // The run log says which fire started the run, so a run explains itself.
    record(
        &mut recorder,
        |recorder| {
            recorder.status(
                "Trigger",
                &format!("{} scheduled for {}", job.id, format_instant(job.fire)),
            )
        },
        &job.id,
    );

    let mut tallies = Counts::default();
    let outcome = Runner::new(&job.working_directory).run_workflow(&workflow, &mut |event| {
        tallies.add(event);
        match recorder.as_mut() {
            Some(recorder) => recorder.event(event),
            None => Ok(()),
        }
    });
    let summary = tallies.summary(started.elapsed());
    let status = outcome.as_ref().ok().copied();
    record(
        &mut recorder,
        |recorder| {
            recorder.status("Summary", &summary)?;
            recorder.finish(status)
        },
        &job.id,
    );

    RunOutcome {
        directory,
        status,
        summary,
        error: outcome.err().map(|error| error.to_string()),
    }
}

/// Writes to the run log, and gives the log up after one failure so the run
/// itself keeps its result.
fn record(
    recorder: &mut Option<RunRecorder>,
    action: impl FnOnce(&mut RunRecorder) -> std::io::Result<()>,
    job: &str,
) {
    let Some(open) = recorder.as_mut() else {
        return;
    };
    if let Err(error) = action(open) {
        report::line("Warning", &format!("{job}: run log: {error}"));
        *recorder = None;
    }
}

fn failure(error: &str) -> RunOutcome {
    RunOutcome {
        directory: None,
        status: None,
        summary: "0 passed".to_owned(),
        error: Some(error.to_owned()),
    }
}
