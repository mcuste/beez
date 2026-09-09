//! Sandbox policy a task declares for the process that runs it.

mod domain;
mod executable;
mod filesystem;
mod group;
mod network;
mod path;
mod profile;

pub use domain::{DomainRule, DomainRuleError};
pub use executable::ExecutablePolicy;
pub use filesystem::FilesystemPolicy;
pub use group::{DomainGroup, ExecutableGroup, GroupError};
pub use network::NetworkPolicy;
pub use path::{SandboxPath, SandboxPathError};
pub use profile::HarnessProfile;

/// Restrictions Loom applies to a task's process tree.
///
/// Every section starts from Loom's defaults. A task adds or removes named
/// groups and adds its own rules. Missing `executables` leaves execution
/// unrestricted.
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

    /// Replaces each section the other policy defines.
    #[must_use]
    pub fn merge(
        mut self,
        network: Option<NetworkPolicy>,
        filesystem: Option<FilesystemPolicy>,
        executables: Option<ExecutablePolicy>,
    ) -> Self {
        if let Some(network) = network {
            self.network = network;
        }
        if let Some(filesystem) = filesystem {
            self.filesystem = filesystem;
        }
        if executables.is_some() {
            self.executables = executables;
        }
        self
    }
}
