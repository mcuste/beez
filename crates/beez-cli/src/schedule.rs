//! The `daemon` and `schedule` commands, and how they print what the daemon
//! holds.

use std::io::{self, Write};
use std::path::PathBuf;

use beez_daemon::{DaemonPaths, DaemonStatus, JobReport, Request, Response, RunLimits, Status};
use beez_record::status_text;
use clap::{Args, Subcommand};

/// Columns of the job table, in the order they are printed.
const HEADINGS: [&str; 5] = ["JOB", "CONDITION", "SCHEDULE", "NEXT FIRE", "LAST RUN"];

#[derive(Debug, Subcommand)]
pub(crate) enum DaemonCommand {
    /// Run the daemon in this terminal.
    Run(DaemonRun),
    /// Start the daemon in the background.
    Start(DaemonRun),
    /// Stop the daemon once its running runs end.
    Stop(RootArgs),
    /// Report the daemon and every job it holds.
    Status(RootArgs),
    /// Read every watched manifest again.
    Reload(RootArgs),
}

#[derive(Debug, Args)]
pub(crate) struct DaemonRun {
    #[command(flatten)]
    root: RootArgs,
    /// How many runs the daemon starts at the same time.
    #[arg(long, value_name = "COUNT", default_value_t = beez_daemon::DEFAULT_RUN_LIMIT)]
    limit: usize,
    /// How many runs to keep in the Beez root. Zero keeps every run.
    #[arg(long, value_name = "COUNT", default_value_t = beez_daemon::DEFAULT_KEPT_RUNS)]
    keep_runs: usize,
}

