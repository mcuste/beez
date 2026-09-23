//! Linux backend: bubblewrap namespaces with the proxies bridged over Unix sockets.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::init::relay_argument;
use crate::proxy::loopback;
use crate::resolve::ResolvedSandbox;
use crate::server::{Server, UnixPathListener, forward};

/// Loopback port of the HTTP proxy inside the sandbox.
pub(crate) const HTTP_PORT: u16 = 3128;
/// Loopback port of the SOCKS5 proxy inside the sandbox.
pub(crate) const SOCKS_PORT: u16 = 1080;

static BRIDGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Unix sockets on the host that forward to the proxies.
///
/// The sandbox has no network namespace of its own, so these sockets are the
/// only way out. They are bind-mounted into the sandbox.
#[derive(Debug)]
pub(crate) struct Bridge {
    // The servers stop before the guard removes the sockets they listen on.
    _servers: [Server; 2],
    directory: SocketDirectory,
    empty: PathBuf,
    http_socket: PathBuf,
    socks_socket: PathBuf,
}

impl Bridge {
    pub(crate) fn start(http_port: u16, socks_port: u16) -> io::Result<Self> {
        let sequence = BRIDGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("beez-sandbox-{}-{sequence}", std::process::id()));
        fs::create_dir(&path)?;
        let directory = SocketDirectory(path);
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        let empty = directory.path().join("empty");
        fs::create_dir(&empty)?;
        let http_socket = directory.path().join("http.sock");
        let socks_socket = directory.path().join("socks.sock");
        let http = forward_socket(&http_socket, http_port)?;
        let socks = forward_socket(&socks_socket, socks_port)?;

        Ok(Self {
            _servers: [http, socks],
            directory,
            empty,
            http_socket,
            socks_socket,
        })
    }

    /// A directory with nothing in it, to mask paths that must stay unwritable.
    fn empty_directory(&self) -> &Path {
        &self.empty
    }
}

/// Removes the socket directory when the bridge stops.
#[derive(Debug)]
struct SocketDirectory(PathBuf);

impl SocketDirectory {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for SocketDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A Unix socket on the host that forwards to a proxy port.
fn forward_socket(socket: &Path, port: u16) -> io::Result<Server> {
    let listener = UnixPathListener::bind(socket.to_path_buf())?;

    Ok(forward(listener, move || {
        TcpStream::connect(loopback(port))
    }))
}

/// Wraps the program in `bwrap`, entered through `beez sandbox-init`.
pub(crate) fn command(
    resolved: &ResolvedSandbox,
    bridge: &Bridge,
    arguments: &[OsString],
) -> io::Result<(Command, HeldMasks)> {
    let bwrap = crate::resolve::find_on_path(Path::new("bwrap")).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "sandbox requires bubblewrap; install the bwrap package",
        )
    })?;
    let beez = std::env::current_exe()?;
    let plan = MaskPlan::of(resolved);

    let mut command = Command::new(bwrap);
    command.args(bwrap_arguments(resolved, bridge, &beez, arguments, &plan));
    Ok((command, plan.hold()))
}

/// Marks a mask directory as needed by a running sandbox.
const MARKER_PREFIX: &str = ".beez-mask-";

const MARK_ATTEMPTS: u32 = 4;

static MARKER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The denied paths that need an empty directory to mask them.
///
/// `bwrap` creates a mount point that is missing, inside a directory bound
/// from the host, so it would stay there after the sandbox ends.
#[derive(Debug)]
struct MaskPlan {
    /// Each masked path with the directories it needs, deepest first.
    masks: Vec<(PathBuf, Vec<PathBuf>)>,
}

impl MaskPlan {
    fn of(resolved: &ResolvedSandbox) -> Self {
        let masks = resolved
            .write_deny
            .iter()
            .filter(|path| resolved.inside_write_allow(path))
            .map(|path| (path.clone(), mask_directories(path)))
            .filter(|(_, directories)| !directories.is_empty())
            .collect();

        Self { masks }
    }

    /// True when an empty directory masks `path`.
    fn covers(&self, path: &Path) -> bool {
        self.masks.iter().any(|(masked, _)| masked == path)
    }

