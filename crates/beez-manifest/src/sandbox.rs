use beez_policy::{
    DomainGroup, DomainRule, ExecutableGroup, ExecutablePolicy, FilesystemPolicy, NetworkPolicy,
    SandboxPath, SandboxPolicy, parse_all,
};
use serde::Deserialize;

/// A task's `sandbox` field: `true`, `false`, or a policy table.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum SandboxSetting {
    Enabled(bool),
    Policy(Box<ManifestSandbox>),
}

/// Policy sections as written in a manifest. Missing sections keep defaults.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestSandbox {
    network: Option<ManifestNetwork>,
    filesystem: Option<ManifestFilesystem>,
    executables: Option<ManifestExecutables>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestNetwork {
    #[serde(default)]
    defaults: Option<bool>,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    disable: Vec<String>,
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    localhost: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFilesystem {
    #[serde(default)]
    defaults: Option<bool>,
    #[serde(default)]
    read_deny: Vec<String>,
    #[serde(default)]
    write_allow: Vec<String>,
    #[serde(default)]
    write_deny: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestExecutables {
    #[serde(default)]
    defaults: Option<bool>,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    disable: Vec<String>,
    #[serde(default)]
    allow: Vec<String>,
}

/// Resolves a task's sandbox from the workflow default and the task's own setting.
///
/// A task runs sandboxed unless it sets `sandbox: false`.
pub(crate) fn resolve(
    workflow: Option<&SandboxPolicy>,
    task: Option<SandboxSetting>,
) -> Result<Option<SandboxPolicy>, String> {
    let base = || workflow.cloned().unwrap_or_default();
    match task {
        None | Some(SandboxSetting::Enabled(true)) => Ok(Some(base())),
        Some(SandboxSetting::Enabled(false)) => Ok(None),
        Some(SandboxSetting::Policy(sandbox)) => (*sandbox).apply_to(base()).map(Some),
    }
}

impl ManifestSandbox {
    /// Adds each section this table defines to `base`.
    pub(crate) fn apply_to(self, base: SandboxPolicy) -> Result<SandboxPolicy, String> {
        let network = self.network.map(NetworkPolicy::try_from).transpose()?;
        let filesystem = self
            .filesystem
            .map(FilesystemPolicy::try_from)
            .transpose()?;
        let executables = self
            .executables
            .map(ExecutablePolicy::try_from)
            .transpose()?;
        Ok(base.merge(network, filesystem, executables))
    }
}

impl TryFrom<ManifestSandbox> for SandboxPolicy {
    type Error = String;

    fn try_from(sandbox: ManifestSandbox) -> Result<Self, Self::Error> {
        sandbox.apply_to(Self::default())
    }
}

impl TryFrom<ManifestNetwork> for NetworkPolicy {
    type Error = String;

    fn try_from(network: ManifestNetwork) -> Result<Self, Self::Error> {
        Ok(Self::new(
            network.defaults,
            parse_all::<DomainGroup>(&network.groups)?,
            parse_all::<DomainGroup>(&network.disable)?,
            parse_all::<DomainRule>(&network.allow)?,
            network.localhost,
        ))
    }
}

impl TryFrom<ManifestFilesystem> for FilesystemPolicy {
    type Error = String;

    fn try_from(filesystem: ManifestFilesystem) -> Result<Self, Self::Error> {
        Ok(Self::new(
            filesystem.defaults,
            parse_all::<SandboxPath>(&filesystem.read_deny)?,
            parse_all::<SandboxPath>(&filesystem.write_allow)?,
            parse_all::<SandboxPath>(&filesystem.write_deny)?,
        ))
    }
}

impl TryFrom<ManifestExecutables> for ExecutablePolicy {
    type Error = String;

    fn try_from(executables: ManifestExecutables) -> Result<Self, Self::Error> {
        Ok(Self::new(
            executables.defaults,
            parse_all::<ExecutableGroup>(&executables.groups)?,
            parse_all::<ExecutableGroup>(&executables.disable)?,
            parse_all::<SandboxPath>(&executables.allow)?,
        ))
    }
}
