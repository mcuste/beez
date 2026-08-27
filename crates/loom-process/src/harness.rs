use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

/// Sends one prompt through a harness's non-interactive interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessCall {
    harness: HeadlessHarness,
    program: PathBuf,
    prompt: OsString,
}

impl HarnessCall {
    /// Builds a harness request.
    #[must_use]
    pub fn new(
        harness: HeadlessHarness,
        program: impl Into<PathBuf>,
        prompt: impl Into<OsString>,
    ) -> Self {
        Self {
            harness,
            program: program.into(),
            prompt: prompt.into(),
        }
    }

    pub(crate) fn into_parts(self) -> (PathBuf, Vec<OsString>) {
        (self.program, vec!["--print".into(), self.prompt])
    }
}

/// Harness selector accepted by Loom.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadlessHarness {
    /// Pi coding agent.
    Pi,
    /// Oh My Pi coding agent.
    Omp,
}

impl std::str::FromStr for HeadlessHarness {
    type Err = HeadlessHarnessError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pi" => Ok(Self::Pi),
            "omp" => Ok(Self::Omp),
            _ => Err(HeadlessHarnessError::Unknown(value.into())),
        }
    }
}

/// Reports an unsupported harness selector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeadlessHarnessError {
    /// The selector does not name a supported harness.
    Unknown(String),
}

impl fmt::Display for HeadlessHarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(value) => {
                write!(
                    formatter,
                    "unsupported headless harness {value:?}; expected pi or omp"
                )
            }
        }
    }
}

impl std::error::Error for HeadlessHarnessError {}

#[cfg(test)]
mod tests {
    use super::{HeadlessHarness, HeadlessHarnessError};

    #[test]
    fn rejects_an_unsupported_harness() {
        let error = "codex".parse::<HeadlessHarness>().unwrap_err();

        assert_eq!(error, HeadlessHarnessError::Unknown("codex".into()));
    }
}
