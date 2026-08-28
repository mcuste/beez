use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

/// Sends one prompt through a harness's non-interactive interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessCall {
    harness: HeadlessHarness,
    program: PathBuf,
    prompt: OsString,
    model: Option<OsString>,
    effort: Option<OsString>,
}

impl HarnessCall {
    /// Builds a harness request. The harness uses its default model and effort.
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
            model: None,
            effort: None,
        }
    }

    /// Selects the model the harness must use.
    #[must_use]
    pub fn model(mut self, model: impl Into<OsString>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Selects the reasoning effort the harness must use.
    #[must_use]
    pub fn effort(mut self, effort: impl Into<OsString>) -> Self {
        self.effort = Some(effort.into());
        self
    }

    pub(crate) fn into_parts(self) -> (PathBuf, Vec<OsString>) {
        let mut arguments = vec![OsString::from("--print")];
        if let Some(model) = self.model {
            arguments.push("--model".into());
            arguments.push(model);
        }
        if let Some(effort) = self.effort {
            arguments.push(self.harness.effort_flag().into());
            arguments.push(effort);
        }
        // The prompt stays last because harnesses read it as a positional argument.
        arguments.push(self.prompt);

        (self.program, arguments)
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

impl HeadlessHarness {
    /// Each harness names the reasoning effort differently.
    fn effort_flag(self) -> &'static str {
        match self {
            Self::Pi => "--effort",
            Self::Omp => "--thinking",
        }
    }
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
    use std::path::PathBuf;

    use super::{HarnessCall, HeadlessHarness, HeadlessHarnessError};

    #[test]
    fn omits_the_model_and_effort_when_unset() {
        let sut = HarnessCall::new(HeadlessHarness::Pi, "pi", "inspect the repository");

        let (program, arguments) = sut.into_parts();

        assert_eq!(program, PathBuf::from("pi"));
        assert_eq!(arguments, ["--print", "inspect the repository"]);
    }

    #[test]
    fn places_the_model_before_the_prompt() {
        let sut =
            HarnessCall::new(HeadlessHarness::Pi, "pi", "inspect the repository").model("opus");

        let (_, arguments) = sut.into_parts();

        assert_eq!(
            arguments,
            ["--print", "--model", "opus", "inspect the repository"]
        );
    }

    #[test]
    fn places_the_effort_before_the_prompt() {
        let sut =
            HarnessCall::new(HeadlessHarness::Pi, "pi", "inspect the repository").effort("high");

        let (_, arguments) = sut.into_parts();

        assert_eq!(
            arguments,
            ["--print", "--effort", "high", "inspect the repository"]
        );
    }

    #[test]
    fn sends_the_effort_to_omp_as_a_thinking_level() {
        let sut =
            HarnessCall::new(HeadlessHarness::Omp, "omp", "inspect the repository").effort("high");

        let (_, arguments) = sut.into_parts();

        assert_eq!(
            arguments,
            ["--print", "--thinking", "high", "inspect the repository"]
        );
    }

    #[test]
    fn places_the_model_and_effort_before_the_prompt() {
        let sut = HarnessCall::new(HeadlessHarness::Omp, "omp", "inspect the repository")
            .model("opus")
            .effort("high");

        let (_, arguments) = sut.into_parts();

        assert_eq!(
            arguments,
            [
                "--print",
                "--model",
                "opus",
                "--thinking",
                "high",
                "inspect the repository"
            ]
        );
    }

    #[test]
    fn rejects_an_unsupported_harness() {
        let error = "codex".parse::<HeadlessHarness>().unwrap_err();

        assert_eq!(error, HeadlessHarnessError::Unknown("codex".into()));
    }
}
