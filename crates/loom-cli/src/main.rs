//! Loom's user-facing command-line interface.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use loom_core::HeadlessHarness;
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
}

#[derive(Debug, Args)]
struct Process {
    program: OsString,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    arguments: Vec<OsString>,
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
    let Command::Run(Run { command }) = cli.command;
    match command {
        RunCommand::Workflow(WorkflowFile { path }) => {
            let workflow = load(&path)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            Runner.run_workflow(&workflow, &mut render)
        }
        RunCommand::Pi(run) => {
            Runner.run_request(harness_request(HeadlessHarness::Pi, run), &mut render)
        }
        RunCommand::Omp(run) => {
            Runner.run_request(harness_request(HeadlessHarness::Omp, run), &mut render)
        }
        RunCommand::Claude(run) => {
            Runner.run_request(harness_request(HeadlessHarness::Claude, run), &mut render)
        }
        RunCommand::Codex(run) => {
            Runner.run_request(harness_request(HeadlessHarness::Codex, run), &mut render)
        }
        RunCommand::Command(Process { program, arguments }) => Runner.run_request(
            ExecutionRequest::Command(
                arguments
                    .into_iter()
                    .fold(ProcessCall::new(program), |call, argument| {
                        call.argument(argument)
                    }),
            ),
            &mut render,
        ),
    }
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
