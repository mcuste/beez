use std::collections::BTreeSet;

use crate::sandbox::{DomainGroup, DomainRule, HarnessProfile};

/// Which remote hosts sandboxed processes may reach.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkPolicy {
    defaults: bool,
    groups: Vec<DomainGroup>,
    disable: Vec<DomainGroup>,
    allow: Vec<DomainRule>,
    localhost: bool,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            defaults: true,
            groups: Vec::new(),
            disable: Vec::new(),
            allow: Vec::new(),
            localhost: false,
        }
    }
}

impl NetworkPolicy {
    /// Builds network rules.
    #[must_use]
    pub fn new(
        defaults: bool,
        groups: Vec<DomainGroup>,
        disable: Vec<DomainGroup>,
        allow: Vec<DomainRule>,
        localhost: bool,
    ) -> Self {
        Self {
            defaults,
            groups,
            disable,
            allow,
            localhost,
        }
    }

    /// True when processes may connect to services on the loopback interface.
    #[must_use]
    pub fn localhost(&self) -> bool {
        self.localhost
    }

    /// Every host rule the policy grants for a harness.
    ///
    /// Required harness groups always apply. Default groups apply unless
    /// disabled. Explicit groups and rules always apply.
    #[must_use]
    pub fn allowed_domains(&self, profile: Option<&HarnessProfile>) -> Vec<DomainRule> {
        let mut groups = BTreeSet::new();
        if let Some(profile) = profile {
            groups.extend(profile.required_domain_groups().iter().copied());
            if self.defaults {
                groups.extend(
                    profile
                        .default_domain_groups()
                        .iter()
                        .filter(|group| !self.disable.contains(group))
                        .copied(),
                );
            }
        }
        groups.extend(self.groups.iter().copied());

        groups
            .into_iter()
            .flat_map(|group| group.domains().iter().map(|domain| domain.parse()))
            .filter_map(Result::ok)
            .chain(self.allow.iter().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::NetworkPolicy;
    use crate::HeadlessHarness;
    use crate::sandbox::{DomainGroup, DomainRule, HarnessProfile};

    fn rule(text: &str) -> DomainRule {
        text.parse().unwrap()
    }

    #[test]
    fn grants_required_and_default_harness_domains() {
        let sut = NetworkPolicy::default();
        let profile = HarnessProfile::for_harness(HeadlessHarness::Claude);

        let domains = sut.allowed_domains(Some(&profile));

        assert!(domains.contains(&rule("api.anthropic.com")));
        assert!(domains.contains(&rule("platform.claude.com")));
        assert!(!domains.contains(&rule("github.com")));
    }

    #[test]
    fn disables_a_default_harness_group() {
        let sut = NetworkPolicy::new(
            true,
            Vec::new(),
            vec![DomainGroup::OpenAi, DomainGroup::Anthropic],
            Vec::new(),
            false,
        );
        let profile = HarnessProfile::for_harness(HeadlessHarness::Pi);

        let domains = sut.allowed_domains(Some(&profile));

        assert!(domains.contains(&rule("generativelanguage.googleapis.com")));
        assert!(domains.contains(&rule("openrouter.ai")));
        assert!(!domains.contains(&rule("api.openai.com")));
        assert!(!domains.contains(&rule("api.anthropic.com")));
    }

    #[test]
    fn keeps_a_required_harness_group_a_task_tries_to_disable() {
        let sut = NetworkPolicy::new(
            true,
            Vec::new(),
            vec![DomainGroup::Anthropic],
            Vec::new(),
            false,
        );
        let profile = HarnessProfile::for_harness(HeadlessHarness::Claude);

        let domains = sut.allowed_domains(Some(&profile));

        assert!(domains.contains(&rule("api.anthropic.com")));
    }

    #[test]
    fn keeps_a_required_harness_group_without_defaults() {
        let sut = NetworkPolicy::new(false, Vec::new(), Vec::new(), Vec::new(), false);

        let claude =
            sut.allowed_domains(Some(&HarnessProfile::for_harness(HeadlessHarness::Claude)));
        let pi = sut.allowed_domains(Some(&HarnessProfile::for_harness(HeadlessHarness::Pi)));

        assert!(claude.contains(&rule("api.anthropic.com")));
        assert!(pi.is_empty());
    }

    #[test]
    fn grants_a_group_the_task_both_asks_for_and_disables() {
        let sut = NetworkPolicy::new(
            true,
            vec![DomainGroup::Anthropic],
            vec![DomainGroup::Anthropic],
            Vec::new(),
            false,
        );
        let profile = HarnessProfile::for_harness(HeadlessHarness::Pi);

        let domains = sut.allowed_domains(Some(&profile));

        assert!(domains.contains(&rule("api.anthropic.com")));
    }

    #[test]
    fn adds_groups_and_rules_without_defaults() {
        let sut = NetworkPolicy::new(
            false,
            vec![DomainGroup::Github],
            Vec::new(),
            vec![rule("registry.internal:443")],
            false,
        );
        let profile = HarnessProfile::for_harness(HeadlessHarness::Codex);

        let domains = sut.allowed_domains(Some(&profile));

        assert!(domains.contains(&rule("api.openai.com")));
        assert!(domains.contains(&rule("github.com")));
        assert!(domains.contains(&rule("registry.internal:443")));
    }

    #[test]
    fn grants_no_domains_to_a_command_without_rules() {
        let sut = NetworkPolicy::default();

        assert!(sut.allowed_domains(None).is_empty());
    }
}
