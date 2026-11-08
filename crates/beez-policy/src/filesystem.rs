use crate::{HarnessProfile, Override, PathGroup, SandboxPath, extend};

/// Which paths sandboxed processes may read and write.
///
/// Reads are allowed unless denied. Writes are denied unless allowed, and a
/// write deny wins over a write allow.
///
/// `defaults` stays unset until a policy names it, so a task that only adds a
/// path keeps what the workflow chose.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FilesystemPolicy {
    defaults: Override<bool>,
    read_deny: Vec<SandboxPath>,
    write_allow: Vec<SandboxPath>,
    write_deny: Vec<SandboxPath>,
}

/// Credential stores no task should read.
const DEFAULT_READ_DENY_GROUPS: [PathGroup; 6] = [
    PathGroup::Keys,
    PathGroup::Cloud,
    PathGroup::Logins,
    PathGroup::Tokens,
    PathGroup::History,
    PathGroup::Secrets,
];

/// Where a task may write.
const DEFAULT_WRITE_ALLOW_GROUPS: [PathGroup; 1] = [PathGroup::Workspace];

/// Files a run could use to execute code outside the sandbox later, and the
/// artifacts of the run itself.
const DEFAULT_WRITE_DENY_GROUPS: [PathGroup; 6] = [
    PathGroup::Git,
    PathGroup::Ci,
    PathGroup::Editor,
    PathGroup::Toolchain,
    PathGroup::Harness,
    PathGroup::Artifacts,
];

impl FilesystemPolicy {
    /// Sets whether Beez's default paths apply; `None` leaves it unset.
    #[must_use]
    pub fn with_defaults(mut self, defaults: Option<bool>) -> Self {
        self.defaults = defaults.into();
        self
    }

    /// Sets the paths no process may read, besides the defaults.
    #[must_use]
    pub fn with_read_deny(mut self, paths: Vec<SandboxPath>) -> Self {
        self.read_deny = paths;
        self
    }

    /// Sets the paths processes may write, besides the defaults.
    #[must_use]
    pub fn with_write_allow(mut self, paths: Vec<SandboxPath>) -> Self {
        self.write_allow = paths;
        self
    }

    /// Sets the paths no process may write, besides the defaults.
    #[must_use]
    pub fn with_write_deny(mut self, paths: Vec<SandboxPath>) -> Self {
        self.write_deny = paths;
        self
    }

