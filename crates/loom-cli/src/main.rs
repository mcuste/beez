//! Loom's user-facing command-line interface.

use std::ffi::OsString;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use loom_core::{
    DomainRule, FilesystemPolicy, HeadlessHarness, NetworkPolicy, SandboxPath, SandboxPolicy,
};
use loom_manifest::load;
use loom_process::{ExecutionRequest, HarnessCall, ProcessCall};
use loom_runner::{RunEvent, Runner};

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
}

#[derive(Debug, Args)]
struct Process {
    #[command(flatten)]
    sandbox: SandboxArgs,
    program: OsString,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    arguments: Vec<OsString>,
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
    let mut render = render;
    let command = match cli.command {
        Command::Run(Run { command }) => command,
        Command::SandboxInit(init) => return sandbox_init(&init),
        Command::SandboxRelay(relay) => return sandbox_relay(&relay),
    };
    match command {
        RunCommand::Workflow(WorkflowFile { path }) => {
            let workflow = load(&path)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            Runner.run_workflow(&workflow, &mut render)
        }
        RunCommand::Pi(run) => run_harness(HeadlessHarness::Pi, run, &mut render),
        RunCommand::Omp(run) => run_harness(HeadlessHarness::Omp, run, &mut render),
        RunCommand::Claude(run) => run_harness(HeadlessHarness::Claude, run, &mut render),
        RunCommand::Codex(run) => run_harness(HeadlessHarness::Codex, run, &mut render),
        RunCommand::Command(Process {
            sandbox,
            program,
            arguments,
        }) => {
            let policy = sandbox.policy()?;
            Runner.run_request_in(
                ExecutionRequest::Command(
                    arguments
                        .into_iter()
                        .fold(ProcessCall::new(program), |call, argument| {
                            call.argument(argument)
                        }),
                ),
                policy.as_ref(),
                &mut render,
            )
        }
    }
}

fn run_harness(
    harness: HeadlessHarness,
    run: HarnessRun,
    render: &mut impl FnMut(&RunEvent) -> io::Result<()>,
) -> io::Result<i32> {
    let policy = run.sandbox.policy()?;
    Runner.run_request_in(harness_request(harness, run), policy.as_ref(), render)
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

fn render(event: &RunEvent) -> io::Result<()> {
    if let RunEvent::Finished { output, .. } = event {
        io::stdout().write_all(output.stdout())?;
        io::stderr().write_all(output.stderr())?;
    }
    Ok(())
}

fn harness_request(harness: HeadlessHarness, run: HarnessRun) -> ExecutionRequest {
    let mut call = HarnessCall::new(harness, run.prompt);
    if let Some(model) = run.model {
        call = call.model(model);
    }
    if let Some(effort) = run.effort {
        call = call.effort(effort);
    }

    ExecutionRequest::Harness(call)
}
