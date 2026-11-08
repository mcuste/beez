//! Beez's user-facing command-line interface.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;

use beez_manifest::load;
use beez_policy::{
    DomainRule, FilesystemPolicy, HarnessOptions, HeadlessHarness, NetworkPolicy, SandboxPath,
    SandboxPolicy, parse_all,
};
use beez_process::{ExecutionRequest, HarnessCall, ProcessCall};
use beez_record::{LogSettings, RunTarget};
use beez_runner::Runner;
use clap::{Args, Parser, Subcommand};

mod report;
mod sandbox;
mod schedule;

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
    allow_domain: Vec<String>,
    /// Path the sandbox may also write.
    #[arg(long, value_name = "PATH")]
    allow_write: Vec<String>,
}

impl SandboxArgs {
    /// Beez's default policy, plus the hosts and paths the command allows.
    fn policy(&self) -> io::Result<Option<SandboxPolicy>> {
        if self.no_sandbox {
            return Ok(None);
        }
        let allow = parse_values::<DomainRule>(&self.allow_domain)?;
        let write_allow = parse_values::<SandboxPath>(&self.allow_write)?;
        let network = NetworkPolicy::default().with_allow(allow);
        let filesystem = FilesystemPolicy::default().with_write_allow(write_allow);
        Ok(Some(SandboxPolicy::new(network, filesystem, None)))
    }
}

/// Reads policy values a flag holds, and reports a wrong one as bad input.
fn parse_values<T>(values: &[String]) -> io::Result<Vec<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    parse_all(values).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn main() {
    let status = match run(Cli::parse()) {
        Ok(status) => status,
        Err(error) => {
            // Standard error may be closed. The status still says Beez failed.
            let _ = writeln!(io::stderr(), "{error}");
            2
        }
    };
    std::process::exit(status);
}

fn run(cli: Cli) -> io::Result<i32> {
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

fn run_workflow(file: WorkflowFile) -> io::Result<i32> {
    let WorkflowFile { path, render, log } = file;
    let workflow = load(&path)
        .map(beez_manifest::Manifest::into_workflow)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let target = RunTarget::Workflow {
        path: &path,
        workflow: &workflow,
    };

    run_reported(&target, render, log.settings(), |runner, reporter| {
        runner.run_workflow(&workflow, &mut |event| reporter.event(event))
    })
}

fn run_harness(harness: HeadlessHarness, run: HarnessRun) -> io::Result<i32> {
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

fn run_command(process: Process) -> io::Result<i32> {
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
) -> io::Result<i32> {
    let policy = sandbox.policy()?;
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
    run: impl FnOnce(&Runner, &mut Reporter) -> io::Result<i32>,
) -> io::Result<i32> {
    let working_directory = std::env::current_dir()?;
    let mut reporter = Reporter::new(target, render, log, &working_directory);
    beez_sandbox::set_diagnostic_sink(reporter.diagnostic_sink());

    let status = run(&Runner::new(&working_directory), &mut reporter);
    let finished = reporter.finish(status.as_ref().ok().copied());
    settle(status, finished, |error| {
        // Standard error may be what failed, so this is best effort.
        let _ = writeln!(io::stderr(), "{error}");
    })
}

/// The result of a run, once its report has closed.
///
/// The run's own status or error decides how Beez exits. A report that
/// could not close goes to `report` instead, so it cannot hide that a run
/// passed or why it failed.
fn settle(
    status: io::Result<i32>,
    finished: io::Result<()>,
    report: impl FnOnce(&io::Error),
) -> io::Result<i32> {
    if let Err(error) = finished {
        report(&error);
    }
    status
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::settle;

    fn closed() -> io::Error {
        io::Error::from(io::ErrorKind::BrokenPipe)
    }

    #[test]
    fn keeps_the_run_status_when_the_report_cannot_close() {
        for status in [0, 1, 23] {
            let mut reported = None;
            let settled = settle(Ok(status), Err(closed()), |error| {
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
    fn returns_the_run_status_when_the_report_closes() {
        let mut reported = false;
        let settled = settle(Ok(4), Ok(()), |_| reported = true);

        assert_eq!(settled.ok(), Some(4));
        assert!(!reported);
    }
}
