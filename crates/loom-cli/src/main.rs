//! Loom's user-facing command-line interface.

use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use loom_daemon::{DaemonPaths, Request, Response};
use loom_manifest::load;
use loom_policy::{
    DomainRule, FilesystemPolicy, HarnessOptions, HeadlessHarness, NetworkPolicy, SandboxPath,
    SandboxPolicy, parse_all,
};
use loom_process::{ExecutionRequest, HarnessCall, ProcessCall};
use loom_record::{LogSettings, RunTarget};
use loom_runner::Runner;

mod report;
mod schedule;

use report::{RenderOptions, Reporter};

#[derive(Debug, Parser)]
#[command(name = "loom", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a supported coding harness, workflow, or direct process.
    Run(Run),
    /// Run workflows on the schedules their manifests declare.
    Daemon(DaemonArgs),
    /// Manage the manifests the daemon watches.
    Schedule(ScheduleArgs),
    /// Internal: first process inside a Linux sandbox.
    #[command(hide = true)]
    SandboxInit(SandboxInit),
    /// Internal: forwards loopback connections to a Unix socket inside a Linux sandbox.
    #[command(hide = true)]
    SandboxRelay(SandboxRelay),
}

#[derive(Debug, Args)]
struct Run {
    #[command(subcommand)]
    command: RunCommand,
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
struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
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
struct DaemonRun {
    #[command(flatten)]
    root: RootArgs,
    /// How many runs the daemon starts at the same time.
    #[arg(long, value_name = "COUNT", default_value_t = loom_daemon::DEFAULT_RUN_LIMIT)]
    limit: usize,
    /// How many runs to keep in the Loom root. Zero keeps every run.
    #[arg(long, value_name = "COUNT", default_value_t = loom_daemon::DEFAULT_KEPT_RUNS)]
    keep_runs: usize,
}

#[derive(Debug, Args)]
struct ScheduleArgs {
    #[command(subcommand)]
    command: ScheduleCommand,
}

#[derive(Debug, Subcommand)]
enum ScheduleCommand {
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
struct ManifestArgs {
    /// Path of the workflow manifest.
    manifest: PathBuf,
    #[command(flatten)]
    root: RootArgs,
}

#[derive(Debug, Args)]
struct JobArgs {
    /// Job ID, as `loom schedule list` prints it.
    job: String,
    #[command(flatten)]
    root: RootArgs,
}

#[derive(Debug, Args)]
struct RootArgs {
    /// Directory that holds the daemon's own files and the runs it starts.
    ///
    /// Loom uses `~/.loom` by default, so one daemon serves every repository.
    #[arg(long, value_name = "PATH", env = "LOOM_ROOT")]
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
    /// Loom writes to `.loom` in the repository root by default, or in the
    /// working directory outside a repository.
    #[arg(long, value_name = "PATH", env = "LOOM_LOG_DIR")]
    log_dir: Option<PathBuf>,
    /// Write no artifacts for this run.
    #[arg(long)]
    no_log: bool,
}

impl LogArgs {
    fn settings(&self) -> LogSettings<'_> {
        LogSettings {
            enabled: !self.no_log,
            directory: self.log_dir.as_deref(),
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
    /// Loom's default policy, plus the hosts and paths the command allows.
    fn policy(&self) -> io::Result<Option<SandboxPolicy>> {
        if self.no_sandbox {
            return Ok(None);
        }
        let allow = parse_values::<DomainRule>(&self.allow_domain)?;
        let write_allow = parse_values::<SandboxPath>(&self.allow_write)?;
        let network = NetworkPolicy::new(None, Vec::new(), Vec::new(), allow, None);
        let filesystem = FilesystemPolicy::new(None, Vec::new(), write_allow, Vec::new());
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

#[derive(Debug, Args)]
struct SandboxInit {
    /// Loopback port and Unix socket pair to bridge, as PORT=SOCKET.
    #[arg(long = "relay", value_name = "PORT=SOCKET")]
    relays: Vec<OsString>,
    /// File or directory that may execute. Absent means unrestricted.
    #[arg(long = "exec", value_name = "PATH")]
    executables: Vec<PathBuf>,
    /// Program and arguments to replace this process with.
    #[arg(last = true, required = true)]
    command: Vec<OsString>,
}

#[derive(Debug, Args)]
struct SandboxRelay {
    /// Loopback address to listen on.
    #[arg(long)]
    listen: SocketAddr,
    /// Unix socket that receives each connection.
    #[arg(long)]
    socket: PathBuf,
}

fn main() {
    let status = match run(Cli::parse()) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("{error}");
            2
        }
    };
    std::process::exit(status);
}

fn run(cli: Cli) -> io::Result<i32> {
    let command = match cli.command {
        Command::Run(Run { command }) => command,
        Command::Daemon(DaemonArgs { command }) => return daemon(command),
        Command::Schedule(ScheduleArgs { command }) => return schedule(command),
        Command::SandboxInit(init) => return sandbox_init(&init),
        Command::SandboxRelay(relay) => return sandbox_relay(&relay),
    };
    match command {
        RunCommand::Workflow(file) => run_workflow(file),
        RunCommand::Pi(run) => run_harness(HeadlessHarness::Pi, run),
        RunCommand::Omp(run) => run_harness(HeadlessHarness::Omp, run),
        RunCommand::Claude(run) => run_harness(HeadlessHarness::Claude, run),
        RunCommand::Codex(run) => run_harness(HeadlessHarness::Codex, run),
        RunCommand::Command(process) => run_command(process),
    }
}

fn run_workflow(file: WorkflowFile) -> io::Result<i32> {
    let WorkflowFile { path, render, log } = file;
    let workflow = load(&path)
        .map(loom_manifest::Manifest::into_workflow)
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
        HarnessCall::new(harness, prompt).options(&HarnessOptions::new(model, effort)),
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
    loom_sandbox::set_diagnostic_sink(reporter.diagnostic_sink());

    let status = run(&Runner::new(&working_directory), &mut reporter);
    reporter.finish(status.as_ref().ok().copied())?;
    status
}

fn daemon(command: DaemonCommand) -> io::Result<i32> {
    match command {
        DaemonCommand::Run(run) => {
            loom_daemon::run(&run.root.paths()?, run.limit, run.keep_runs)?;
            Ok(0)
        }
        DaemonCommand::Start(run) => Ok(answer(loom_daemon::start(
            &run.root.paths()?,
            run.limit,
            run.keep_runs,
        )?)),
        DaemonCommand::Stop(root) => send(&root, Request::Stop),
        DaemonCommand::Status(root) => report_jobs(&root),
        DaemonCommand::Reload(root) => send(&root, Request::Reload),
    }
}

fn schedule(command: ScheduleCommand) -> io::Result<i32> {
    match command {
        ScheduleCommand::Add(args) => Ok(answer(loom_daemon::add(
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
        ScheduleCommand::Trigger(args) => send(&args.root, Request::Trigger { job: args.job }),
        ScheduleCommand::Pause(args) => send(&args.root, Request::Pause { job: args.job }),
        ScheduleCommand::Resume(args) => send(&args.root, Request::Resume { job: args.job }),
    }
}

/// Gives one command, whether the daemon runs or not, and prints its answer.
fn send(root: &RootArgs, request: Request) -> io::Result<i32> {
    Ok(answer(loom_daemon::command(&root.paths()?, request)?))
}

/// Prints the daemon and every job it holds.
fn report_jobs(root: &RootArgs) -> io::Result<i32> {
    schedule::status(&loom_daemon::status(&root.paths()?)?)?;

    Ok(0)
}

/// Prints what a command answered, and returns the status for it.
fn answer(response: Response) -> i32 {
    match response {
        Response::Done { message } => {
            println!("{message}");
            0
        }
        Response::Error { message } => {
            eprintln!("{message}");
            1
        }
        Response::Status { .. } => {
            eprintln!("unexpected answer from the daemon");
            2
        }
    }
}

#[cfg(target_os = "linux")]
fn sandbox_init(init: &SandboxInit) -> io::Result<i32> {
    let relays = init
        .relays
        .iter()
        .map(|relay| loom_sandbox::parse_relay(relay))
        .collect::<io::Result<Vec<_>>>()?;
    let executables = (!init.executables.is_empty()).then_some(init.executables.as_slice());
    let Some((program, arguments)) = init.command.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sandbox-init needs a program",
        ));
    };
    loom_sandbox::init(&relays, executables, program.as_ref(), arguments)?;
    Ok(0)
}

#[cfg(not(target_os = "linux"))]
fn sandbox_init(_init: &SandboxInit) -> io::Result<i32> {
    Err(unsupported_sandbox_helper())
}

#[cfg(target_os = "linux")]
fn sandbox_relay(relay: &SandboxRelay) -> io::Result<i32> {
    loom_sandbox::relay(relay.listen, &relay.socket)?;
    Ok(0)
}

#[cfg(not(target_os = "linux"))]
fn sandbox_relay(_relay: &SandboxRelay) -> io::Result<i32> {
    Err(unsupported_sandbox_helper())
}

#[cfg(not(target_os = "linux"))]
fn unsupported_sandbox_helper() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "sandbox helper commands run only inside a Linux sandbox",
    )
}