    /// Builds every mask directory and marks it as in use.
    ///
    /// Runs that share a working directory need the same directories, so each
    /// adds a marker of its own. A marker means a run has not ended.
    fn hold(self) -> HeldMasks {
        let name = format!(
            "{MARKER_PREFIX}{}-{}",
            std::process::id(),
            MARKER_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let mut markers = Vec::new();
        let mut directories = Vec::new();
        for (_, needed) in self.masks {
            if let Some(marked) = mark_in_use(&needed, &name) {
                markers.extend(marked);
                directories.extend(needed);
            }
        }

        HeldMasks {
            markers,
            directories,
        }
    }
}

/// The directories a mask of `path` needs, deepest first.
///
/// These are `path` and every parent that holds nothing but masks. The list
/// is empty for a path that exists, which needs no mask.
fn mask_directories(path: &Path) -> Vec<PathBuf> {
    path.ancestors()
        .take_while(|directory| !directory.exists() || is_marked(directory))
        .map(Path::to_path_buf)
        .collect()
}

fn is_marked(directory: &Path) -> bool {
    markers(directory).next().is_some()
}

fn markers(directory: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(MARKER_PREFIX))
        })
        .map(|entry| entry.path())
}

/// Creates the directories of one mask and marks each with `name`.
///
/// Another run can take a directory away between the two steps, so this tries
/// again. Answers where the markers went.
fn mark_in_use(directories: &[PathBuf], name: &str) -> Option<Vec<PathBuf>> {
    let deepest = directories.first()?;
    for _ in 0..MARK_ATTEMPTS {
        if fs::create_dir_all(deepest).is_err() {
            return None;
        }
        // Marking a parent first stops another run from taking it away
        // while the deeper directories are still unmarked.
        let marked = directories
            .iter()
            .rev()
            .map(|directory| {
                let marker = directory.join(name);
                fs::write(&marker, b"").ok().map(|()| marker)
            })
            .collect::<Option<Vec<_>>>();
        if let Some(marked) = marked {
            for directory in directories {
                remove_dead_markers(directory);
            }
            return Some(marked);
        }
    }

    None
}

/// Removes the markers of runs that ended without giving their masks back.
fn remove_dead_markers(directory: &Path) {
    for marker in markers(directory) {
        let owner = marker
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(MARKER_PREFIX))
            .and_then(|name| name.split('-').next())
            .unwrap_or_default();
        if !Path::new("/proc").join(owner).exists() {
            let _ = fs::remove_file(&marker);
        }
    }
}

