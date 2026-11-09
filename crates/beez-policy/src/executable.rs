use std::fmt;
use std::str::FromStr;

use crate::{ExecutableGroup, GroupSelection, SandboxPath, SandboxPathError, extend};

/// Which programs sandboxed processes may execute.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExecutablePolicy {
    groups: GroupSelection<ExecutableGroup>,
    allow: Vec<ExecutableRule>,
}

/// A program an executable policy allows, by name or by path.
///
/// A rule without a directory separator, other than `~`, names a program to
/// look up on `PATH`. Any other rule is a file or directory.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ExecutableRule {
    /// A program name, looked up on `PATH` at launch.
    Program(String),
    /// A file, or a directory of files, that may execute.
    Path(SandboxPath),
}

impl FromStr for ExecutableRule {
    type Err = SandboxPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let path = value.parse::<SandboxPath>()?;
        Ok(if path.is_bare_name() {
            Self::Program(value.to_owned())
        } else {
            Self::Path(path)
        })
    }
}

impl fmt::Display for ExecutableRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Program(name) => formatter.write_str(name),
            Self::Path(path) => path.fmt(formatter),
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
    pub fn with_allow(mut self, allow: Vec<ExecutableRule>) -> Self {
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
    pub fn allow(&self) -> &[ExecutableRule] {
        &self.allow
    }

    /// Directories the programs of the granted groups need to write.
    #[must_use]
    pub fn state_paths(&self) -> Vec<SandboxPath> {
        self.groups()
            .into_iter()
            .flat_map(ExecutableGroup::state_sandbox_paths)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{ExecutablePolicy, ExecutableRule};
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
        assert_eq!(sut.allow(), Vec::<ExecutableRule>::new());
    }

    #[test]
    fn tells_program_names_from_paths() {
        for (text, expected) in [
            ("git", ExecutableRule::Program("git".into())),
            ("~", ExecutableRule::Path("~".parse().unwrap())),
            ("./git", ExecutableRule::Path("./git".parse().unwrap())),
            (
                "/usr/bin",
                ExecutableRule::Path("/usr/bin".parse().unwrap()),
            ),
        ] {
            let sut = text.parse::<ExecutableRule>().unwrap();

            assert_eq!(sut, expected, "{text}");
            assert_eq!(sut.to_string(), text);
        }
    }

    #[test]
    fn rejects_an_empty_rule() {
        assert_eq!(
            "".parse::<ExecutableRule>(),
            Err(crate::SandboxPathError::Empty)
        );
    }

    #[test]
    fn grants_the_state_paths_of_the_granted_groups() {
        let sut = ExecutablePolicy::default()
            .with_defaults(Some(false))
            .with_groups(vec![ExecutableGroup::Rust, ExecutableGroup::Git]);

        assert_eq!(
            sut.state_paths(),
            ["~/.cargo", "~/.rustup"].map(|text| text.parse::<crate::SandboxPath>().unwrap())
        );
    }
}
