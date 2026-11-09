use std::fmt;

use beez_policy::{
    DomainGroup, DomainRule, ExecutableGroup, ExecutablePolicy, ExecutableRule, FilesystemPolicy,
    NetworkPolicy, SandboxPath, SandboxPolicy, parse_all,
};
use serde::de::value::MapAccessDeserializer;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

/// A task's `sandbox` field: `true`, `false`, or a policy table.
pub(crate) enum SandboxSetting {
    Enabled(bool),
    Policy(Box<ManifestSandbox>),
}

impl<'de> Deserialize<'de> for SandboxSetting {
    /// Reads the shape the manifest wrote, so a mistake inside a table reports
    /// the table's own error rather than a generic mismatch.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(SandboxSettingVisitor)
    }
}

struct SandboxSettingVisitor;

impl<'de> Visitor<'de> for SandboxSettingVisitor {
    type Value = SandboxSetting;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("true, false, or a sandbox table")
    }

    fn visit_bool<E: de::Error>(self, enabled: bool) -> Result<Self::Value, E> {
        Ok(SandboxSetting::Enabled(enabled))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        ManifestSandbox::deserialize(MapAccessDeserializer::new(map))
            .map(|sandbox| SandboxSetting::Policy(Box::new(sandbox)))
    }
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
        Some(SandboxSetting::Policy(sandbox)) => {
            Ok(Some(base().merge(SandboxPolicy::try_from(*sandbox)?)))
        }
    }
}

impl TryFrom<ManifestSandbox> for SandboxPolicy {
    type Error = String;

    /// Builds the policy the table defines; a missing section keeps defaults.
    fn try_from(sandbox: ManifestSandbox) -> Result<Self, Self::Error> {
        let network = sandbox.network.map(NetworkPolicy::try_from).transpose()?;
        let filesystem = sandbox
            .filesystem
            .map(FilesystemPolicy::try_from)
            .transpose()?;
        let executables = sandbox
            .executables
            .map(ExecutablePolicy::try_from)
            .transpose()?;
        Ok(Self::new(
            network.unwrap_or_default(),
            filesystem.unwrap_or_default(),
            executables,
        ))
    }
}

impl TryFrom<ManifestNetwork> for NetworkPolicy {
    type Error = String;

    fn try_from(network: ManifestNetwork) -> Result<Self, Self::Error> {
        Ok(Self::default()
            .with_defaults(network.defaults)
            .with_groups(parse_all::<DomainGroup>(&network.groups)?)
            .with_disable(parse_all::<DomainGroup>(&network.disable)?)
            .with_allow(parse_all::<DomainRule>(&network.allow)?)
            .with_localhost(network.localhost))
    }
}

impl TryFrom<ManifestFilesystem> for FilesystemPolicy {
    type Error = String;

    fn try_from(filesystem: ManifestFilesystem) -> Result<Self, Self::Error> {
        Ok(Self::default()
            .with_defaults(filesystem.defaults)
            .with_read_deny(parse_all::<SandboxPath>(&filesystem.read_deny)?)
            .with_write_allow(parse_all::<SandboxPath>(&filesystem.write_allow)?)
            .with_write_deny(parse_all::<SandboxPath>(&filesystem.write_deny)?))
    }
}

impl TryFrom<ManifestExecutables> for ExecutablePolicy {
    type Error = String;

    fn try_from(executables: ManifestExecutables) -> Result<Self, Self::Error> {
        Ok(Self::default()
            .with_defaults(executables.defaults)
            .with_groups(parse_all::<ExecutableGroup>(&executables.groups)?)
            .with_disable(parse_all::<ExecutableGroup>(&executables.disable)?)
            .with_allow(parse_all::<ExecutableRule>(&executables.allow)?))
    }
}
