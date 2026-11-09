//! Beez's user-facing command-line interface.

use std::error::Error;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;

use anstyle::{AnsiColor, Style};
use anyhow::Context;
use beez_manifest::load;
use beez_policy::{
    DomainRule, FilesystemPolicy, HarnessOptions, HeadlessHarness, NetworkPolicy, SandboxPath,
    SandboxPolicy,
};
use beez_process::{ExecutionRequest, HarnessCall, ProcessCall};
use beez_record::{LogSettings, RunTarget};
use beez_runner::{RunError, Runner};
use clap::{Args, Parser, Subcommand};

mod report;
mod sandbox;
mod schedule;
mod trace;

use report::{RenderOptions, Reporter};
use sandbox::{SandboxInit, SandboxRelay};
use schedule::{DaemonCommand, ScheduleCommand};

#[derive(Debug, Parser)]
#[command(name = "beez", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a supported coding harness, workflow, or direct process.
    Run {
        #[command(subcommand)]
        command: RunCommand,
    },
    /// Run workflows on the schedules their manifests declare.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Manage the manifests the daemon watches.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    /// Internal: first process inside a Linux sandbox.
    #[command(hide = true)]
    SandboxInit(SandboxInit),
    /// Internal: forwards loopback connections to a Unix socket inside a Linux sandbox.
    #[command(hide = true)]
    SandboxRelay(SandboxRelay),
}

#[derive(Debug, Subcommand)]
enum RunCommand {
    /// Run the tasks of a workflow manifest.
    Workflow(WorkflowFile),
    /// Prompt the Pi harness.
    Pi(HarnessRun),
    /// Prompt the Oh My Pi harness.
    Omp(HarnessRun),
    /// Prompt the Claude Code harness.
    Claude(HarnessRun),
    /// Prompt the Codex harness.
    Codex(HarnessRun),
    /// Run a program without a shell.
    Command(Process),
}

#[derive(Debug, Args)]
struct WorkflowFile {
    path: PathBuf,
    #[command(flatten)]
    render: RenderOptions,
    #[command(flatten)]
    log: LogArgs,
}

#[derive(Debug, Args)]
struct HarnessRun {
    prompt: OsString,
    /// Model the harness must use.
    #[arg(long)]
    model: Option<String>,
    /// Reasoning effort the harness must use.
    #[arg(long)]
    effort: Option<String>,
    #[command(flatten)]
    sandbox: SandboxArgs,
    #[command(flatten)]
    log: LogArgs,
}

#[derive(Debug, Args)]
struct Process {
    #[command(flatten)]
    sandbox: SandboxArgs,
    #[command(flatten)]
    log: LogArgs,
    program: OsString,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    arguments: Vec<OsString>,
}

#[derive(Debug, Args)]
struct LogArgs {
    /// Directory that holds the artifacts of every run.
    ///
    /// Beez writes to `.beez` in the repository root by default, or in the
    /// working directory outside a repository.
    #[arg(long, value_name = "PATH", env = "BEEZ_LOG_DIR")]
    log_dir: Option<PathBuf>,
    /// Write no artifacts for this run.
    #[arg(long)]
    no_log: bool,
}

impl LogArgs {
    fn settings(&self) -> LogSettings<'_> {
        match (self.no_log, self.log_dir.as_deref()) {
            (true, _) => LogSettings::Disabled,
            (false, None) => LogSettings::Resolved,
            (false, Some(directory)) => LogSettings::At(directory),
        }
    }
}

#[derive(Debug, Args)]
struct SandboxArgs {
    /// Run with no operating-system sandbox.
    #[arg(long, conflicts_with_all = ["allow_domain", "allow_write"])]
    no_sandbox: bool,
    /// Host the sandbox may also reach, such as github.com or *.npmjs.org:443.
    #[arg(long, value_name = "HOST")]
    allow_domain: Vec<DomainRule>,
    /// Path the sandbox may also write.
    #[arg(long, value_name = "PATH")]
    allow_write: Vec<SandboxPath>,
}

