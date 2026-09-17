use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use loom_policy::{
    DomainRule, ExecutableGroup, HarnessProfile, HeadlessHarness, SandboxPath, SandboxPolicy,
};

/// Programs every harness needs to run its own tools.
const HARNESS_PROGRAMS: [&str; 7] = ["sh", "bash", "zsh", "env", "node", "bun", "rg"];

/// Where Git keeps its helper programs, relative to the prefix of the `git` binary.
const GIT_HELPER_DIRECTORIES: [&str; 2] = ["libexec/git-core", "lib/git-core"];

/// Where the dynamic loaders live, by architecture and libc.
#[cfg(target_os = "linux")]
const LOADER_DIRECTORIES: [&str; 3] = ["/lib", "/lib64", "/usr/lib"];

/// Where macOS keeps the real tools behind the `/usr/bin` shims.
#[cfg(target_os = "macos")]
const MACOS_TOOLCHAIN_DIRECTORIES: [&str; 2] = [
    "/Library/Developer/CommandLineTools/usr",
    "/Applications/Xcode.app/Contents/Developer/usr",
];

/// A policy turned into absolute host paths and concrete host rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSandbox {
    pub(crate) allowed_domains: Vec<DomainRule>,
    pub(crate) localhost: bool,
    pub(crate) read_deny: Vec<PathBuf>,
    pub(crate) write_allow: Vec<PathBuf>,
    pub(crate) write_deny: Vec<PathBuf>,
    pub(crate) executables: Option<Vec<PathBuf>>,
    pub(crate) environment: Vec<(OsString, OsString)>,
    pub(crate) working_directory: PathBuf,
    pub(crate) program: PathBuf,
}

impl ResolvedSandbox {
    /// Resolves a policy for one program on this host.
    ///
    /// Paths that do not exist are dropped from read denies and write allows,
    /// because there is nothing to protect or grant yet. Write denies keep
    /// paths that do not exist so a run cannot create them. Program names
    /// missing from `PATH` are skipped.
    pub fn resolve(
        policy: &SandboxPolicy,
        harness: Option<HeadlessHarness>,
        program: &Path,
        working_directory: &Path,
        home: &Path,
    ) -> io::Result<Self> {
        let working_directory = fs::canonicalize(working_directory)?;
        let profile = harness.map(HarnessProfile::for_harness);
        let program = locate_program(program, &working_directory).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("sandbox cannot find program {}", program.display()),
            )
        })?;

        let filesystem = policy.filesystem();
        let read_deny = existing(&filesystem.read_deny(), home, &working_directory);
        let mut write_allow = existing(
            &filesystem.write_allow(profile.as_ref()),
            home,
            &working_directory,
        );
        if filesystem.defaults() {
            write_allow.extend(fs::canonicalize(std::env::temp_dir()));
        }
        let write_deny = dedup(
            filesystem
                .write_deny()
                .iter()
                .map(|path| canonicalize_lenient(&path.resolve(home, &working_directory)))
                .collect(),
        );

        let executables = policy.executables().map(|executables| {
            let groups = executables.groups();
            let state_paths = groups
                .iter()
                .flat_map(|group| group.state_sandbox_paths())
                .collect::<Vec<_>>();
            write_allow.extend(existing(&state_paths, home, &working_directory));
            resolve_executables(
                &groups,
                executables.allow(),
                &program,
                home,
                &working_directory,
            )
        });

        let environment = profile
            .as_ref()
            .map(HarnessProfile::environment)
            .unwrap_or_default()
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect();

        Ok(Self {
            allowed_domains: policy.network().allowed_domains(profile.as_ref()),
            localhost: policy.network().localhost(),
            read_deny,
            write_allow: dedup(write_allow),
            write_deny,
            executables,
            environment,
            working_directory,
            program,
        })
    }

    /// The canonical path of the program to run.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// True when a writable directory contains `path`.
    pub(crate) fn inside_write_allow(&self, path: &Path) -> bool {
        self.write_allow
            .iter()
            .any(|allowed| path.starts_with(allowed) && path != allowed.as_path())
    }

    /// Parents of write-denied paths that sit inside a writable directory.
    ///
    /// A task must not move a denied path out of the way and put a writable
    /// one in its place, so both backends stop these directories from being
    /// renamed or removed. Parents come before their own children.
    pub(crate) fn protected_ancestors(&self) -> Vec<PathBuf> {
        dedup(
            self.write_deny
                .iter()
                .flat_map(|denied| denied.ancestors().skip(1))
                .filter(|ancestor| self.inside_write_allow(ancestor))
                .map(Path::to_path_buf)
                .collect(),
        )
    }
}

