use std::ffi::OsString;

use beez_policy::{HarnessOptions, HeadlessHarness};

use crate::process_call::ProcessCall;

/// Sends one prompt through a harness's non-interactive interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessCall {
    harness: HeadlessHarness,
    prompt: OsString,
    options: HarnessOptions,
}

impl HarnessCall {
    /// Uses the harness's default program with its default model and effort.
    #[must_use]
    pub fn new(harness: HeadlessHarness, prompt: impl Into<OsString>) -> Self {
        Self {
            harness,
            prompt: prompt.into(),
            options: HarnessOptions::default(),
        }
    }

    /// Uses the model and the effort the options name.
    ///
    /// An option that names neither keeps the harness default.
    #[must_use]
    pub fn options(mut self, options: HarnessOptions) -> Self {
        self.options = options;
        self
    }

    /// The harness that receives the prompt.
    #[must_use]
    pub fn harness(&self) -> HeadlessHarness {
        self.harness
    }
}

impl From<HarnessCall> for ProcessCall {
    fn from(call: HarnessCall) -> Self {
        let HarnessCall {
            harness,
            prompt,
            options,
        } = call;
        let mut arguments: Vec<OsString> = Vec::with_capacity(7);
        arguments.push(headless_argument(harness).into());
        if let Some(model) = options.model() {
            arguments.extend(["--model".into(), model.into()]);
        }
        if let Some(effort) = options.effort() {
            arguments.extend(effort_arguments(harness, effort));
        }
        if prompt.as_encoded_bytes().starts_with(b"-") {
            arguments.push("--".into());
        }
        // The prompt stays last because harnesses read it as a positional argument.
        arguments.push(prompt);

        Self::new(harness.name()).arguments(arguments)
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
fn effort_arguments(harness: HeadlessHarness, effort: &str) -> [OsString; 2] {
    match harness {
        HeadlessHarness::Pi | HeadlessHarness::Claude => ["--effort".into(), effort.into()],
        HeadlessHarness::Omp => ["--thinking".into(), effort.into()],
        HeadlessHarness::Codex => [
            "-c".into(),
            format!("model_reasoning_effort={effort}").into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use beez_policy::{HarnessOptions, HeadlessHarness};

    use super::HarnessCall;
    use crate::process_call::ProcessCall;

    /// A call that names the model and the effort a test needs.
    fn call(
        harness: HeadlessHarness,
        prompt: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> HarnessCall {
        HarnessCall::new(harness, prompt).options(HarnessOptions::new(
            model.map(str::to_owned),
            effort.map(str::to_owned),
        ))
    }

    #[test]
    fn omits_the_model_and_effort_when_unset() {
        let sut = HarnessCall::new(HeadlessHarness::Pi, "inspect the repository");

        let actual = ProcessCall::from(sut);

        assert_eq!(
            actual,
            ProcessCall::new("pi").arguments(["--print", "inspect the repository"])
        );
    }

    #[test]
    fn places_the_model_before_the_prompt() {
        let sut = call(
            HeadlessHarness::Pi,
            "inspect the repository",
            Some("opus"),
            None,
        );

        let actual = ProcessCall::from(sut);

        assert_eq!(
            actual,
            ProcessCall::new("pi").arguments([
                "--print",
                "--model",
                "opus",
                "inspect the repository"
            ])
        );
    }

    #[test]
    fn places_the_effort_before_the_prompt() {
        let sut = call(
            HeadlessHarness::Pi,
            "inspect the repository",
            None,
            Some("high"),
        );

        let actual = ProcessCall::from(sut);

        assert_eq!(
            actual,
            ProcessCall::new("pi").arguments([
                "--print",
                "--effort",
                "high",
                "inspect the repository"
            ])
        );
    }

    #[test]
    fn sends_the_effort_to_omp_as_a_thinking_level() {
        let sut = call(
            HeadlessHarness::Omp,
            "inspect the repository",
            None,
            Some("high"),
        );

        let actual = ProcessCall::from(sut);

        assert_eq!(
            actual,
            ProcessCall::new("omp").arguments([
                "--print",
                "--thinking",
                "high",
                "inspect the repository"
            ])
        );
    }

    #[test]
    fn prints_from_claude_code_with_a_model_and_effort() {
        let sut = call(
            HeadlessHarness::Claude,
            "inspect the repository",
            Some("opus"),
            Some("high"),
        );

        let actual = ProcessCall::from(sut);

        assert_eq!(
            actual,
            ProcessCall::new("claude").arguments([
                "--print",
                "--model",
                "opus",
                "--effort",
                "high",
                "inspect the repository"
            ])
        );
    }

    #[test]
    fn runs_codex_through_exec_and_sends_the_effort_as_a_config_override() {
        let sut = call(
            HeadlessHarness::Codex,
            "inspect the repository",
            Some("gpt-5"),
            Some("high"),
        );

        let actual = ProcessCall::from(sut);

        assert_eq!(
            actual,
            ProcessCall::new("codex").arguments([
                "exec",
                "--model",
                "gpt-5",
                "-c",
                "model_reasoning_effort=high",
                "inspect the repository"
            ])
        );
    }

    #[test]
    fn places_the_model_and_effort_before_the_prompt() {
        let sut = call(
            HeadlessHarness::Omp,
            "inspect the repository",
            Some("opus"),
            Some("high"),
        );

        let actual = ProcessCall::from(sut);

        assert_eq!(
            actual,
            ProcessCall::new("omp").arguments([
                "--print",
                "--model",
                "opus",
                "--thinking",
                "high",
                "inspect the repository"
            ])
        );
    }

    #[test]
    fn separates_a_hyphenated_prompt_after_the_model_and_effort() {
        let sut = call(
            HeadlessHarness::Pi,
            "--inspect the repository",
            Some("opus"),
            Some("high"),
        );

        let actual = ProcessCall::from(sut);

        assert_eq!(
            actual,
            ProcessCall::new("pi").arguments([
                "--print",
                "--model",
                "opus",
                "--effort",
                "high",
                "--",
                "--inspect the repository"
            ])
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
            let actual = ProcessCall::from(HarnessCall::new(harness, "--inspect the repository"));

            assert_eq!(
                actual,
                ProcessCall::new(expected_program).arguments([
                    expected_headless_argument,
                    "--",
                    "--inspect the repository"
                ])
            );
        }
    }
}
