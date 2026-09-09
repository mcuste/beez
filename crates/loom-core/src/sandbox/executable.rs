use std::collections::BTreeSet;

use crate::sandbox::{ExecutableGroup, SandboxPath};

/// Which programs sandboxed processes may execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutablePolicy {
    defaults: bool,
    groups: Vec<ExecutableGroup>,
    disable: Vec<ExecutableGroup>,
    allow: Vec<SandboxPath>,
}

impl Default for ExecutablePolicy {
    fn default() -> Self {
        Self {
            defaults: true,
            groups: Vec::new(),
            disable: Vec::new(),
            allow: Vec::new(),
        }
    }
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
        defaults: bool,
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

    /// Groups whose programs may run.
    ///
    /// Default groups apply unless disabled. Explicit groups always apply, even
    /// when the same group is also disabled.
    #[must_use]
    pub fn groups(&self) -> Vec<ExecutableGroup> {
        DEFAULT_EXECUTABLE_GROUPS
            .iter()
            .filter(|group| self.defaults && !self.disable.contains(group))
            .chain(self.groups.iter())
            .copied()
            .collect::<BTreeSet<_>>()
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
    use crate::sandbox::ExecutableGroup;

    #[test]
    fn resolves_executable_groups() {
        let sut = ExecutablePolicy::new(
            true,
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
        let sut = ExecutablePolicy::new(false, vec![ExecutableGroup::Node], Vec::new(), Vec::new());

        assert_eq!(sut.groups(), [ExecutableGroup::Node]);
    }

    #[test]
    fn grants_a_group_the_task_both_asks_for_and_disables() {
        let sut = ExecutablePolicy::new(
            true,
            vec![ExecutableGroup::Net],
            vec![ExecutableGroup::Net],
            Vec::new(),
        );

        assert!(sut.groups().contains(&ExecutableGroup::Net));
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