/// Gives back the mask directories of one run when it ends.
#[derive(Debug)]
pub(crate) struct HeldMasks {
    markers: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

impl Drop for HeldMasks {
    fn drop(&mut self) {
        for marker in &self.markers {
            let _ = fs::remove_file(marker);
        }
        for directory in &self.directories {
            // Only an empty directory goes away: another run's marker, or
            // any real content, keeps it.
            let _ = fs::remove_dir(directory);
        }
    }
}

/// Every `bwrap` argument, ending with the program to run inside the sandbox.
///
/// The arguments after `sandbox-init` are read back by Beez's own
/// `sandbox-init` command, so their names must match its options.
fn bwrap_arguments(
    resolved: &ResolvedSandbox,
    bridge: &Bridge,
    beez: &Path,
    program_arguments: &[OsString],
    plan: &MaskPlan,
) -> Vec<OsString> {
    let mut arguments = flags(&[
        "--new-session",
        "--die-with-parent",
        "--unshare-net",
        "--unshare-pid",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
    ]);
    if !resolved
        .write_allow
        .iter()
        .any(|path| path == Path::new("/tmp"))
    {
        arguments.extend(flags(&["--tmpfs", "/tmp"]));
    }
    for path in &resolved.write_allow {
        bind(&mut arguments, "--bind", path, path);
    }
    // A parent bound onto itself becomes a mount point, and the kernel refuses
    // to rename a mount point. Without it a task moves the parent aside and the
    // deny mount goes with it. This bind must come before the denies inside it,
    // and a parent that does not exist yet cannot be bound.
    for path in resolved.protected_ancestors() {
        if path.exists() {
            bind(&mut arguments, "--bind", &path, &path);
        }
    }
    for path in &resolved.write_deny {
        if plan.covers(path) {
            bind(&mut arguments, "--ro-bind", bridge.empty_directory(), path);
        } else if path.exists() {
            bind(&mut arguments, "--ro-bind", path, path);
        }
        // A path outside every writable directory needs no mount.
    }
    for path in &resolved.read_deny {
        if path.is_dir() {
            option(&mut arguments, "--tmpfs", path);
        } else {
            bind(&mut arguments, "--ro-bind", Path::new("/dev/null"), path);
        }
    }
    bind(
        &mut arguments,
        "--bind",
        bridge.directory.path(),
        bridge.directory.path(),
    );
    option(&mut arguments, "--chdir", &resolved.working_directory);
    arguments.push(OsString::from("--"));
    arguments.push(beez.into());
    arguments.push(OsString::from("sandbox-init"));
    for (port, socket) in [
        (HTTP_PORT, &bridge.http_socket),
        (SOCKS_PORT, &bridge.socks_socket),
    ] {
        arguments.push(OsString::from("--relay"));
        arguments.push(relay_argument(port, socket));
    }
    for path in resolved.executables.iter().flatten() {
        option(&mut arguments, "--exec", path);
    }
    arguments.push(OsString::from("--"));
    arguments.push(resolved.program().into());
    arguments.extend(program_arguments.iter().cloned());
    arguments
}

fn flags(names: &[&str]) -> Vec<OsString> {
    names.iter().map(OsString::from).collect()
}

fn option(arguments: &mut Vec<OsString>, name: &str, value: &Path) {
    arguments.push(OsString::from(name));
    arguments.push(value.into());
}

fn bind(arguments: &mut Vec<OsString>, mode: &str, source: &Path, target: &Path) {
    option(arguments, mode, source);
    arguments.push(target.into());
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};

    use beez_test_support::{TemporaryDirectory, text};

    use super::{Bridge, HTTP_PORT, MaskPlan, SOCKS_PORT, bwrap_arguments};
    use crate::resolve::ResolvedSandbox;

    /// A policy that may write `work` and must not write `.git/hooks` in it.
    fn resolved(work: &Path) -> ResolvedSandbox {
        ResolvedSandbox {
            allowed_domains: Vec::new(),
            localhost: false,
            read_deny: Vec::new(),
            write_allow: vec![work.to_path_buf()],
            write_deny: vec![work.join(".git/hooks")],
            executables: None,
            environment: Vec::new(),
            working_directory: work.to_path_buf(),
            program: PathBuf::from("/bin/bash"),
        }
    }

    fn bridge() -> Bridge {
        Bridge::start(0, 0).unwrap()
    }

    fn arguments(resolved: &ResolvedSandbox, bridge: &Bridge) -> Vec<String> {
        bwrap_arguments(
            resolved,
            bridge,
            Path::new("/usr/local/bin/beez"),
            &[OsString::from("-c"), OsString::from("exit 0")],
            &MaskPlan::of(resolved),
        )
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
    }

    /// Where `expected` appears as consecutive arguments.
    fn run_at(arguments: &[String], expected: &[&str]) -> Option<usize> {
        arguments
            .windows(expected.len())
            .position(|window| window.iter().zip(expected).all(|(one, two)| one == two))
    }

    /// True when `expected` appears as consecutive arguments.
    fn has_run(arguments: &[String], expected: &[&str]) -> bool {
        run_at(arguments, expected).is_some()
    }

    #[test]
    fn binds_writable_paths_and_read_only_binds_denied_paths_that_exist() {
        let directory = TemporaryDirectory::new("bwrap-writes").unwrap();
        let work = directory.join("work");
        fs::create_dir_all(work.join(".git/hooks")).unwrap();
        let bridge = bridge();

        let sut = arguments(&resolved(&work), &bridge);

        let work = work.to_string_lossy().into_owned();
        assert!(has_run(&sut, &["--bind", &work, &work]), "{sut:?}");
        let hooks = format!("{work}/.git/hooks");
        assert!(has_run(&sut, &["--ro-bind", &hooks, &hooks]), "{sut:?}");
    }

