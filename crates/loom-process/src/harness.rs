use std::ffi::OsString;
use std::path::PathBuf;

use loom_policy::{HarnessOptions, HeadlessHarness};

/// Sends one prompt through a harness's non-interactive interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessCall {
    harness: HeadlessHarness,
    prompt: OsString,
    model: Option<OsString>,
    effort: Option<OsString>,
}

impl HarnessCall {
    /// Uses the harness's default program with its default model and effort.
    #[must_use]
    pub fn new(harness: HeadlessHarness, prompt: impl Into<OsString>) -> Self {
        Self {
            harness,
            prompt: prompt.into(),
            model: None,
            effort: None,
        }
    }

    /// Sets the model the harness must use.
    #[must_use]
    pub fn model(mut self, model: impl Into<OsString>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Sets the reasoning effort the harness must use.
    #[must_use]
    pub fn effort(mut self, effort: impl Into<OsString>) -> Self {
        self.effort = Some(effort.into());
        self
    }

    /// Uses the model and the effort the options name.
    ///
    /// An option that names neither keeps the harness default.
    #[must_use]
    pub fn options(mut self, options: &HarnessOptions) -> Self {
        if let Some(model) = options.model() {
            self.model = Some(model.into());
        }
        if let Some(effort) = options.effort() {
            self.effort = Some(effort.into());
        }
        self
    }

    /// The harness that receives the prompt.
    #[must_use]
    pub fn harness(&self) -> HeadlessHarness {
        self.harness
    }

    pub(crate) fn into_parts(self) -> (PathBuf, Vec<OsString>) {
        let mut arguments = Vec::with_capacity(7);
        arguments.push(headless_argument(self.harness).into());
        if let Some(model) = self.model {
            arguments.push("--model".into());
            arguments.push(model);
        }
        if let Some(effort) = self.effort {
            append_effort_arguments(&mut arguments, self.harness, effort);
        }
        if self.prompt.as_encoded_bytes().starts_with(b"-") {
            arguments.push("--".into());
        }
        // The prompt stays last because harnesses read it as a positional argument.
        arguments.push(self.prompt);

        (default_program(self.harness).into(), arguments)
    }
}

fn default_program(harness: HeadlessHarness) -> &'static str {
    match harness {
        HeadlessHarness::Pi => "pi",
        HeadlessHarness::Omp => "omp",
        HeadlessHarness::Claude => "claude",
        HeadlessHarness::Codex => "codex",
    }
}

/// Codex takes a subcommand here where the other harnesses take a flag.
fn headless_argument(harness: HeadlessHarness) -> &'static str {
    match harness {
        HeadlessHarness::Pi | HeadlessHarness::Omp | HeadlessHarness::Claude => "--print",
        HeadlessHarness::Codex => "exec",
    }
}

/// Each harness names the reasoning effort differently.
fn append_effort_arguments(
    arguments: &mut Vec<OsString>,
    harness: HeadlessHarness,
    effort: OsString,
) {
    match harness {
        HeadlessHarness::Pi | HeadlessHarness::Claude => {
            arguments.push("--effort".into());
            arguments.push(effort);
        }
        HeadlessHarness::Omp => {
            arguments.push("--thinking".into());
            arguments.push(effort);
        }
        HeadlessHarness::Codex => {
            let mut override_value = OsString::from("model_reasoning_effort=");
            override_value.push(effort);
            arguments.push("-c".into());
            arguments.push(override_value);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use loom_policy::HeadlessHarness;

    use super::HarnessCall;

    #[test]
    fn omits_the_model_and_effort_when_unset() {
        let sut = HarnessCall::new(HeadlessHarness::Pi, "inspect the repository");

        let (program, arguments) = sut.into_parts();

        assert_eq!(program, PathBuf::from("pi"));
        assert_eq!(arguments, ["--print", "inspect the repository"]);
    }

    #[test]
    fn places_the_model_before_the_prompt() {
        let sut = HarnessCall::new(HeadlessHarness::Pi, "inspect the repository").model("opus");

        let (_, arguments) = sut.into_parts();

        assert_eq!(
            arguments,
            ["--print", "--model", "opus", "inspect the repository"]
        );
    }

    #[test]
    fn places_the_effort_before_the_prompt() {
        let sut = HarnessCall::new(HeadlessHarness::Pi, "inspect the repository").effort("high");

        let (_, arguments) = sut.into_parts();

        assert_eq!(
            arguments,
            ["--print", "--effort", "high", "inspect the repository"]
        );
    }

    #[test]
    fn sends_the_effort_to_omp_as_a_thinking_level() {
        let sut = HarnessCall::new(HeadlessHarness::Omp, "inspect the repository").effort("high");

        let (program, arguments) = sut.into_parts();

        assert_eq!(program, PathBuf::from("omp"));
        assert_eq!(
            arguments,
            ["--print", "--thinking", "high", "inspect the repository"]
        );
    }

    #[test]
    fn prints_from_claude_code_with_a_model_and_effort() {
        let sut = HarnessCall::new(HeadlessHarness::Claude, "inspect the repository")
            .model("opus")
            .effort("high");

        let (program, arguments) = sut.into_parts();

        assert_eq!(program, PathBuf::from("claude"));
        assert_eq!(
            arguments,
            [
                "--print",
                "--model",
                "opus",
                "--effort",
                "high",
                "inspect the repository"
            ]
        );
    }

    #[test]
    fn runs_codex_through_exec_and_sends_the_effort_as_a_config_override() {
        let sut = HarnessCall::new(HeadlessHarness::Codex, "inspect the repository")
            .model("gpt-5")
            .effort("high");

        let (program, arguments) = sut.into_parts();

        assert_eq!(program, PathBuf::from("codex"));
        assert_eq!(
            arguments,
            [
                "exec",
                "--model",
                "gpt-5",
                "-c",
                "model_reasoning_effort=high",
                "inspect the repository"
            ]
        );
    }

    #[test]
    fn places_the_model_and_effort_before_the_prompt() {
        let sut = HarnessCall::new(HeadlessHarness::Omp, "inspect the repository")
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
    fn separates_a_hyphenated_prompt_after_the_model_and_effort() {
        let sut = HarnessCall::new(HeadlessHarness::Pi, "--inspect the repository")
            .model("opus")
            .effort("high");

        let (_, arguments) = sut.into_parts();

        assert_eq!(
            arguments,
            [
                "--print",
                "--model",
                "opus",
                "--effort",
                "high",
                "--",
                "--inspect the repository"
            ]
        );
    }

    #[test]
    fn separates_a_hyphenated_prompt_for_every_harness() {
        for (harness, expected_program, expected_headless_argument) in [
            (HeadlessHarness::Pi, "pi", "--print"),
            (HeadlessHarness::Omp, "omp", "--print"),
            (HeadlessHarness::Claude, "claude", "--print"),
            (HeadlessHarness::Codex, "codex", "exec"),
        ] {
            let (program, arguments) =
                HarnessCall::new(harness, "--inspect the repository").into_parts();

            assert_eq!(program, PathBuf::from(expected_program));
            assert_eq!(
                arguments,
                [expected_headless_argument, "--", "--inspect the repository"]
            );
        }
    }
}
