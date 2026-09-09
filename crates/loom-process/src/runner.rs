use std::process::{Command, Output};

use loom_core::SandboxPolicy;
use loom_sandbox::SandboxedCommand;

use crate::execution::ExecutionRequest;

/// Captured child-process streams and exit status.
#[derive(Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status_code: Option<i32>,
}

impl ProcessOutput {
    /// Captured standard output.
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Captured standard error.
    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Returns `None` after signal termination.
    #[must_use]
    pub fn status_code(&self) -> Option<i32> {
        self.status_code
    }

    /// True only when the process exits with status zero.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.status_code == Some(0)
    }
}

/// Runs Loom process requests.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessRunner;

impl ProcessRunner {
    /// Starts a request and captures its output.
    pub fn run(&self, request: ExecutionRequest) -> Result<ProcessOutput, std::io::Error> {
        let (program, arguments) = request.into_parts();
        let output = Command::new(program).args(arguments).output()?;

        Ok(ProcessOutput::from(output))
    }

    /// Starts a request inside an operating-system sandbox and captures its output.
    ///
    /// The sandbox uses the current directory as the task's working directory.
    pub fn run_sandboxed(
        &self,
        request: ExecutionRequest,
        policy: &SandboxPolicy,
    ) -> Result<ProcessOutput, std::io::Error> {
        let harness = request.harness();
        let (program, arguments) = request.into_parts();
        let working_directory = std::env::current_dir()?;
        let command =
            SandboxedCommand::new(policy, harness, &program, &arguments, &working_directory)?;

        Ok(ProcessOutput::from(command.output()?))
    }
}

impl From<Output> for ProcessOutput {
    fn from(output: Output) -> Self {
        Self {
            stdout: output.stdout,
            stderr: output.stderr,
            status_code: output.status.code(),
        }
    }
}