    #[test]
    fn binds_the_parent_of_a_denied_path_before_the_deny_so_it_cannot_be_renamed() {
        let directory = TemporaryDirectory::new("bwrap-deny-parent").unwrap();
        let work = directory.join("work");
        fs::create_dir_all(work.join(".git/hooks")).unwrap();
        let bridge = bridge();

        let sut = arguments(&resolved(&work), &bridge);

        let git = text(&work.join(".git"));
        let hooks = text(&work.join(".git/hooks"));
        let parent = run_at(&sut, &["--bind", &git, &git]);
        let deny = run_at(&sut, &["--ro-bind", &hooks, &hooks]);
        assert!(
            matches!((parent, deny), (Some(parent), Some(deny)) if parent < deny),
            "{sut:?}"
        );
    }

    #[test]
    fn binds_no_parent_of_a_denied_path_that_does_not_exist_yet() {
        let directory = TemporaryDirectory::new("bwrap-missing-parent").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let bridge = bridge();

        let sut = arguments(&resolved(&work), &bridge);

        let git = text(&work.join(".git"));
        assert!(!has_run(&sut, &["--bind", &git, &git]), "{sut:?}");
    }

    #[test]
    fn masks_a_denied_path_that_does_not_exist_yet_with_an_empty_directory() {
        let directory = TemporaryDirectory::new("bwrap-missing-deny").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let bridge = bridge();

        let sut = arguments(&resolved(&work), &bridge);

        let empty = bridge.empty_directory().to_string_lossy().into_owned();
        let hooks = format!("{}/.git/hooks", work.to_string_lossy());
        assert!(has_run(&sut, &["--ro-bind", &empty, &hooks]), "{sut:?}");
    }

    #[test]
    fn leaves_a_denied_path_outside_every_writable_directory_unmounted() {
        let directory = TemporaryDirectory::new("bwrap-unwritable-deny").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let mut policy = resolved(&work);
        policy.write_deny = vec![directory.join("elsewhere/.claude")];
        let bridge = bridge();

        let sut = arguments(&policy, &bridge);

        let denied = directory.join("elsewhere/.claude");
        let denied = denied.to_string_lossy().into_owned();
        assert!(!sut.contains(&denied), "{sut:?}");
    }

    #[test]
    fn replaces_tmp_with_a_tmpfs_unless_the_policy_may_write_it() {
        let directory = TemporaryDirectory::new("bwrap-tmp").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let mut writable_tmp = resolved(&work);
        writable_tmp.write_allow.push(PathBuf::from("/tmp"));
        let bridge = bridge();

        let private_tmp = arguments(&resolved(&work), &bridge);
        let shared_tmp = arguments(&writable_tmp, &bridge);

        assert!(
            has_run(&private_tmp, &["--tmpfs", "/tmp"]),
            "{private_tmp:?}"
        );
        assert!(
            !has_run(&shared_tmp, &["--tmpfs", "/tmp"]),
            "{shared_tmp:?}"
        );
        assert!(
            has_run(&shared_tmp, &["--bind", "/tmp", "/tmp"]),
            "{shared_tmp:?}"
        );
    }

    #[test]
    fn creates_every_mask_directory_and_marks_it_as_in_use() {
        let directory = TemporaryDirectory::new("bwrap-mask-hold").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let hooks = work.join(".git/hooks");

        let sut = MaskPlan::of(&resolved(&work)).hold();

        assert_eq!(sut.directories, [hooks.clone(), work.join(".git")]);
        assert_eq!(sut.markers.len(), 2, "{sut:?}");
        assert!(sut.markers.iter().all(|marker| marker.is_file()), "{sut:?}");
        assert!(hooks.is_dir());
    }