/// Sorted, with each path once.
fn dedup(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Canonical paths of the rules that exist on this host.
fn existing(paths: &[SandboxPath], home: &Path, working_directory: &Path) -> Vec<PathBuf> {
    dedup(
        paths
            .iter()
            .filter_map(|path| canonical(path, home, working_directory))
            .collect(),
    )
}

/// The canonical path of one rule, when it exists on this host.
fn canonical(path: &SandboxPath, home: &Path, working_directory: &Path) -> Option<PathBuf> {
    fs::canonicalize(path.resolve(home, working_directory)).ok()
}

/// Canonicalizes the longest existing prefix and appends the rest unchanged.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    let mut missing = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        if let Ok(canonical) = fs::canonicalize(&current) {
            return missing
                .into_iter()
                .rev()
                .fold(canonical, |path, component| path.join(component));
        }
        let Some(file_name) = current.file_name().map(OsString::from) else {
            return path.to_path_buf();
        };
        missing.push(file_name);
        if !current.pop() {
            return path.to_path_buf();
        }
    }
}

/// Finds a program the way a shell would: on `PATH` for bare names, else by path.
fn locate_program(program: &Path, working_directory: &Path) -> Option<PathBuf> {
    let candidate = if program.components().count() == 1
        && !matches!(program.components().next(), Some(Component::RootDir))
    {
        find_on_path(program)?
    } else {
        // An absolute path replaces the working directory when joined.
        working_directory.join(program)
    };
    canonical_executable(&candidate)
}

pub(crate) fn find_on_path(name: &Path) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// The canonical path of `path`, when it is a file that may execute.
fn canonical_executable(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path)
        .ok()
        .filter(|path| is_executable_file(path))
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Every file or directory the execute allowlist must cover.
fn resolve_executables(
    groups: &[ExecutableGroup],
    allow: &[SandboxPath],
    program: &Path,
    home: &Path,
    working_directory: &Path,
) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    paths.insert(program.to_path_buf());

    let names = HARNESS_PROGRAMS
        .iter()
        .chain(groups.iter().flat_map(|group| group.programs().iter()))
        .map(Path::new)
        .chain(
            allow
                .iter()
                .filter(|path| path.is_bare_name())
                .map(|path| Path::new(path.as_str())),
        );
    for name in names {
        paths.extend(program_locations(name));
    }
    paths.extend(
        allow
            .iter()
            .filter(|path| !path.is_bare_name())
            .filter_map(|path| canonical(path, home, working_directory)),
    );

    if groups.contains(&ExecutableGroup::Git) {
        let git_binaries = paths
            .iter()
            .filter(|path| path.file_name().is_some_and(|name| name == "git"))
            .cloned()
            .collect::<Vec<_>>();
        paths.extend(
            git_binaries
                .iter()
                .flat_map(|git| git_helper_directories(git)),
        );
    }

    let interpreters = paths
        .iter()
        .filter(|path| path.is_file())
        .flat_map(|path| shebang_interpreters(path))
        .collect::<Vec<_>>();
    paths.extend(interpreters);
    #[cfg(target_os = "linux")]
    paths.extend(dynamic_loaders());

    paths.into_iter().collect()
}

/// The dynamic loaders on this host.
///
/// Landlock checks execute rights on the loader as well, so no dynamically
/// linked program starts unless the allowlist names it. The loader can start
/// any file it may read, so the list bounds the tooling a task reaches rather
/// than what it could do.
#[cfg(target_os = "linux")]
fn dynamic_loaders() -> Vec<PathBuf> {
    LOADER_DIRECTORIES
        .iter()
        .filter_map(|directory| fs::read_dir(directory).ok())
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().as_encoded_bytes().starts_with(b"ld-"))
        .filter_map(|entry| fs::canonicalize(entry.path()).ok())
        .collect()
}

