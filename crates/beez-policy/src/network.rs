use crate::{DomainGroup, DomainRule, GroupSelection, HarnessProfile, Override, extend};

/// Which remote hosts sandboxed processes may reach.
///
/// `localhost` stays unset until a policy names it, so a task that only adds
/// a host keeps what the workflow chose.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NetworkPolicy {
    groups: GroupSelection<DomainGroup>,
    allow: Vec<DomainRule>,
    localhost: Override<bool>,
}

impl NetworkPolicy {
    /// Sets whether the harness's default domain groups apply; `None` leaves
    /// it unset.
    #[must_use]
    pub fn with_defaults(mut self, defaults: Option<bool>) -> Self {
        self.groups = self.groups.with_defaults(defaults);
        self
    }

    /// Sets the domain groups the policy adds.
    #[must_use]
    pub fn with_groups(mut self, groups: Vec<DomainGroup>) -> Self {
        self.groups = self.groups.with_groups(groups);
        self
    }

    /// Sets the default domain groups the policy disables.
    #[must_use]
    pub fn with_disable(mut self, disable: Vec<DomainGroup>) -> Self {
        self.groups = self.groups.with_disable(disable);
        self
    }

    /// Sets the host rules the policy adds.
    #[must_use]
    pub fn with_allow(mut self, allow: Vec<DomainRule>) -> Self {
        self.allow = allow;
        self
    }

    /// Sets whether processes may reach the loopback interface; `None` leaves
    /// it unset.
    #[must_use]
    pub fn with_localhost(mut self, localhost: Option<bool>) -> Self {
        self.localhost = localhost.into();
        self
    }

    /// Adds the rules of `other` to these rules.
    ///
    /// The lists join. A value `other` sets replaces the value here, and a
    /// value it leaves unset keeps the value here.
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.groups = self.groups.merge(other.groups);
        extend(&mut self.allow, other.allow);
        self.localhost = self.localhost.merge(other.localhost);
        self
    }

    /// True when processes may connect to services on the loopback interface.
    #[must_use]
    pub fn localhost(&self) -> bool {
        self.localhost.get_or(false)
    }

    /// Every host rule the policy grants for a harness.
    ///
    /// Required harness groups always apply. Default groups apply unless
    /// disabled. Explicit groups and rules always apply.
    #[must_use]
    pub fn allowed_domains(&self, profile: Option<HarnessProfile>) -> Vec<DomainRule> {
        let defaults = profile.map_or(&[][..], HarnessProfile::default_domain_groups);
        let mut groups = self.groups.selected(defaults);
        // A harness reaches no provider at all without its required groups.
        if let Some(profile) = profile {
            groups.extend(profile.required_domain_groups().iter().copied());
        }

        groups
            .into_iter()
            .flat_map(DomainGroup::rules)
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

        let domains = sut.allowed_domains(Some(profile));

        assert!(domains.contains(&rule("api.anthropic.com")));
        assert!(domains.contains(&rule("platform.claude.com")));
        assert!(!domains.contains(&rule("github.com")));
    }

    #[test]
    fn disables_a_default_harness_group() {
        let sut = NetworkPolicy::default()
            .with_defaults(Some(true))
            .with_disable(vec![DomainGroup::OpenAi, DomainGroup::Anthropic])
            .with_localhost(Some(false));
        let profile = HarnessProfile::for_harness(HeadlessHarness::Pi);

        let domains = sut.allowed_domains(Some(profile));

        assert!(domains.contains(&rule("generativelanguage.googleapis.com")));
        assert!(domains.contains(&rule("openrouter.ai")));
        assert!(!domains.contains(&rule("api.openai.com")));
        assert!(!domains.contains(&rule("api.anthropic.com")));
    }

    #[test]
    fn keeps_a_required_harness_group_a_task_tries_to_disable() {
        let sut = NetworkPolicy::default()
            .with_defaults(Some(true))
            .with_disable(vec![DomainGroup::Anthropic])
            .with_localhost(Some(false));
        let profile = HarnessProfile::for_harness(HeadlessHarness::Claude);

        let domains = sut.allowed_domains(Some(profile));

        assert!(domains.contains(&rule("api.anthropic.com")));
    }

    #[test]
    fn keeps_a_required_harness_group_without_defaults() {
        let sut = NetworkPolicy::default()
            .with_defaults(Some(false))
            .with_localhost(Some(false));

        let claude =
            sut.allowed_domains(Some(HarnessProfile::for_harness(HeadlessHarness::Claude)));
        let pi = sut.allowed_domains(Some(HarnessProfile::for_harness(HeadlessHarness::Pi)));

        assert!(claude.contains(&rule("api.anthropic.com")));
        assert_eq!(pi, Vec::<DomainRule>::new());
    }

    #[test]
    fn grants_a_group_the_task_both_asks_for_and_disables() {
        let sut = NetworkPolicy::default()
            .with_defaults(Some(true))
            .with_groups(vec![DomainGroup::Anthropic])
            .with_disable(vec![DomainGroup::Anthropic])
            .with_localhost(Some(false));
        let profile = HarnessProfile::for_harness(HeadlessHarness::Pi);

        let domains = sut.allowed_domains(Some(profile));

        assert!(domains.contains(&rule("api.anthropic.com")));
    }

    #[test]
    fn adds_groups_and_rules_without_defaults() {
        let sut = NetworkPolicy::default()
            .with_defaults(Some(false))
            .with_groups(vec![DomainGroup::Github])
            .with_allow(vec![rule("registry.internal:443")])
            .with_localhost(Some(false));
        let profile = HarnessProfile::for_harness(HeadlessHarness::Codex);

        let domains = sut.allowed_domains(Some(profile));

        assert!(domains.contains(&rule("api.openai.com")));
        assert!(domains.contains(&rule("github.com")));
        assert!(domains.contains(&rule("registry.internal:443")));
    }

    #[test]
    fn joins_the_rules_of_both_policies_on_merge() {
        let workflow = NetworkPolicy::default()
            .with_groups(vec![DomainGroup::Crates])
            .with_allow(vec![rule("registry.internal:443")])
            .with_localhost(Some(true));
        let task = NetworkPolicy::default()
            .with_groups(vec![DomainGroup::Github, DomainGroup::Crates])
            .with_allow(vec![rule("registry.internal:443")]);

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
        let workflow = NetworkPolicy::default().with_localhost(Some(true));
        let task = NetworkPolicy::default()
            .with_defaults(Some(false))
            .with_localhost(Some(false));

        let sut = workflow.merge(task);

        let profile = HarnessProfile::for_harness(HeadlessHarness::Pi);
        assert!(!sut.localhost());
        assert_eq!(sut.allowed_domains(Some(profile)), Vec::<DomainRule>::new());
    }

    #[test]
    fn grants_no_domains_to_a_command_without_rules() {
        let sut = NetworkPolicy::default();

        assert_eq!(sut.allowed_domains(None), Vec::<DomainRule>::new());
    }
}
