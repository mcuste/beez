use crate::{ExecutableGroup, GroupSelection, SandboxPath, extend};

/// Which programs sandboxed processes may execute.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExecutablePolicy {
    groups: GroupSelection<ExecutableGroup>,
    allow: Vec<SandboxPath>,
}

/// Tools most agent tasks need.
const DEFAULT_EXECUTABLE_GROUPS: [ExecutableGroup; 4] = [
    ExecutableGroup::Coreutils,
    ExecutableGroup::Text,
    ExecutableGroup::Git,
    ExecutableGroup::Net,
];

impl ExecutablePolicy {
    /// Sets whether Beez's default executable groups apply; `None` leaves it
    /// unset.
    #[must_use]
    pub fn with_defaults(mut self, defaults: Option<bool>) -> Self {
        self.groups = self.groups.with_defaults(defaults);
        self
    }

    /// Sets the executable groups the policy adds.
    #[must_use]
    pub fn with_groups(mut self, groups: Vec<ExecutableGroup>) -> Self {
        self.groups = self.groups.with_groups(groups);
        self
    }

    /// Sets the default executable groups the policy disables.
    #[must_use]
    pub fn with_disable(mut self, disable: Vec<ExecutableGroup>) -> Self {
        self.groups = self.groups.with_disable(disable);
        self
    }

    /// Sets the program names, files, or directories the policy adds.
    #[must_use]
    pub fn with_allow(mut self, allow: Vec<SandboxPath>) -> Self {
        self.allow = allow;
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
        self
    }

    /// Groups whose programs may run.
    ///
    /// Default groups apply unless disabled. Explicit groups always apply, even
    /// when the same group is also disabled.
    #[must_use]
    pub fn groups(&self) -> Vec<ExecutableGroup> {
        self.groups
            .selected(&DEFAULT_EXECUTABLE_GROUPS)
            .into_iter()
            .collect()
    }

    /// Program names, files, or directories that may run in addition to the groups.
    #[must_use]
    pub fn allow(&self) -> &[SandboxPath] {
        &self.allow
    }
}

#[cfg(test)]
mod tests {
    use super::ExecutablePolicy;
    use crate::ExecutableGroup;

    #[test]
    fn resolves_executable_groups() {
        let sut = ExecutablePolicy::default()
            .with_defaults(Some(true))
            .with_groups(vec![ExecutableGroup::Rust])
            .with_disable(vec![ExecutableGroup::Net]);

        assert_eq!(
            sut.groups(),
            [
                ExecutableGroup::Coreutils,
                ExecutableGroup::Text,
                ExecutableGroup::Git,
                ExecutableGroup::Rust,
            ]
        );
    }

    #[test]
    fn keeps_only_explicit_executable_groups_without_defaults() {
        let sut = ExecutablePolicy::default()
            .with_defaults(Some(false))
            .with_groups(vec![ExecutableGroup::Node]);

        assert_eq!(sut.groups(), [ExecutableGroup::Node]);
    }

    #[test]
    fn grants_a_group_the_task_both_asks_for_and_disables() {
        let sut = ExecutablePolicy::default()
            .with_defaults(Some(true))
            .with_groups(vec![ExecutableGroup::Net])
            .with_disable(vec![ExecutableGroup::Net]);

        assert!(sut.groups().contains(&ExecutableGroup::Net));
    }

    #[test]
    fn joins_the_groups_of_both_policies_on_merge() {
        let workflow = ExecutablePolicy::default()
            .with_defaults(Some(false))
            .with_groups(vec![ExecutableGroup::Coreutils]);
        let task = ExecutablePolicy::default()
            .with_groups(vec![ExecutableGroup::Rust, ExecutableGroup::Coreutils]);

        let sut = workflow.merge(task);

        assert_eq!(
            sut.groups(),
            [ExecutableGroup::Coreutils, ExecutableGroup::Rust],
            "the merged policy keeps defaults off"
        );
    }

    #[test]
    fn grants_the_default_groups_to_a_task_that_configures_nothing() {
        let sut = ExecutablePolicy::default();

        assert_eq!(
            sut.groups(),
            [
                ExecutableGroup::Coreutils,
                ExecutableGroup::Text,
                ExecutableGroup::Git,
                ExecutableGroup::Net,
            ]
        );
        assert_eq!(sut.allow(), Vec::<crate::SandboxPath>::new());
    }
}
