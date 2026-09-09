use crate::sandbox::{HarnessProfile, SandboxPath};

/// Which paths sandboxed processes may read and write.
///
/// Reads are allowed unless denied. Writes are denied unless allowed, and a
/// write deny wins over a write allow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemPolicy {
    defaults: bool,
    read_deny: Vec<SandboxPath>,
    write_allow: Vec<SandboxPath>,
    write_deny: Vec<SandboxPath>,
}

impl Default for FilesystemPolicy {
    fn default() -> Self {
        Self {
            defaults: true,
            read_deny: Vec::new(),
            write_allow: Vec::new(),
            write_deny: Vec::new(),
        }
    }
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

/// Files a run could use to execute code outside the sandbox later.
const DEFAULT_WRITE_DENY: [&str; 8] = [
    ".git/hooks",
    ".git/config",
    ".claude",
    ".mcp.json",
    ".codex",
    ".agents",
    ".pi",
    ".omp",
];

impl FilesystemPolicy {
    /// Builds filesystem rules.
    #[must_use]
    pub fn new(
        defaults: bool,
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

    /// True when Loom's default paths apply.
    #[must_use]
    pub fn defaults(&self) -> bool {
        self.defaults
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
            .filter(|_| self.defaults)
            .map(|path| path.parse())
            .filter_map(Result::ok);
        defaults.chain(own.iter().cloned()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_READ_DENY, DEFAULT_WRITE_ALLOW, DEFAULT_WRITE_DENY, FilesystemPolicy};
    use crate::HeadlessHarness;
    use crate::sandbox::{HarnessProfile, SandboxPath};

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
        ] {
            assert!(write_deny.contains(&path(text)), "{text} is writable");
        }
    }

    #[test]
    fn combines_default_paths_with_task_paths() {
        let sut = FilesystemPolicy::new(
            true,
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
    fn drops_default_paths_on_request() {
        let sut = FilesystemPolicy::new(false, Vec::new(), vec![path("/data")], Vec::new());

        assert!(sut.read_deny().is_empty());
        assert_eq!(sut.write_allow(None), [path("/data")]);
        assert!(sut.write_deny().is_empty());
    }
}
