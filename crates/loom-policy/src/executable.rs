use crate::{ExecutableGroup, SandboxPath, extend, selected_groups};

/// Which programs sandboxed processes may execute.
///
/// `defaults` stays unset until a policy names it, so a task that only adds a
/// program keeps what the workflow chose.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExecutablePolicy {
    defaults: Option<bool>,
    groups: Vec<ExecutableGroup>,
    disable: Vec<ExecutableGroup>,
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
    /// Builds execution rules.
    #[must_use]
    pub fn new(
        defaults: Option<bool>,
        groups: Vec<ExecutableGroup>,
        disable: Vec<ExecutableGroup>,
        allow: Vec<SandboxPath>,
    ) -> Self {
        Self {
            defaults,
            groups,
            disable,
            allow,
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
        self
    }

    /// Groups whose programs may run.
    ///
    /// Default groups apply unless disabled. Explicit groups always apply, even
    /// when the same group is also disabled.
    #[must_use]
    pub fn groups(&self) -> Vec<ExecutableGroup> {
        selected_groups(
            self.defaults.unwrap_or(true),
            &DEFAULT_EXECUTABLE_GROUPS,
            &self.disable,
            &self.groups,
        )
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
        let sut = ExecutablePolicy::new(
            Some(true),
            vec![ExecutableGroup::Rust],
            vec![ExecutableGroup::Net],
            Vec::new(),
        );

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
        let sut = ExecutablePolicy::new(
            Some(false),
            vec![ExecutableGroup::Node],
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(sut.groups(), [ExecutableGroup::Node]);
    }

    #[test]
    fn grants_a_group_the_task_both_asks_for_and_disables() {
        let sut = ExecutablePolicy::new(
            Some(true),
            vec![ExecutableGroup::Net],
            vec![ExecutableGroup::Net],
            Vec::new(),
        );

        assert!(sut.groups().contains(&ExecutableGroup::Net));
    }

    #[test]
    fn joins_the_groups_of_both_policies_on_merge() {
        let workflow = ExecutablePolicy::new(
            Some(false),
            vec![ExecutableGroup::Coreutils],
            Vec::new(),
            Vec::new(),
        );
        let task = ExecutablePolicy::new(
            None,
            vec![ExecutableGroup::Rust, ExecutableGroup::Coreutils],
            Vec::new(),
            Vec::new(),
        );

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
        assert!(sut.allow().is_empty());
    }
}
