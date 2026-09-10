//! Loom's user-facing command-line interface.

use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use loom_core::{
    DomainRule, FilesystemPolicy, HeadlessHarness, NetworkPolicy, SandboxPath, SandboxPolicy,
};
use loom_daemon::{DaemonPaths, Request, Response};
use loom_manifest::load;
use loom_process::{ExecutionRequest, HarnessCall, ProcessCall};
use loom_record::{LogSettings, RunTarget};
use loom_runner::Runner;

mod report;
mod schedule;

use report::{ColorMode, OutputMode, Reporter, Timestamps};

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
    /// How to render the output of the tasks.
    #[arg(long, value_enum, default_value_t = OutputMode::Stream)]
    output: OutputMode,
    /// When to colour Loom's own output.
    #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
    color: ColorMode,
    /// Prefix every line Loom writes with a timestamp.
    ///
    /// A value needs an equals sign, so the bare flag cannot take the path of
    /// the manifest as its value.
    #[arg(
        long,
        value_enum,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "date-time"
    )]
    timestamps: Option<Timestamps>,
    #[command(flatten)]
    log: LogArgs,
}

#[derive(Debug, Args)]
struct HarnessRun {
    prompt: OsString,
    /// Model the harness must use.
    #[arg(long)]
    model: Option<OsString>,
    /// Reasoning effort the harness must use.
    #[arg(long)]
    effort: Option<OsString>,
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
        let allow = parse_all::<DomainRule>(&self.allow_domain)?;
        let write_allow = parse_all::<SandboxPath>(&self.allow_write)?;
        let network = NetworkPolicy::new(None, Vec::new(), Vec::new(), allow, None);
        let filesystem = FilesystemPolicy::new(None, Vec::new(), write_allow, Vec::new());
        Ok(Some(SandboxPolicy::new(network, filesystem, None)))
    }
}

fn parse_all<T>(values: &[String]) -> io::Result<Vec<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    values
        .iter()
        .map(|value| {
            value
                .parse::<T>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))
        })
        .collect()
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

/// How long to wait for a started daemon to answer.
const START_WAIT: std::time::Duration = std::time::Duration::from_millis(100);
/// How many times to look for the answer, so a slow start still reports.
const START_ATTEMPTS: usize = 50;

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
    let WorkflowFile {
        path,
        output,
        color,
        timestamps,
        log,
    } = file;
    let workflow = load(&path)
        .map(loom_manifest::Manifest::into_workflow)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let working_directory = std::env::current_dir()?;
    let target = RunTarget::Workflow {
        path: &path,
        workflow: &workflow,
    };
    let mut reporter = reporter(
        &target,
        output,
        color,
        timestamps,
        log.settings(),
        &working_directory,
    );

    let status =
        Runner::new(&working_directory).run_workflow(&workflow, &mut |event| reporter.event(event));
    finish(&mut reporter, status)
}

fn run_harness(harness: HeadlessHarness, run: HarnessRun) -> io::Result<i32> {
    let HarnessRun {
        prompt,
        model,
        effort,
        sandbox,
        log,
    } = run;
    let policy = sandbox.policy()?;
    let working_directory = std::env::current_dir()?;
    let target = RunTarget::Request {
        name: harness.name(),
    };
    let mut reporter = reporter(
        &target,
        OutputMode::Stream,
        ColorMode::Auto,
        None,
        log.settings(),
        &working_directory,
    );

    let status = Runner::new(&working_directory).run_request_in(
        harness_request(harness, prompt, model, effort),
        policy.as_ref(),
        &mut |event| reporter.event(event),
    );
    finish(&mut reporter, status)
}

fn run_command(process: Process) -> io::Result<i32> {
    let Process {
        sandbox,
        log,
        program,
        arguments,
    } = process;
    let policy = sandbox.policy()?;
    let working_directory = std::env::current_dir()?;
    let target = RunTarget::Request { name: "command" };
    let mut reporter = reporter(
        &target,
        OutputMode::Stream,
        ColorMode::Auto,
        None,
        log.settings(),
        &working_directory,
    );
    let request = ExecutionRequest::Command(
        arguments
            .into_iter()
            .fold(ProcessCall::new(program), ProcessCall::argument),
    );

    let status =
        Runner::new(&working_directory)
            .run_request_in(request, policy.as_ref(), &mut |event| reporter.event(event));
    finish(&mut reporter, status)
}