impl SandboxArgs {
    /// Beez's default policy, plus the hosts and paths the command allows.
    fn policy(&self) -> Option<SandboxPolicy> {
        if self.no_sandbox {
            return None;
        }
        let network = NetworkPolicy::default().with_allow(self.allow_domain.clone());
        let filesystem = FilesystemPolicy::default().with_write_allow(self.allow_write.clone());
        Some(SandboxPolicy::new(network, filesystem, None))
    }
}

/// The status Beez exits with when it fails itself, as for a usage error.
///
/// A run that Beez carried out exits with the status of its tasks instead.
const FAILURE: i32 = 2;

fn main() {
    let cli = Cli::parse();
    // The sandbox helpers share standard error with the sandboxed program,
    // so a trace line there would land in the task's own output.
    if !matches!(
        cli.command,
        Command::SandboxInit(_) | Command::SandboxRelay(_)
    ) {
        trace::init();
    }
    tracing::debug!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        "beez started"
    );
    let status = match run(cli) {
        Ok(status) => status,
        Err(error) => {
            tracing::debug!(error = %format_args!("{error:#}"), "beez failed");
            report_error(error.as_ref());
            FAILURE
        }
    };
    tracing::debug!(status, "beez exiting");
    std::process::exit(status);
}

/// Prints an error for a person: what failed on the first line, then each
/// cause behind it on a line of its own.
///
/// Standard error may be closed, so this is best effort; the exit status
/// still says Beez failed.
fn report_error(error: &(dyn Error + 'static)) {
    let mut stderr = anstream::stderr().lock();
    let _ = write_error(&mut stderr, error);
}

/// Writes `error` and its chain of causes, as [`report_error`] prints them.
fn write_error(out: &mut impl Write, error: &(dyn Error + 'static)) -> io::Result<()> {
    let style = Style::new().bold().fg_color(Some(AnsiColor::Red.into()));
    writeln!(out, "{style}error:{style:#} {error}")?;
    let mut source = error.source();
    while let Some(cause) = source {
        writeln!(out, "  caused by: {cause}")?;
        source = cause.source();
    }
    Ok(())
}

fn run(cli: Cli) -> anyhow::Result<i32> {
    match cli.command {
        Command::Run { command } => match command {
            RunCommand::Workflow(file) => run_workflow(file),
            RunCommand::Pi(run) => run_harness(HeadlessHarness::Pi, run),
            RunCommand::Omp(run) => run_harness(HeadlessHarness::Omp, run),
            RunCommand::Claude(run) => run_harness(HeadlessHarness::Claude, run),
            RunCommand::Codex(run) => run_harness(HeadlessHarness::Codex, run),
            RunCommand::Command(process) => run_command(process),
        },
        Command::Daemon { command } => schedule::daemon(command),
        Command::Schedule { command } => schedule::schedule(command),
        Command::SandboxInit(init) => sandbox::init(&init),
        Command::SandboxRelay(relay) => sandbox::relay(&relay),
    }
}

fn run_workflow(file: WorkflowFile) -> anyhow::Result<i32> {
    let WorkflowFile { path, render, log } = file;
    let workflow = load(&path)
        .map(beez_manifest::Manifest::into_workflow)
        .with_context(|| format!("cannot load workflow {}", path.display()))?;
    let target = RunTarget::Workflow {
        path: &path,
        workflow: &workflow,
    };

    run_reported(&target, render, log.settings(), |runner, reporter| {
        runner.run_workflow(&workflow, &mut |event| reporter.event(event))
    })
}

fn run_harness(harness: HeadlessHarness, run: HarnessRun) -> anyhow::Result<i32> {
    let HarnessRun {
        prompt,
        model,
        effort,
        sandbox,
        log,
    } = run;
    let request = ExecutionRequest::Harness(
        HarnessCall::new(harness, prompt).options(HarnessOptions::new(model, effort)),
    );

    run_request(harness.name(), request, &sandbox, &log)
}

fn run_command(process: Process) -> anyhow::Result<i32> {
    let Process {
        sandbox,
        log,
        program,
        arguments,
    } = process;
    let request = ExecutionRequest::Command(ProcessCall::new(program).arguments(arguments));

    run_request("command", request, &sandbox, &log)
}

/// Runs one direct request, named after the command that asked for it.
fn run_request(
    name: &str,
    request: ExecutionRequest,
    sandbox: &SandboxArgs,
    log: &LogArgs,
) -> anyhow::Result<i32> {
    let policy = sandbox.policy();
    let target = RunTarget::Request { name };

    run_reported(
        &target,
        RenderOptions::default(),
        log.settings(),
        |runner, reporter| {
            runner.run_request_in(request, policy.as_ref(), &mut |event| reporter.event(event))
        },
    )
}

/// Runs `run` in the working directory, reports its events, and closes the
/// run's own report before returning its status.
fn run_reported(
    target: &RunTarget<'_>,
    render: RenderOptions,
    log: LogSettings<'_>,
    run: impl FnOnce(&Runner, &mut Reporter) -> Result<i32, RunError>,
) -> anyhow::Result<i32> {
    let working_directory = std::env::current_dir().context("cannot read the working directory")?;
    let mut reporter = Reporter::new(target, render, log, &working_directory);
    beez_sandbox::set_diagnostic_sink(reporter.diagnostic_sink());

    let status = run(&Runner::new(&working_directory), &mut reporter);
    let finished = reporter.finish(status.as_ref().ok().copied());
    match &status {
        Ok(code) => tracing::debug!(status = code, "run ended"),
        Err(error) => tracing::debug!(%error, "run ended in an error"),
    }
    Ok(settle(status, finished, |error| report_error(error))?)
}

/// The result of a run, once its report has closed.
///
/// The run's own status or error decides how Beez exits. A report that
/// could not close goes to `report` instead, so it cannot hide that a run
/// passed or why it failed.
fn settle<E>(
    status: Result<i32, E>,
    finished: io::Result<()>,
    report: impl FnOnce(&io::Error),
) -> Result<i32, E> {
    if let Err(error) = finished {
        report(&error);
    }
    status
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{settle, write_error};

    fn closed() -> io::Error {
        io::Error::from(io::ErrorKind::BrokenPipe)
    }

    #[test]
    fn keeps_the_run_status_when_the_report_cannot_close() {
        for status in [0, 1, 23] {
            let mut reported = None;
            let settled = settle(Ok::<_, io::Error>(status), Err(closed()), |error| {
                reported = Some(error.kind());
            });

            assert_eq!(settled.ok(), Some(status));
            assert_eq!(reported, Some(io::ErrorKind::BrokenPipe));
        }
    }

    #[test]
    fn keeps_the_run_error_when_the_report_cannot_close() {
        let settled = settle(
            Err(io::Error::new(io::ErrorKind::NotFound, "no program")),
            Err(closed()),
            |_| {},
        );

        let error = settled.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(error.to_string(), "no program");
    }

    #[test]
    fn writes_each_cause_of_an_error_on_its_own_line() {
        let error = anyhow::Error::new(io::Error::other("unknown field `comand`"))
            .context("cannot load workflow nightly.yaml");
        let mut out = Vec::new();

        write_error(&mut out, error.as_ref()).unwrap();

        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("error:"), "{out}");
        assert!(
            out.ends_with(
                " cannot load workflow nightly.yaml\n  caused by: unknown field `comand`\n"
            ),
            "{out}"
        );
    }

    #[test]
    fn returns_the_run_status_when_the_report_closes() {
        let mut reported = false;
        let settled = settle(Ok::<_, io::Error>(4), Ok(()), |_| reported = true);

        assert_eq!(settled.ok(), Some(4));
        assert!(!reported);
    }
}
