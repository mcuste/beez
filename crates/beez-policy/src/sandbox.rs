use crate::{ExecutablePolicy, FilesystemPolicy, NetworkPolicy};

/// Restrictions Beez applies to a task's process tree.
///
/// Every section starts from Beez's defaults. A task adds or removes named
/// groups and adds its own rules. Missing `executables` leaves execution
/// unrestricted.
///
/// A task cannot take back a rule the workflow granted. Set `defaults: false`
/// in a section to drop Beez's own defaults from it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SandboxPolicy {
    network: NetworkPolicy,
    filesystem: FilesystemPolicy,
    executables: Option<ExecutablePolicy>,
}

impl SandboxPolicy {
    /// Builds a policy from its sections.
    #[must_use]
    pub fn new(
        network: NetworkPolicy,
        filesystem: FilesystemPolicy,
        executables: Option<ExecutablePolicy>,
    ) -> Self {
        Self {
            network,
            filesystem,
            executables,
        }
    }

    /// Network rules.
    #[must_use]
    pub fn network(&self) -> &NetworkPolicy {
        &self.network
    }

    /// Filesystem rules.
    #[must_use]
    pub fn filesystem(&self) -> &FilesystemPolicy {
        &self.filesystem
    }

    /// Execution rules, or `None` when execution is unrestricted.
    #[must_use]
    pub fn executables(&self) -> Option<&ExecutablePolicy> {
        self.executables.as_ref()
    }

    /// Adds the sections of `other` to this policy.
    ///
    /// A section `other` leaves at its default keeps the section here, and
    /// missing `executables` in `other` keeps the execution rules here.
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.network = self.network.merge(other.network);
        self.filesystem = self.filesystem.merge(other.filesystem);
        self.executables = match (self.executables, other.executables) {
            (Some(base), Some(executables)) => Some(base.merge(executables)),
            (base, executables) => executables.or(base),
        };
        self
    }
}

#[cfg(test)]
mod tests {
    use super::SandboxPolicy;
    use crate::{ExecutableGroup, ExecutablePolicy, FilesystemPolicy, NetworkPolicy};

    fn base() -> SandboxPolicy {
        SandboxPolicy::new(
            NetworkPolicy::default().with_localhost(Some(true)),
            FilesystemPolicy::default().with_write_allow(vec!["/data".parse().unwrap()]),
            Some(ExecutablePolicy::default().with_groups(vec![ExecutableGroup::Rust])),
        )
    }

    #[test]
    fn keeps_every_section_a_merged_policy_leaves_unset() {
        let sut = base().merge(SandboxPolicy::default());

        assert_eq!(sut, base());
    }

    #[test]
    fn keeps_unrestricted_execution_when_no_policy_restricts_it() {
        let sut = SandboxPolicy::default().merge(SandboxPolicy::default());

        assert_eq!(sut.executables(), None);
    }
}
