use std::collections::BTreeSet;

use crate::{DomainGroup, DomainRule, HarnessProfile, extend};

/// Which remote hosts sandboxed processes may reach.
///
/// `defaults` and `localhost` stay unset until a policy names them, so a task
/// that only adds a host keeps what the workflow chose.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NetworkPolicy {
    defaults: Option<bool>,
    groups: Vec<DomainGroup>,
    disable: Vec<DomainGroup>,
    allow: Vec<DomainRule>,
    localhost: Option<bool>,
}

impl NetworkPolicy {
    /// Builds network rules.
    #[must_use]
    pub fn new(
        defaults: Option<bool>,
        groups: Vec<DomainGroup>,
        disable: Vec<DomainGroup>,
        allow: Vec<DomainRule>,
        localhost: Option<bool>,
    ) -> Self {
        Self {
            defaults,
            groups,
            disable,
            allow,
            localhost,
        }
    }

    /// Adds the rules of `other` to these rules.
    ///
    /// The lists join. A value `other` sets replaces the value here, and a
    /// value it leaves unset keeps the value here.
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.defaults = other.defaults.or(self.defaults);
        extend(&mut self.groups, other.groups);
        extend(&mut self.disable, other.disable);
        extend(&mut self.allow, other.allow);
        self.localhost = other.localhost.or(self.localhost);
        self
    }

    /// True when processes may connect to services on the loopback interface.
    #[must_use]
    pub fn localhost(&self) -> bool {
        self.localhost.unwrap_or(false)
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
            if self.defaults.unwrap_or(true) {
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
    use crate::{DomainGroup, DomainRule, HarnessProfile, HeadlessHarness};

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
            Some(true),
            Vec::new(),
            vec![DomainGroup::OpenAi, DomainGroup::Anthropic],
            Vec::new(),
            Some(false),
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
            Some(true),
            Vec::new(),
            vec![DomainGroup::Anthropic],
            Vec::new(),
            Some(false),
        );
        let profile = HarnessProfile::for_harness(HeadlessHarness::Claude);

        let domains = sut.allowed_domains(Some(&profile));

        assert!(domains.contains(&rule("api.anthropic.com")));
    }

    #[test]
    fn keeps_a_required_harness_group_without_defaults() {
        let sut = NetworkPolicy::new(Some(false), Vec::new(), Vec::new(), Vec::new(), Some(false));

        let claude =
            sut.allowed_domains(Some(&HarnessProfile::for_harness(HeadlessHarness::Claude)));
        let pi = sut.allowed_domains(Some(&HarnessProfile::for_harness(HeadlessHarness::Pi)));

        assert!(claude.contains(&rule("api.anthropic.com")));
        assert!(pi.is_empty());
    }

    #[test]
    fn grants_a_group_the_task_both_asks_for_and_disables() {
        let sut = NetworkPolicy::new(
            Some(true),
            vec![DomainGroup::Anthropic],
            vec![DomainGroup::Anthropic],
            Vec::new(),
            Some(false),
        );
        let profile = HarnessProfile::for_harness(HeadlessHarness::Pi);

        let domains = sut.allowed_domains(Some(&profile));

        assert!(domains.contains(&rule("api.anthropic.com")));
    }

    #[test]
    fn adds_groups_and_rules_without_defaults() {
        let sut = NetworkPolicy::new(
            Some(false),
            vec![DomainGroup::Github],
            Vec::new(),
            vec![rule("registry.internal:443")],
            Some(false),
        );
        let profile = HarnessProfile::for_harness(HeadlessHarness::Codex);

        let domains = sut.allowed_domains(Some(&profile));

        assert!(domains.contains(&rule("api.openai.com")));
        assert!(domains.contains(&rule("github.com")));
        assert!(domains.contains(&rule("registry.internal:443")));
    }

    #[test]
    fn joins_the_rules_of_both_policies_on_merge() {
        let workflow = NetworkPolicy::new(
            None,
            vec![DomainGroup::Crates],
            Vec::new(),
            vec![rule("registry.internal:443")],
            Some(true),
        );
        let task = NetworkPolicy::new(
            None,
            vec![DomainGroup::Github, DomainGroup::Crates],
            Vec::new(),
            vec![rule("registry.internal:443")],
            None,
        );

        let sut = workflow.merge(task);

        let domains = sut.allowed_domains(None);
        assert!(domains.contains(&rule("github.com")));
        assert!(domains.contains(&rule("crates.io")));
        assert_eq!(
            domains
                .iter()
                .filter(|domain| **domain == rule("registry.internal:443"))
                .count(),
            1
        );
        assert!(sut.localhost(), "the task kept what the workflow chose");
    }

    #[test]
    fn takes_the_settings_the_merged_policy_names() {
        let workflow = NetworkPolicy::new(None, Vec::new(), Vec::new(), Vec::new(), Some(true));
        let task = NetworkPolicy::new(Some(false), Vec::new(), Vec::new(), Vec::new(), Some(false));

        let sut = workflow.merge(task);

        let profile = HarnessProfile::for_harness(HeadlessHarness::Pi);
        assert!(!sut.localhost());
        assert!(sut.allowed_domains(Some(&profile)).is_empty());
    }

    #[test]
    fn grants_no_domains_to_a_command_without_rules() {
        let sut = NetworkPolicy::default();

        assert!(sut.allowed_domains(None).is_empty());
    }
}