    /// Adds the rules of `other` to these rules.
    ///
    /// The lists join. A value `other` sets replaces the value here, and a
    /// value it leaves unset keeps the value here.
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.defaults = self.defaults.merge(other.defaults);
        extend(&mut self.read_deny, other.read_deny);
        extend(&mut self.write_allow, other.write_allow);
        extend(&mut self.write_deny, other.write_deny);
        self
    }

    /// True when Beez's default paths apply.
    #[must_use]
    pub fn defaults(&self) -> bool {
        self.defaults.get_or(true)
    }

    /// Paths no process may read.
    #[must_use]
    pub fn read_deny(&self) -> Vec<SandboxPath> {
        self.default_paths_and(&DEFAULT_READ_DENY_GROUPS, &self.read_deny)
    }

    /// Paths processes may write, including the harness's own state.
    #[must_use]
    pub fn write_allow(&self, profile: Option<&HarnessProfile>) -> Vec<SandboxPath> {
        let mut paths = self.default_paths_and(&DEFAULT_WRITE_ALLOW_GROUPS, &self.write_allow);
        if let Some(profile) = profile {
            paths.extend(profile.state_paths().iter().cloned());
        }
        paths
    }

    /// Paths no process may write even inside an allowed directory.
    #[must_use]
    pub fn write_deny(&self) -> Vec<SandboxPath> {
        self.default_paths_and(&DEFAULT_WRITE_DENY_GROUPS, &self.write_deny)
    }

    fn default_paths_and(&self, groups: &[PathGroup], own: &[SandboxPath]) -> Vec<SandboxPath> {
        let groups = if self.defaults() { groups } else { &[] };
        groups
            .iter()
            .flat_map(|group| group.sandbox_paths())
            .chain(own.iter().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::FilesystemPolicy;
    use crate::{HarnessProfile, HeadlessHarness, SandboxPath};

    fn path(text: &str) -> SandboxPath {
        text.parse().unwrap()
    }

    #[test]
    fn denies_reads_of_every_default_credential_store() {
        let sut = FilesystemPolicy::default();

        let read_deny = sut.read_deny();

        for text in [
            "~/.ssh",
            "~/.aws",
            "~/.gnupg",
            "~/.kube",
            "~/.netrc",
            "~/.docker",
            "~/.config/gcloud",
            "~/.azure",
            "~/.oci",
            "~/.databrickscfg",
            "~/.terraform.d/credentials.tfrc.json",
            "~/.git-credentials",
            "~/.config/gh",
            "~/.config/glab-cli",
            "~/.npmrc",
            "~/.pypirc",
            "~/.cargo/credentials.toml",
            "~/.gem/credentials",
            "~/.gradle/gradle.properties",
            "~/.bash_history",
            "~/.zsh_history",
            "~/.local/share/fish/fish_history",
            "~/Library/Keychains",
            "~/.password-store",
            "~/.config/op",
        ] {
            assert!(read_deny.contains(&path(text)), "{text} is readable");
        }
    }

    #[test]
    fn denies_writes_to_every_default_escape_path() {
        let sut = FilesystemPolicy::default();

        let write_deny = sut.write_deny();

        for text in [
            ".git/hooks",
            ".git/config",
            ".husky",
            ".pre-commit-config.yaml",
            ".github/workflows",
            ".gitlab-ci.yml",
            ".vscode",
            ".idea",
            ".devcontainer",
            ".envrc",
            ".cargo/config.toml",
            ".npmrc",
            ".yarnrc.yml",
            ".claude",
            ".mcp.json",
            ".codex",
            ".agents",
            ".pi",
            ".omp",
            ".beez",
        ] {
            assert!(write_deny.contains(&path(text)), "{text} is writable");
        }
    }

    #[test]
    fn combines_default_paths_with_task_paths() {
        let sut = FilesystemPolicy::default()
            .with_defaults(Some(true))
            .with_read_deny(vec![path("~/.config/secrets")])
            .with_write_allow(vec![path("/data")])
            .with_write_deny(vec![path("./locked")]);
        let profile = HarnessProfile::for_harness(HeadlessHarness::Claude);

        assert!(sut.read_deny().contains(&path("~/.ssh")));
        assert!(sut.read_deny().contains(&path("~/.config/secrets")));
        let write_allow = sut.write_allow(Some(&profile));
        assert!(write_allow.contains(&path(".")));
        assert!(write_allow.contains(&path("/tmp")));
        assert!(write_allow.contains(&path("/data")));
        assert!(write_allow.contains(&path("~/.claude")));
        assert!(sut.write_deny().contains(&path(".git/hooks")));
        assert!(sut.write_deny().contains(&path("./locked")));
    }

    #[test]
    fn joins_the_paths_of_both_policies_on_merge() {
        let workflow = FilesystemPolicy::default()
            .with_write_allow(vec![path("/data")])
            .with_write_deny(vec![path("./locked")]);
        let task = FilesystemPolicy::default()
            .with_defaults(Some(false))
            .with_read_deny(vec![path("~/.config/secrets")])
            .with_write_allow(vec![path("/data"), path("/cache")]);

        let sut = workflow.merge(task);

        assert_eq!(sut.read_deny(), [path("~/.config/secrets")]);
        assert_eq!(
            sut.write_allow(None),
            [path("/data"), path("/cache")],
            "the workflow path stays, and it stays once"
        );
        assert_eq!(sut.write_deny(), [path("./locked")]);
    }

    #[test]
    fn drops_default_paths_on_request() {
        let sut = FilesystemPolicy::default()
            .with_defaults(Some(false))
            .with_write_allow(vec![path("/data")]);

        assert_eq!(sut.read_deny(), Vec::<SandboxPath>::new());
        assert_eq!(sut.write_allow(None), [path("/data")]);
        assert_eq!(sut.write_deny(), Vec::<SandboxPath>::new());
    }
}
