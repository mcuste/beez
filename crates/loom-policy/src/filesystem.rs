use crate::{HarnessProfile, SandboxPath, extend};

/// Which paths sandboxed processes may read and write.
///
/// Reads are allowed unless denied. Writes are denied unless allowed, and a
/// write deny wins over a write allow.
///
/// `defaults` stays unset until a policy names it, so a task that only adds a
/// path keeps what the workflow chose.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FilesystemPolicy {
    defaults: Option<bool>,
    read_deny: Vec<SandboxPath>,
    write_allow: Vec<SandboxPath>,
    write_deny: Vec<SandboxPath>,
}

/// Credential stores no task should read.
const DEFAULT_READ_DENY: [&str; 8] = [
    "~/.ssh",
    "~/.aws",
    "~/.gnupg",
    "~/.kube",
    "~/.netrc",
    "~/.docker",
    "~/.config/gcloud",
    "~/.azure",
];

/// The task's working directory and the temporary directory.
const DEFAULT_WRITE_ALLOW: [&str; 2] = [".", "/tmp"];

/// Files a run could use to execute code outside the sandbox later, and the
/// artifacts of the run itself.
const DEFAULT_WRITE_DENY: [&str; 9] = [
    ".git/hooks",
    ".git/config",
    ".claude",
    ".mcp.json",
    ".codex",
    ".agents",
    ".pi",
    ".omp",
    ".loom",
];

impl FilesystemPolicy {
    /// Builds filesystem rules.
    #[must_use]
    pub fn new(
        defaults: Option<bool>,
        read_deny: Vec<SandboxPath>,
        write_allow: Vec<SandboxPath>,
        write_deny: Vec<SandboxPath>,
    ) -> Self {
        Self {
            defaults,
            read_deny,
            write_allow,
            write_deny,
        }
    }

    /// Adds the rules of `other` to these rules.
    ///
    /// The lists join. A value `other` sets replaces the value here, and a
    /// value it leaves unset keeps the value here.
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.defaults = other.defaults.or(self.defaults);
        extend(&mut self.read_deny, other.read_deny);
        extend(&mut self.write_allow, other.write_allow);
        extend(&mut self.write_deny, other.write_deny);
        self
    }

    /// True when Loom's default paths apply.
    #[must_use]
    pub fn defaults(&self) -> bool {
        self.defaults.unwrap_or(true)
    }

    /// Paths no process may read.
    #[must_use]
    pub fn read_deny(&self) -> Vec<SandboxPath> {
        self.with_defaults(&DEFAULT_READ_DENY, &self.read_deny)
    }

    /// Paths processes may write, including the harness's own state.
    #[must_use]
    pub fn write_allow(&self, profile: Option<&HarnessProfile>) -> Vec<SandboxPath> {
        let mut paths = self.with_defaults(&DEFAULT_WRITE_ALLOW, &self.write_allow);
        if let Some(profile) = profile {
            paths.extend(profile.state_paths().iter().cloned());
        }
        paths
    }

    /// Paths no process may write even inside an allowed directory.
    #[must_use]
    pub fn write_deny(&self) -> Vec<SandboxPath> {
        self.with_defaults(&DEFAULT_WRITE_DENY, &self.write_deny)
    }

    fn with_defaults(&self, defaults: &[&str], own: &[SandboxPath]) -> Vec<SandboxPath> {
        let defaults = defaults
            .iter()
            .filter(|_| self.defaults())
            .map(|path| path.parse())
            .filter_map(Result::ok);
        defaults.chain(own.iter().cloned()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_READ_DENY, DEFAULT_WRITE_ALLOW, DEFAULT_WRITE_DENY, FilesystemPolicy};
    use crate::{HarnessProfile, HeadlessHarness, SandboxPath};

    fn path(text: &str) -> SandboxPath {
        text.parse().unwrap()
    }

    /// A default path that fails to parse is dropped, which removes a rule.
    #[test]
    fn parses_every_default_path() {
        for text in DEFAULT_READ_DENY
            .iter()
            .chain(&DEFAULT_WRITE_ALLOW)
            .chain(&DEFAULT_WRITE_DENY)
        {
            assert!(text.parse::<SandboxPath>().is_ok(), "{text}");
        }
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
            ".claude",
            ".mcp.json",
            ".codex",
            ".agents",
            ".pi",
            ".omp",
            ".loom",
        ] {
            assert!(write_deny.contains(&path(text)), "{text} is writable");
        }
    }

    #[test]
    fn combines_default_paths_with_task_paths() {
        let sut = FilesystemPolicy::new(
            Some(true),
            vec![path("~/.config/secrets")],
            vec![path("/data")],
            vec![path("./locked")],
        );
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
        let workflow = FilesystemPolicy::new(
            None,
            Vec::new(),
            vec![path("/data")],
            vec![path("./locked")],
        );
        let task = FilesystemPolicy::new(
            Some(false),
            vec![path("~/.config/secrets")],
            vec![path("/data"), path("/cache")],
            Vec::new(),
        );

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
        let sut = FilesystemPolicy::new(Some(false), Vec::new(), vec![path("/data")], Vec::new());

        assert!(sut.read_deny().is_empty());
        assert_eq!(sut.write_allow(None), [path("/data")]);
        assert!(sut.write_deny().is_empty());
    }
}
