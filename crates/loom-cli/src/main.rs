//! Loom's user-facing command-line interface.

use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use loom_core::{
    DomainRule, FilesystemPolicy, HeadlessHarness, NetworkPolicy, SandboxPath, SandboxPolicy,
};
use loom_manifest::load;
use loom_process::{ExecutionRequest, HarnessCall, ProcessCall};
use loom_runner::Runner;

mod log;
mod report;
mod time;

use log::{LogSettings, RunTarget};
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
    /// Run inside an operating-system sandbox with Loom's default policy.
    #[arg(long)]
    sandbox: bool,
    /// Host the sandbox may also reach, such as github.com or *.npmjs.org:443. Implies --sandbox.
    #[arg(long, value_name = "HOST")]
    allow_domain: Vec<String>,
    /// Path the sandbox may also write. Implies --sandbox.
    #[arg(long, value_name = "PATH")]
    allow_write: Vec<String>,
}

impl SandboxArgs {
    fn policy(&self) -> io::Result<Option<SandboxPolicy>> {
        if !self.sandbox && self.allow_domain.is_empty() && self.allow_write.is_empty() {
            return Ok(None);
        }
        let allow = parse_all::<DomainRule>(&self.allow_domain)?;
        let write_allow = parse_all::<SandboxPath>(&self.allow_write)?;
        let network = NetworkPolicy::new(true, Vec::new(), Vec::new(), allow, false);
        let filesystem = FilesystemPolicy::new(true, Vec::new(), write_allow, Vec::new());
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
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let target = RunTarget::Workflow {
        path: &path,
        workflow: &workflow,
    };
    let mut reporter = reporter(&target, output, color, timestamps, log.settings());

    let status = Runner.run_workflow(&workflow, &mut |event| reporter.event(event));
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
    let target = RunTarget::Request {
        name: harness.name(),
    };
    let mut reporter = reporter(
        &target,
        OutputMode::Stream,
        ColorMode::Auto,
        None,
        log.settings(),
    );

    let status = Runner.run_request_in(
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
    let target = RunTarget::Request { name: "command" };
    let mut reporter = reporter(
        &target,
        OutputMode::Stream,
        ColorMode::Auto,
        None,
        log.settings(),
    );
    let request = ExecutionRequest::Command(
        arguments
            .into_iter()
            .fold(ProcessCall::new(program), ProcessCall::argument),
    );

    let status =
        Runner.run_request_in(request, policy.as_ref(), &mut |event| reporter.event(event));
    finish(&mut reporter, status)
}

/// Builds a reporter and routes sandbox diagnostics through it.
fn reporter(
    target: &RunTarget<'_>,
    output: OutputMode,
    color: ColorMode,
    timestamps: Option<Timestamps>,
    log: LogSettings<'_>,
) -> Reporter {
    let reporter = Reporter::new(target, output, color, timestamps, log);
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
