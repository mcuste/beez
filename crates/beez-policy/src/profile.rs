use crate::{DomainGroup, HeadlessHarness, PathGroup, SandboxPath};

/// Pi and Omp pick the provider from their own configuration.
const MULTI_PROVIDER_GROUPS: [DomainGroup; 4] = [
    DomainGroup::Anthropic,
    DomainGroup::OpenAi,
    DomainGroup::Google,
    DomainGroup::OpenRouter,
];

/// What a harness needs from the sandbox to run at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HarnessProfile(HeadlessHarness);

impl HarnessProfile {
    /// The profile of a supported harness.
    #[must_use]
    pub const fn for_harness(harness: HeadlessHarness) -> Self {
        Self(harness)
    }

    /// Domain groups the harness cannot run without.
    #[must_use]
    pub fn required_domain_groups(self) -> &'static [DomainGroup] {
        match self.0 {
            HeadlessHarness::Claude => &[DomainGroup::Anthropic],
            HeadlessHarness::Codex => &[DomainGroup::OpenAi],
            HeadlessHarness::Pi | HeadlessHarness::Omp => &[],
        }
    }

    /// Domain groups the harness may use, which a task can disable.
    #[must_use]
    pub fn default_domain_groups(self) -> &'static [DomainGroup] {
        match self.0 {
            HeadlessHarness::Claude | HeadlessHarness::Codex => &[],
            HeadlessHarness::Pi | HeadlessHarness::Omp => &MULTI_PROVIDER_GROUPS,
        }
    }

    /// Paths the harness reads and writes for its own configuration and state.
    #[must_use]
    pub fn state_paths(self) -> Vec<SandboxPath> {
        state_group(self.0).sandbox_paths()
    }

    /// Environment variables that keep the harness working inside the sandbox.
    #[must_use]
    pub fn environment(self) -> &'static [(&'static str, &'static str)] {
        environment(self.0)
    }
}

/// Where the harness keeps its own configuration and state.
fn state_group(harness: HeadlessHarness) -> PathGroup {
    match harness {
        HeadlessHarness::Claude => PathGroup::ClaudeState,
        HeadlessHarness::Codex => PathGroup::CodexState,
        HeadlessHarness::Pi => PathGroup::PiState,
        HeadlessHarness::Omp => PathGroup::OmpState,
    }
}

fn environment(harness: HeadlessHarness) -> &'static [(&'static str, &'static str)] {
    match harness {
        // Optional traffic must not retry against a closed network.
        HeadlessHarness::Claude => &[
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
            ("DISABLE_AUTOUPDATER", "1"),
            ("ENABLE_CLAUDEAI_MCP_SERVERS", "false"),
        ],
        HeadlessHarness::Codex | HeadlessHarness::Pi | HeadlessHarness::Omp => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::HarnessProfile;
    use crate::{DomainGroup, HeadlessHarness, SandboxPath};

    /// Without a domain group a harness reaches no provider, and without a
    /// state path its own configuration directory stays read-only.
    #[test]
    fn builds_a_usable_profile_for_every_harness() {
        for &harness in HeadlessHarness::ALL {
            let sut = HarnessProfile::for_harness(harness);

            assert!(
                !sut.required_domain_groups().is_empty() || !sut.default_domain_groups().is_empty(),
                "{harness} reaches no provider"
            );
            assert!(!sut.state_paths().is_empty(), "{harness} keeps no state");
        }
    }

    #[test]
    fn grants_every_harness_its_own_state_paths() {
        let expected = [
            (HeadlessHarness::Pi, &["~/.pi"][..]),
            (HeadlessHarness::Omp, &["~/.omp"][..]),
            (
                HeadlessHarness::Claude,
                &["~/.claude", "~/.claude.json"][..],
            ),
            (HeadlessHarness::Codex, &["~/.codex"][..]),
        ];

        for (harness, paths) in expected {
            let sut = HarnessProfile::for_harness(harness);

            assert_eq!(
                sut.state_paths()
                    .iter()
                    .map(SandboxPath::as_str)
                    .collect::<Vec<_>>(),
                paths,
                "{harness}"
            );
        }
    }

    #[test]
    fn requires_no_provider_of_a_multi_provider_harness() {
        for harness in [HeadlessHarness::Pi, HeadlessHarness::Omp] {
            let sut = HarnessProfile::for_harness(harness);

            assert!(sut.required_domain_groups().is_empty(), "{harness}");
            assert_eq!(
                sut.default_domain_groups(),
                [
                    DomainGroup::Anthropic,
                    DomainGroup::OpenAi,
                    DomainGroup::Google,
                    DomainGroup::OpenRouter,
                ]
            );
        }
    }
}