/// The canonical file behind a program name, plus the real tool behind a macOS shim.
fn program_locations(name: &Path) -> Vec<PathBuf> {
    let mut locations = Vec::new();
    locations.extend(find_on_path(name).and_then(|path| canonical_executable(&path)));
    #[cfg(target_os = "macos")]
    locations.extend(
        MACOS_TOOLCHAIN_DIRECTORIES
            .iter()
            .map(|directory| Path::new(directory).join("bin").join(name))
            .filter_map(|candidate| canonical_executable(&candidate)),
    );
    locations
}

/// Helper directories next to a `git` binary, such as `libexec/git-core`.
fn git_helper_directories(git: &Path) -> Vec<PathBuf> {
    let Some(prefix) = git.parent().and_then(Path::parent) else {
        return Vec::new();
    };
    GIT_HELPER_DIRECTORIES
        .iter()
        .map(|helper| prefix.join(helper))
        .filter(|candidate| candidate.is_dir())
        .collect()
}

/// The interpreter a `#!` script runs, and the program `env` would look up.
fn shebang_interpreters(script: &Path) -> Vec<PathBuf> {
    let mut head = [0_u8; 256];
    let Ok(read) = fs::File::open(script).and_then(|mut file| file.read(&mut head)) else {
        return Vec::new();
    };
    let Some(line) = head
        .get(..read)
        .and_then(|head| head.strip_prefix(b"#!"))
        .and_then(|rest| rest.split(|byte| *byte == b'\n').next())
    else {
        return Vec::new();
    };
    let line = String::from_utf8_lossy(line);
    let mut words = line.split_whitespace();
    let Some(interpreter) = words.next() else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    paths.extend(fs::canonicalize(interpreter));
    if Path::new(interpreter)
        .file_name()
        .is_some_and(|name| name == "env")
    {
        let program = words.find(|word| !word.starts_with('-'));
        paths.extend(
            program
                .into_iter()
                .flat_map(|name| program_locations(Path::new(name))),
        );
    }
    paths
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};

    use loom_policy::{
        ExecutableGroup, ExecutablePolicy, FilesystemPolicy, HeadlessHarness, NetworkPolicy,
        SandboxPath, SandboxPolicy,
    };
    use loom_test_support::TemporaryDirectory;

    use super::{
        ResolvedSandbox, canonicalize_lenient, git_helper_directories, shebang_interpreters,
    };

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn canonical(path: &Path) -> PathBuf {
        fs::canonicalize(path).unwrap()
    }

    fn paths(texts: &[&str]) -> Vec<SandboxPath> {
        texts.iter().map(|text| text.parse().unwrap()).collect()
    }

    /// A policy without Loom's default paths, so a test sees only its own rules.
    fn policy(
        read_deny: &[&str],
        write_allow: &[&str],
        write_deny: &[&str],
        executables: Option<ExecutablePolicy>,
    ) -> SandboxPolicy {
        SandboxPolicy::new(
            NetworkPolicy::default(),
            FilesystemPolicy::new(
                Some(false),
                paths(read_deny),
                paths(write_allow),
                paths(write_deny),
            ),
            executables,
        )
    }

    /// An empty working directory and the home directory beside it.
    fn host(name: &str) -> (TemporaryDirectory, PathBuf, PathBuf) {
        let directory = TemporaryDirectory::new(name).unwrap();
        let work = directory.join("work");
        let home = directory.join("home");
        fs::create_dir(&work).unwrap();
        fs::create_dir(&home).unwrap();
        (directory, work, home)
    }

    fn resolve(policy: &SandboxPolicy, work: &Path, home: &Path) -> ResolvedSandbox {
        ResolvedSandbox::resolve(policy, None, Path::new("bash"), work, home).unwrap()
    }

    /// Writes an executable file with `contents` and returns its path.
    fn program(directory: &Path, name: &str, contents: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn drops_missing_read_denies_and_write_allows_but_keeps_missing_write_denies() {
        let (directory, work, home) = host("resolve-existence");
        let secret = directory.join("secret");
        fs::write(&secret, "s3cret").unwrap();
        let policy = policy(
            &[&text(&secret), &text(&directory.join("no-secret"))],
            &[&text(&work), &text(&directory.join("no-work"))],
            &[&text(&work.join("no-deny"))],
            None,
        );

        let sut = resolve(&policy, &work, &home);

        assert_eq!(sut.read_deny, [canonical(&secret)]);
        assert_eq!(sut.write_allow, [canonical(&work)]);
        assert_eq!(sut.write_deny, [canonical(&work).join("no-deny")]);
    }

    #[test]
    fn protects_every_parent_of_a_denied_path_up_to_the_writable_directory() {
        let (_directory, work, home) = host("resolve-ancestors");
        let policy = policy(
            &[],
            &[&text(&work)],
            &[&text(&work.join("a/b/hooks"))],
            None,
        );

        let sut = resolve(&policy, &work, &home);

        let work = canonical(&work);
        assert_eq!(
            sut.protected_ancestors(),
            [work.join("a"), work.join("a/b")]
        );
    }

    #[test]
    fn allows_writes_to_the_temporary_directory_only_with_defaults() {
        let (_directory, work, home) = host("resolve-temporary");
        let temporary = canonical(&std::env::temp_dir());
        let with_defaults = SandboxPolicy::new(
            NetworkPolicy::default(),
            FilesystemPolicy::new(Some(true), Vec::new(), paths(&["."]), Vec::new()),
            None,
        );

        let defaults = resolve(&with_defaults, &work, &home);
        let own_rules = resolve(&policy(&[], &[&text(&work)], &[], None), &work, &home);

        assert!(defaults.write_allow.contains(&temporary));
        assert!(!own_rules.write_allow.contains(&temporary));
    }

    #[test]
    fn allows_writes_to_the_state_paths_of_enabled_executable_groups() {
        let (_directory, work, home) = host("resolve-state");
        fs::create_dir(home.join(".cargo")).unwrap();
        let executables = ExecutablePolicy::new(
            Some(false),
            vec![ExecutableGroup::Rust],
            Vec::new(),
            Vec::new(),
        );

        let sut = resolve(
            &policy(&[], &[&text(&work)], &[], Some(executables)),
            &work,
            &home,
        );

        assert!(sut.write_allow.contains(&canonical(&home.join(".cargo"))));
        assert!(!sut.write_allow.contains(&home.join(".rustup")));
    }

    #[test]
    fn allows_writes_to_the_state_paths_of_the_harness() {
        let (_directory, work, home) = host("resolve-harness");
        fs::create_dir(home.join(".claude")).unwrap();
        fs::write(home.join(".claude.json"), "{}").unwrap();
        let policy = policy(&[], &[&text(&work)], &[], None);

        let sut = ResolvedSandbox::resolve(
            &policy,
            Some(HeadlessHarness::Claude),
            Path::new("bash"),
            &work,
            &home,
        )
        .unwrap();

        assert!(sut.write_allow.contains(&canonical(&home.join(".claude"))));
        assert!(
            sut.write_allow
                .contains(&canonical(&home.join(".claude.json")))
        );
        assert!(
            sut.environment
                .contains(&(OsString::from("DISABLE_AUTOUPDATER"), OsString::from("1")))
        );
    }

    #[test]
    fn grants_no_environment_to_a_command_without_a_harness() {
        let (_directory, work, home) = host("resolve-no-harness");

        let sut = resolve(&policy(&[], &[&text(&work)], &[], None), &work, &home);

        assert!(sut.environment.is_empty());
        assert!(sut.allowed_domains.is_empty());
    }

    #[test]
    fn leaves_execution_unrestricted_without_an_executable_section() {
        let (_directory, work, home) = host("resolve-unrestricted");

        let sut = resolve(&policy(&[], &[&text(&work)], &[], None), &work, &home);

        assert!(sut.executables.is_none());
    }

    #[test]
    fn always_allows_the_program_itself() {
        let (_directory, work, home) = host("resolve-program");
        let tool = program(&work, "tool.sh", "#!/bin/sh\nexit 0\n");
        let executables = ExecutablePolicy::new(Some(false), Vec::new(), Vec::new(), Vec::new());
        let policy = policy(&[], &[&text(&work)], &[], Some(executables));

        let sut =
            ResolvedSandbox::resolve(&policy, None, Path::new("./tool.sh"), &work, &home).unwrap();

        assert_eq!(sut.program(), canonical(&tool));
        assert!(sut.executables.unwrap().contains(&canonical(&tool)));
    }

    #[test]
    fn allows_the_interpreter_a_shebang_names() {
        let (_directory, work, home) = host("resolve-shebang");
        program(&work, "tool.sh", "#!/bin/sh\nexit 0\n");
        let executables = ExecutablePolicy::new(Some(false), Vec::new(), Vec::new(), Vec::new());
        let policy = policy(&[], &[&text(&work)], &[], Some(executables));

        let sut =
            ResolvedSandbox::resolve(&policy, None, Path::new("./tool.sh"), &work, &home).unwrap();

        assert!(
            sut.executables
                .unwrap()
                .contains(&canonical(Path::new("/bin/sh")))
        );
    }

    #[test]
    fn skips_group_programs_that_are_missing_from_path() {
        let (_directory, work, home) = host("resolve-groups");
        let executables = ExecutablePolicy::new(
            Some(true),
            ExecutableGroup::ALL.to_vec(),
            Vec::new(),
            Vec::new(),
        );

        let sut = resolve(
            &policy(&[], &[&text(&work)], &[], Some(executables)),
            &work,
            &home,
        );

        let executables = sut.executables.unwrap();
        assert!(executables.contains(&canonical(Path::new("/bin/sh"))));
        assert!(
            executables.iter().all(|path| path.exists()),
            "{executables:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allows_the_dynamic_loader_so_a_linked_program_can_start() {
        let (_directory, work, home) = host("resolve-loader");
        let executables = ExecutablePolicy::new(Some(true), Vec::new(), Vec::new(), Vec::new());

        let sut = resolve(
            &policy(&[], &[&text(&work)], &[], Some(executables)),
            &work,
            &home,
        );

        assert!(sut.executables.unwrap().iter().any(|path| {
            path.file_name()
                .is_some_and(|name| name.as_encoded_bytes().starts_with(b"ld-"))
        }));
    }

    #[test]
    fn resolves_a_relative_program_against_the_working_directory() {
        let (_directory, work, home) = host("resolve-relative");
        let nested = work.join("bin");
        fs::create_dir(&nested).unwrap();
        let tool = program(&nested, "tool.sh", "#!/bin/sh\nexit 0\n");
        let policy = policy(&[], &[&text(&work)], &[], None);

        let sut = ResolvedSandbox::resolve(&policy, None, Path::new("bin/tool.sh"), &work, &home)
            .unwrap();

        assert_eq!(sut.program(), canonical(&tool));
    }

    #[test]
    fn rejects_a_bare_program_name_that_is_not_on_path() {
        let (_directory, work, home) = host("resolve-missing-program");
        program(&work, "tool.sh", "#!/bin/sh\nexit 0\n");
        let policy = policy(&[], &[&text(&work)], &[], None);

        let error = ResolvedSandbox::resolve(&policy, None, Path::new("tool.sh"), &work, &home)
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn rejects_a_program_file_that_cannot_execute() {
        let (_directory, work, home) = host("resolve-not-executable");
        fs::write(work.join("notes.txt"), "text").unwrap();
        let policy = policy(&[], &[&text(&work)], &[], None);

        let error = ResolvedSandbox::resolve(&policy, None, Path::new("./notes.txt"), &work, &home)
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn finds_the_git_helper_directories_next_to_the_binary() {
        let directory = TemporaryDirectory::new("resolve-git").unwrap();
        let prefix = directory.join("usr");
        fs::create_dir_all(prefix.join("libexec/git-core")).unwrap();
        fs::create_dir_all(prefix.join("lib/git-core")).unwrap();
        fs::create_dir_all(prefix.join("bin")).unwrap();

        let helpers = git_helper_directories(&prefix.join("bin/git"));

        assert_eq!(
            helpers,
            [prefix.join("libexec/git-core"), prefix.join("lib/git-core")]
        );
    }

    #[test]
    fn ignores_git_helper_directories_that_do_not_exist() {
        let directory = TemporaryDirectory::new("resolve-git-missing").unwrap();

        assert!(git_helper_directories(&directory.join("usr/bin/git")).is_empty());
    }

    #[test]
    fn keeps_missing_components_after_the_canonical_prefix() {
        let temp = std::env::temp_dir();
        let canonical_temp = fs::canonicalize(&temp).unwrap();

        let resolved = canonicalize_lenient(&temp.join("loom-missing/child.json"));

        assert_eq!(resolved, canonical_temp.join("loom-missing/child.json"));
    }

    #[test]
    fn ignores_files_without_a_shebang() {
        assert!(shebang_interpreters(Path::new("/nonexistent/script")).is_empty());
    }
}