    #[test]
    fn gives_back_its_mask_directories_when_the_run_ends() {
        let directory = TemporaryDirectory::new("bwrap-mask-release").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();

        drop(MaskPlan::of(&resolved(&work)).hold());

        assert!(!work.join(".git").exists());
        assert!(work.is_dir());
    }

    #[test]
    fn keeps_a_mask_directory_another_run_still_needs() {
        let directory = TemporaryDirectory::new("bwrap-mask-shared").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let held = MaskPlan::of(&resolved(&work)).hold();

        drop(MaskPlan::of(&resolved(&work)).hold());

        assert!(work.join(".git/hooks").is_dir());
        drop(held);
        assert!(!work.join(".git").exists());
    }

    #[test]
    fn plans_no_mask_for_a_deny_that_exists_already() {
        let directory = TemporaryDirectory::new("bwrap-mask-existing").unwrap();
        let work = directory.join("work");
        fs::create_dir_all(work.join(".git/hooks")).unwrap();

        let sut = MaskPlan::of(&resolved(&work));

        assert!(sut.masks.is_empty(), "{sut:?}");
    }

    #[test]
    fn hides_denied_directories_with_a_tmpfs_and_denied_files_with_dev_null() {
        let directory = TemporaryDirectory::new("bwrap-reads").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let keys = directory.join("keys");
        let netrc = directory.join(".netrc");
        fs::create_dir(&keys).unwrap();
        fs::write(&netrc, "secret").unwrap();
        let mut policy = resolved(&work);
        policy.read_deny = vec![keys.clone(), netrc.clone()];
        let bridge = bridge();

        let sut = arguments(&policy, &bridge);

        let keys = keys.to_string_lossy().into_owned();
        let netrc = netrc.to_string_lossy().into_owned();
        assert!(has_run(&sut, &["--tmpfs", &keys]), "{sut:?}");
        assert!(
            has_run(&sut, &["--ro-bind", "/dev/null", &netrc]),
            "{sut:?}"
        );
    }

    #[test]
    fn relays_both_proxy_ports_through_the_bridge_sockets() {
        let directory = TemporaryDirectory::new("bwrap-relays").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let bridge = bridge();

        let sut = arguments(&resolved(&work), &bridge);

        let http = format!("{HTTP_PORT}={}", bridge.http_socket.to_string_lossy());
        let socks = format!("{SOCKS_PORT}={}", bridge.socks_socket.to_string_lossy());
        assert!(has_run(&sut, &["--relay", &http]), "{sut:?}");
        assert!(has_run(&sut, &["--relay", &socks]), "{sut:?}");
    }

    #[test]
    fn passes_one_exec_option_for_each_allowed_executable() {
        let directory = TemporaryDirectory::new("bwrap-executables").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let mut restricted = resolved(&work);
        restricted.executables = Some(vec![PathBuf::from("/bin/bash"), PathBuf::from("/usr/bin")]);
        let bridge = bridge();

        let unrestricted = arguments(&resolved(&work), &bridge);
        let sut = arguments(&restricted, &bridge);

        assert!(!unrestricted.iter().any(|argument| argument == "--exec"));
        assert!(has_run(&sut, &["--exec", "/bin/bash"]), "{sut:?}");
        assert!(has_run(&sut, &["--exec", "/usr/bin"]), "{sut:?}");
    }

    #[test]
    fn ends_with_the_program_and_its_arguments_after_a_separator() {
        let directory = TemporaryDirectory::new("bwrap-program").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let bridge = bridge();

        let sut = arguments(&resolved(&work), &bridge);

        let tail = sut.split_at(sut.len() - 4).1;
        assert_eq!(tail, ["--", "/bin/bash", "-c", "exit 0"]);
        assert!(
            has_run(&sut, &["--", "/usr/local/bin/beez", "sandbox-init"]),
            "{sut:?}"
        );
    }

    #[test]
    fn changes_to_the_working_directory() {
        let directory = TemporaryDirectory::new("bwrap-chdir").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let bridge = bridge();

        let sut = arguments(&resolved(&work), &bridge);

        let work = work.to_string_lossy().into_owned();
        assert!(has_run(&sut, &["--chdir", &work]), "{sut:?}");
    }
}
