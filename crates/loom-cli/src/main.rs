//! Loom's user-facing command-line interface.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use loom_manifest::load;
use loom_process::{ExecutionRequest, HarnessCall, HeadlessHarness, ProcessCall};
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
    Workflow(WorkflowFile),
    Pi(Prompt),
    Omp(Prompt),
    Command(Process),
}

#[derive(Debug, Args)]
struct WorkflowFile {
    path: PathBuf,
}

#[derive(Debug, Args)]
struct Prompt {
    prompt: OsString,
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
    match cli.command {
        Command::Run(Run {
            command: RunCommand::Workflow(WorkflowFile { path }),
        }) => {
            let workflow = load(&path)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            Runner.run_workflow(&workflow, &mut render)
        }
        Command::Run(Run {
            command: RunCommand::Pi(Prompt { prompt }),
        }) => Runner.run_request(
            harness_request(HeadlessHarness::Pi, "pi", prompt),
            &mut render,
        ),
        Command::Run(Run {
            command: RunCommand::Omp(Prompt { prompt }),
        }) => Runner.run_request(
            harness_request(HeadlessHarness::Omp, "omp", prompt),
            &mut render,
        ),
        Command::Run(Run {
            command: RunCommand::Command(Process { program, arguments }),
        }) => Runner.run_request(
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

fn harness_request(
    harness: HeadlessHarness,
    program: &'static str,
    prompt: OsString,
) -> ExecutionRequest {
    ExecutionRequest::Harness(HarnessCall::new(harness, program, prompt))
}