impl DaemonRun {
    fn limits(&self) -> RunLimits {
        RunLimits {
            limit: self.limit,
            keep_runs: self.keep_runs,
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum ScheduleCommand {
    /// Watch one more manifest, so its schedules fire.
    Add(ManifestArgs),
    /// Stop watching one manifest.
    Remove(ManifestArgs),
    /// Report every job.
    List(RootArgs),
    /// Run one job now, beside its schedule.
    Trigger(JobArgs),
    /// Hold one job back until it resumes.
    Pause(JobArgs),
    /// Let a paused job fire again.
    Resume(JobArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ManifestArgs {
    /// Path of the workflow manifest.
    manifest: PathBuf,
    #[command(flatten)]
    root: RootArgs,
}

#[derive(Debug, Args)]
pub(crate) struct JobArgs {
    /// Job ID, as `beez schedule list` prints it.
    job: String,
    #[command(flatten)]
    root: RootArgs,
}

#[derive(Debug, Args)]
pub(crate) struct RootArgs {
    /// Directory that holds the daemon's own files and the runs it starts.
    ///
    /// Beez uses `~/.beez` by default, so one daemon serves every repository.
    #[arg(long, value_name = "PATH", env = "BEEZ_ROOT")]
    root: Option<PathBuf>,
}

impl RootArgs {
    fn paths(&self) -> io::Result<DaemonPaths> {
        match &self.root {
            Some(root) => Ok(DaemonPaths::new(root)),
            None => DaemonPaths::user(),
        }
    }
}

/// Runs one `beez daemon` command.
pub(crate) fn daemon(command: DaemonCommand) -> io::Result<i32> {
    match command {
        DaemonCommand::Run(run) => {
            beez_daemon::run(&run.root.paths()?, run.limits())?;
            Ok(0)
        }
        DaemonCommand::Start(run) => Ok(answer(beez_daemon::start(
            &run.root.paths()?,
            run.limits(),
        )?)),
        DaemonCommand::Stop(root) => send(&root, Request::Stop),
        DaemonCommand::Status(root) => report_jobs(&root),
        DaemonCommand::Reload(root) => send(&root, Request::Reload),
    }
}

/// Runs one `beez schedule` command.
pub(crate) fn schedule(command: ScheduleCommand) -> io::Result<i32> {
    match command {
        ScheduleCommand::Add(args) => Ok(answer(beez_daemon::add(
            &args.root.paths()?,
            &args.manifest,
        )?)),
        ScheduleCommand::Remove(args) => send(
            &args.root,
            Request::Remove {
                manifest: args.manifest,
            },
        ),
        ScheduleCommand::List(root) => report_jobs(&root),
        ScheduleCommand::Trigger(args) => send(
            &args.root,
            Request::Trigger {
                job: args.job.into(),
            },
        ),
        ScheduleCommand::Pause(args) => send(
            &args.root,
            Request::Pause {
                job: args.job.into(),
            },
        ),
        ScheduleCommand::Resume(args) => send(
            &args.root,
            Request::Resume {
                job: args.job.into(),
            },
        ),
    }
}

/// Gives one command, whether the daemon runs or not, and prints its answer.
fn send(root: &RootArgs, request: Request) -> io::Result<i32> {
    Ok(answer(beez_daemon::command(&root.paths()?, request)?))
}

/// Prints the daemon and every job it holds.
fn report_jobs(root: &RootArgs) -> io::Result<i32> {
    let status = beez_daemon::status(&root.paths()?)?;
    let mut out = io::stdout().lock();
    unless_closed(write_status(&mut out, &status).and_then(|()| out.flush()))?;

    Ok(0)
}

/// Prints what a command answered, and returns the status for it.
///
/// The status stands even when the answer cannot be printed, since the
/// command itself already happened.
fn answer(response: Response) -> i32 {
    match response {
        Response::Done { message } => {
            let _ = writeln!(io::stdout(), "{message}");
            0
        }
        Response::Error { message } => {
            let _ = writeln!(io::stderr(), "{message}");
            1
        }
        Response::Status(_) => {
            let _ = writeln!(io::stderr(), "unexpected answer from the daemon");
            2
        }
    }
}

/// Takes a reader that stopped reading, as `head` does, for the end of the
/// output rather than a failure, the way other command-line tools do.
fn unless_closed(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

/// Writes the daemon's own line, then its jobs.
fn write_status(out: &mut impl Write, status: &Status) -> io::Result<()> {
    let jobs = match status {
        Status::Running(DaemonStatus {
            pid,
            started,
            running,
            jobs,
        }) => {
            writeln!(
                out,
                "Daemon running, pid {pid}, started {started}, {running} running"
            )?;
            jobs
        }
        Status::Stopped(jobs) => {
            writeln!(out, "Daemon not running")?;
            jobs
        }
    };

    write_table(out, jobs)
}

/// Writes one row for each job, and the reason under a broken one.
fn write_table(out: &mut impl Write, jobs: &[JobReport]) -> io::Result<()> {
    if jobs.is_empty() {
        return writeln!(
            out,
            "No manifest is watched. Add one with: beez schedule add <manifest>"
        );
    }
    let rows: Vec<[String; 5]> = jobs.iter().map(row).collect();
    let widths = widths(&rows);
    write_row(out, &HEADINGS.map(str::to_owned), &widths)?;
    for (row, job) in rows.iter().zip(jobs) {
        write_row(out, row, &widths)?;
        if let Some(error) = &job.error {
            writeln!(out, "  {error}")?;
        }
    }

    Ok(())
}

fn row(job: &JobReport) -> [String; 5] {
    [
        job.id.clone(),
        job.condition.to_string(),
        or_dash(job.schedule.as_deref()),
        or_dash(job.next_fire.as_deref()),
        last_run(job),
    ]
}

fn last_run(job: &JobReport) -> String {
    let Some(directory) = &job.last_run else {
        return or_dash(None);
    };
    match job.last_status {
        // No status means the run never reached its end, so there is none to name.
        None => format!("{directory} (unfinished)"),
        status => format!("{directory} ({})", status_text(status)),
    }
}

/// The value of a column, or a dash when the job has none.
fn or_dash(value: Option<&str>) -> String {
    value.unwrap_or("-").to_owned()
}

/// As wide as the longest value of each column, the heading included.
fn widths(rows: &[[String; 5]]) -> [usize; 5] {
    let mut widths = HEADINGS.map(str::len);
    for row in rows {
        for (width, value) in widths.iter_mut().zip(row) {
            *width = (*width).max(value.len());
        }
    }

    widths
}

/// Writes one row, with no padding after the last column.
fn write_row(out: &mut impl Write, row: &[String; 5], widths: &[usize; 5]) -> io::Result<()> {
    let last = row.len().saturating_sub(1);
    for (index, (value, width)) in row.iter().zip(widths).enumerate() {
        if index == last {
            writeln!(out, "{value}")?;
        } else {
            write!(out, "{value:<width$}  ")?;
        }
    }

    Ok(())
}
