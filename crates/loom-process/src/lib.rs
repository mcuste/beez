use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    program: PathBuf,
    arguments: Vec<OsString>,
    current_dir: Option<PathBuf>,
}

impl CommandSpec {
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

    pub fn argument(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    pub fn current_dir(mut self, current_dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(current_dir.into());
        self
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    pub fn working_directory(&self) -> Option<&Path> {
        self.current_dir.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandSpecError {
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

#[derive(Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status_code: Option<i32>,
}

impl ProcessOutput {
    pub fn succeeded(&self) -> bool {
        self.status_code == Some(0)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HostProcess;

impl HostProcess {
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
        assert_eq!(output.stdout, b"loom");
        assert!(output.stderr.is_empty());
    }
}
