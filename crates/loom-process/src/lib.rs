//! Direct host-process execution without implicit shell invocation.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A direct child-process invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    program: PathBuf,
    arguments: Vec<OsString>,
    current_dir: Option<PathBuf>,
}

impl CommandSpec {
    /// Creates a process specification for a non-empty executable path.
    pub fn new(program: impl Into<PathBuf>) -> Result<Self, CommandSpecError> {
        let program = program.into();
        if program.as_os_str().is_empty() {
            return Err(CommandSpecError::EmptyProgram);
        }

        Ok(Self {
            program,
            arguments: Vec::new(),
            current_dir: None,
        })
    }

    /// Adds one literal argument to the direct process invocation.
    #[must_use]
    pub fn argument(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    /// Sets the child process working directory.
    #[must_use]
    pub fn current_dir(mut self, current_dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(current_dir.into());
        self
    }

    /// Returns the child process executable path.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Returns the child process argument vector.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    /// Returns the configured child process working directory.
    #[must_use]
    pub fn working_directory(&self) -> Option<&Path> {
        self.current_dir.as_deref()
    }
}

/// Reports an invalid child-process specification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandSpecError {
    /// The process executable path is empty.
    EmptyProgram,
}

impl fmt::Display for CommandSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyProgram => formatter.write_str("process program must not be empty"),
        }
    }
}

impl std::error::Error for CommandSpecError {}

/// The captured result of a child process.
#[derive(Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status_code: Option<i32>,
}

impl ProcessOutput {
    /// Returns the captured standard output bytes.
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Returns the captured standard error bytes.
    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Returns the process exit status, or `None` when terminated by a signal.
    #[must_use]
    pub fn status_code(&self) -> Option<i32> {
        self.status_code
    }

    /// Reports whether the process exited successfully.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.status_code == Some(0)
    }
}

/// Executes direct process specifications on the host.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostProcess;

impl HostProcess {
    /// Runs a process and captures its standard streams and exit status.
    pub fn run(&self, spec: &CommandSpec) -> Result<ProcessOutput, std::io::Error> {
        let mut command = Command::new(&spec.program);
        command.args(&spec.arguments);

        if let Some(current_dir) = &spec.current_dir {
            command.current_dir(current_dir);
        }

        let output = command.output()?;
        Ok(ProcessOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            status_code: output.status.code(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandSpec, CommandSpecError, HostProcess};

    #[test]
    fn rejects_empty_programs() {
        assert_eq!(CommandSpec::new(""), Err(CommandSpecError::EmptyProgram));
    }

    #[test]
    fn runs_a_direct_process() {
        let spec = CommandSpec::new("sh")
            .unwrap()
            .argument("-c")
            .argument("printf loom");

        let output = HostProcess.run(&spec).unwrap();

        assert!(output.succeeded());
        assert_eq!(output.stdout(), b"loom");
        assert!(output.stderr().is_empty());
    }
}
