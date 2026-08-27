//! Runs supported coding harnesses and direct process calls.

use std::ffi::OsString;
use std::io::{self, Write};

use clap::{Args, Parser, Subcommand};
use loom_process::{ExecutionRequest, HarnessCall, HeadlessHarness, ProcessCall, ProcessRunner};

#[derive(Debug, Parser)]
#[command(name = "loom", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a supported coding harness or direct process.
    Run(Run),
}

#[derive(Debug, Args)]
struct Run {
    #[command(subcommand)]
    command: RunCommand,
}

#[derive(Debug, Subcommand)]
enum RunCommand {
    /// Run Pi in headless mode.
    Pi(Prompt),
    /// Run Oh My Pi in headless mode.
    Omp(Prompt),
    /// Run a program without invoking a shell.
    Command(Process),
}

#[derive(Debug, Args)]
struct Prompt {
    prompt: OsString,
}

#[derive(Debug, Args)]
struct Process {
    program: OsString,
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        num_args = 1..
    )]
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
    let request = match cli.command {
        Command::Run(run) => run.command.into_request(),
    };
    let result = ProcessRunner.run(request)?;

    io::stdout().write_all(result.stdout())?;
    io::stderr().write_all(result.stderr())?;

    Ok(result.status_code().unwrap_or(1))
}

impl RunCommand {
    fn into_request(self) -> ExecutionRequest {
        match self {
            Self::Pi(Prompt { prompt }) => harness_request(HeadlessHarness::Pi, "pi", prompt),
            Self::Omp(Prompt { prompt }) => harness_request(HeadlessHarness::Omp, "omp", prompt),
            Self::Command(Process { program, arguments }) => ExecutionRequest::Command(
                arguments
                    .into_iter()
                    .fold(ProcessCall::new(program), |call, argument| {
                        call.argument(argument)
                    }),
            ),
        }
    }
}

fn harness_request(
    harness: HeadlessHarness,
    program: &'static str,
    prompt: OsString,
) -> ExecutionRequest {
    ExecutionRequest::Harness(HarnessCall::new(harness, program, prompt))
}
