use std::ffi::OsString;
use std::path::PathBuf;

/// A direct host-process invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessCall {
    program: PathBuf,
    arguments: Vec<OsString>,
}

impl ProcessCall {
    /// Builds a process request.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
        }
    }

    /// Adds a literal process argument.
    #[must_use]
    pub fn argument(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    pub(crate) fn into_parts(self) -> (PathBuf, Vec<OsString>) {
        (self.program, self.arguments)
    }
}