fn daemon(command: DaemonCommand) -> io::Result<i32> {
    match command {
        DaemonCommand::Run(run) => {
            loom_daemon::run(&run.root.paths()?, run.limit, run.keep_runs)?;
            Ok(0)
        }
        DaemonCommand::Start(run) => start_daemon(&run),
        DaemonCommand::Stop(root) => {
            Ok(answer(loom_daemon::command(&root.paths()?, Request::Stop)?))
        }
        DaemonCommand::Status(root) => {
            schedule::status(&loom_daemon::status(&root.paths()?)?)?;
            Ok(0)
        }
        DaemonCommand::Reload(root) => Ok(answer(loom_daemon::command(
            &root.paths()?,
            Request::Reload,
        )?)),
    }
}

fn schedule(command: ScheduleCommand) -> io::Result<i32> {
    match command {
        ScheduleCommand::Add(args) => Ok(answer(loom_daemon::add(
            &args.root.paths()?,
            &args.manifest,
        )?)),
        ScheduleCommand::Remove(args) => Ok(answer(loom_daemon::command(
            &args.root.paths()?,
            Request::Remove {
                manifest: args.manifest,
            },
        )?)),
        ScheduleCommand::List(root) => {
            schedule::status(&loom_daemon::status(&root.paths()?)?)?;
            Ok(0)
        }
        ScheduleCommand::Trigger(args) => Ok(answer(loom_daemon::command(
            &args.root.paths()?,
            Request::Trigger { job: args.job },
        )?)),
        ScheduleCommand::Pause(args) => Ok(answer(loom_daemon::command(
            &args.root.paths()?,
            Request::Pause { job: args.job },
        )?)),
        ScheduleCommand::Resume(args) => Ok(answer(loom_daemon::command(
            &args.root.paths()?,
            Request::Resume { job: args.job },
        )?)),
    }
}

/// Starts the daemon in its own process group, so a closing terminal leaves it
/// running, with its own lines in `daemon.log`.
fn start_daemon(run: &DaemonRun) -> io::Result<i32> {
    use std::os::unix::process::CommandExt;

    let paths = run.root.paths()?;
    if loom_daemon::is_running(paths.socket()) {
        eprintln!("a daemon already runs for {}", paths.root().display());
        return Ok(1);
    }
    paths.create()?;
    let log = std::fs::File::options()
        .create(true)
        .append(true)
        .open(paths.log())?;
    let child = std::process::Command::new(std::env::current_exe()?)
        .args(["daemon", "run", "--root"])
        .arg(paths.root())
        .arg("--limit")
        .arg(run.limit.to_string())
        .arg("--keep-runs")
        .arg(run.keep_runs.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;

    for _ in 0..START_ATTEMPTS {
        if loom_daemon::is_running(paths.socket()) {
            println!(
                "daemon started, pid {}, writing to {}",
                child.id(),
                paths.log().display()
            );
            return Ok(0);
        }
        std::thread::sleep(START_WAIT);
    }
    eprintln!(
        "the daemon did not answer on {}, see {}",
        paths.socket().display(),
        paths.log().display()
    );

    Ok(1)
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

/// Builds a reporter and routes sandbox diagnostics through it.
fn reporter(
    target: &RunTarget<'_>,
    output: OutputMode,
    color: ColorMode,
    timestamps: Option<Timestamps>,
    log: LogSettings<'_>,
    working_directory: &Path,
) -> Reporter {
    let reporter = Reporter::new(target, output, color, timestamps, log, working_directory);
    loom_sandbox::set_diagnostic_sink(reporter.diagnostic_sink());
    reporter
}

/// Closes the run's own report, then returns its status.
fn finish(reporter: &mut Reporter, status: io::Result<i32>) -> io::Result<i32> {
    reporter.finish(status.as_ref().ok().copied())?;
    status
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

fn harness_request(
    harness: HeadlessHarness,
    prompt: OsString,
    model: Option<OsString>,
    effort: Option<OsString>,
) -> ExecutionRequest {
    let mut call = HarnessCall::new(harness, prompt);
    if let Some(model) = model {
        call = call.model(model);
    }
    if let Some(effort) = effort {
        call = call.effort(effort);
    }

    ExecutionRequest::Harness(call)
}
