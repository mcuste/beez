//! Linux backend: bubblewrap namespaces with the proxies bridged over Unix sockets.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::net::TcpStream;
use std::num::NonZeroU16;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::mask_marker::{MARKER_PREFIX, marker_owner};
use crate::proxy::{ProxyPorts, loopback};
use crate::relay::Relay;
use crate::resolve::{Execution, PathSet, ResolvedSandbox};
use crate::sandbox::Wrapped;
use crate::server::{Server, UnixPathListener, forward};

/// Loopback port of the HTTP proxy inside the sandbox.
const HTTP_PORT: NonZeroU16 = NonZeroU16::new(3128).unwrap();
/// Loopback port of the SOCKS5 proxy inside the sandbox.
const SOCKS_PORT: NonZeroU16 = NonZeroU16::new(1080).unwrap();

/// Loopback ports of the proxies inside the sandbox.
const SANDBOX_PORTS: ProxyPorts = ProxyPorts {
    http: HTTP_PORT.get(),
    socks: SOCKS_PORT.get(),
};

static BRIDGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Directories that get an empty tmpfs of their own in every sandbox.
///
/// They hold the host's Unix sockets: the D-Bus buses, the Docker and Podman
/// sockets, SSH and GPG agents, terminal multiplexers, and the bridges of other
/// runs. A read-only mount does not stop a connection to a socket, so these
/// directories must be replaced, not just made read-only.
const PRIVATE_DIRECTORIES: [&str; 2] = ["/tmp", "/run"];

/// Paths under `/run` that hold programs rather than sockets, kept read-only.
///
/// NixOS puts the system profile and its setuid wrappers here, and `PATH`
/// points into them.
const KEPT_RUN_PATHS: [&str; 4] = [
    "/run/current-system",
    "/run/booted-system",
    "/run/wrappers",
    "/run/opengl-driver",
];

/// Unix sockets on the host that forward to the proxies.
///
/// The sandbox has no network namespace of its own, so these sockets are the
/// only way out. They are bind-mounted into the sandbox.
#[derive(Debug)]
struct Bridge {
    // The servers stop before the guard removes the sockets they listen on.
    _servers: [Server; 2],
    directory: SocketDirectory,
    empty: PathBuf,
    http_socket: PathBuf,
    socks_socket: PathBuf,
}

impl Bridge {
    fn start(ports: ProxyPorts) -> io::Result<Self> {
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
        let http = forward_socket(&http_socket, ports.http)?;
        let socks = forward_socket(&socks_socket, ports.socks)?;

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

/// What a run keeps alive until its program has ended.
#[derive(Debug)]
pub(crate) struct Guard {
    // The bridge goes first, then the mask directories are given back.
    _bridge: Bridge,
    _masks: HeldMasks,
}

/// Wraps the program in `bwrap` with the proxies bridged into the sandbox.
pub(crate) fn wrap(
    resolved: &ResolvedSandbox,
    ports: ProxyPorts,
    arguments: &[OsString],
) -> io::Result<Wrapped> {
    let bridge = Bridge::start(ports)?;
    let (command, masks) = command(resolved, &bridge, arguments)?;
    Ok(Wrapped {
        command,
        ports: SANDBOX_PORTS,
        guard: Guard {
            _bridge: bridge,
            _masks: masks,
        },
    })
}

/// Wraps the program in `bwrap`, entered through `beez sandbox-init`.
fn command(
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

const MARK_ATTEMPTS: u32 = 4;

static MARKER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The denied paths that need an empty directory to mask them.
///
/// `bwrap` creates a mount point that is missing, inside a directory bound
/// from the host, so it would stay there after the sandbox ends.
///
/// The plan holds the mask lock from the moment it looks at the paths until
/// [`MaskPlan::hold`] has marked the directories it needs.
#[derive(Debug)]
struct MaskPlan {
    /// Each masked path with the directories it needs, deepest first.
    masks: Vec<(PathBuf, Vec<PathBuf>)>,
    /// The writable directories whose lock this plan holds.
    roots: Vec<PathBuf>,
    _lock: MaskLock,
}

impl MaskPlan {
    fn of(resolved: &ResolvedSandbox) -> Self {
        let roots = resolved
            .write_allow
            .iter()
            .filter(|root| {
                resolved
                    .write_deny
                    .iter()
                    .any(|path| path.starts_with(root) && path != *root)
            })
            .cloned()
            .collect::<Vec<_>>();
        let lock = MaskLock::acquire(&roots);
        let masks = resolved
            .write_deny
            .iter()
            .filter(|path| resolved.inside_write_allow(path))
            .map(|path| (path.clone(), mask_directories(path)))
            .filter(|(_, directories)| !directories.is_empty())
            .collect();

        Self {
            masks,
            roots,
            _lock: lock,
        }
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
            roots: self.roots,
        }
    }
}

/// Keeps the mask work of runs that share a writable directory apart.
///
/// A mask directory that exists without a marker looks like real content. A
/// run that plans its masks while another run is between creating a directory
/// and marking it, or between removing its marker and the directory, would
/// bind that directory onto itself and fail when it goes away. So planning,
/// marking, and giving masks back happen under an exclusive `flock` on every
/// writable directory that holds a denied path. The lock is taken on the
/// directory itself, so it leaves no file behind, and it is let go before the
/// task starts, so sandboxed tasks still run side by side.
///
/// Runs take the locks in path order, so no two runs wait on each other in a
/// cycle. A directory that cannot be opened or locked stays unlocked.
#[derive(Debug)]
struct MaskLock {
    /// The locked directories; closing them lets the locks go.
    _directories: Vec<File>,
}

impl MaskLock {
    fn acquire(roots: &[PathBuf]) -> Self {
        let directories = roots
            .iter()
            .filter_map(|root| {
                let directory = File::open(root).ok()?;
                directory.lock().ok()?;
                Some(directory)
            })
            .collect();
        Self {
            _directories: directories,
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
        // A marker without a process ID belongs to no live run.
        let live = marker
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(marker_owner)
            .is_some_and(|owner| Path::new("/proc").join(owner.to_string()).exists());
        if !live {
            let _ = fs::remove_file(&marker);
        }
    }
}

/// Gives back the mask directories of one run when it ends.
#[derive(Debug)]
struct HeldMasks {
    markers: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    /// The writable directories to lock while giving the masks back.
    roots: Vec<PathBuf>,
}

impl Drop for HeldMasks {
    fn drop(&mut self) {
        let _lock = MaskLock::acquire(&self.roots);
        for marker in &self.markers {
            let _ = fs::remove_file(marker);
        }
        for directory in &self.directories {
            remove_dead_markers(directory);
            // Only an empty directory goes away: another live run's marker,
            // or any real content, keeps it.
            let _ = fs::remove_dir(directory);
        }
    }
}

/// Every `bwrap` argument, ending with the program to run inside the sandbox.
///
/// bubblewrap applies its mounts in order, and a mount covers whatever an
/// earlier one put at the same path or below it. So the layers come in this
/// order: the empty private directories first, then the paths the task may
/// write, which may sit inside them, then the denies, which must win over the
/// writes, and the bridge last, so that nothing covers it.
fn bwrap_arguments(
    resolved: &ResolvedSandbox,
    bridge: &Bridge,
    beez: &Path,
    program_arguments: &[OsString],
    plan: &MaskPlan,
) -> Vec<OsString> {
    Bwrap::isolated()
        .private_directories()
        .writable_paths(resolved)
        .write_denies(resolved, bridge.empty_directory(), plan)
        .read_denies(&resolved.read_deny)
        .bind(bridge.directory.path())
        .chdir(&resolved.working_directory)
        .run(sandbox_init(resolved, bridge, beez, program_arguments))
}

/// The command `bwrap` runs: `beez sandbox-init`, which starts the proxy
/// relays and limits the programs, then runs the task's program.
///
/// The arguments after `sandbox-init` are read back by Beez's own
/// `sandbox-init` command, so their names must match its options.
fn sandbox_init(
    resolved: &ResolvedSandbox,
    bridge: &Bridge,
    beez: &Path,
    program_arguments: &[OsString],
) -> Vec<OsString> {
    let mut command = vec![beez.into(), OsString::from("sandbox-init")];
    for relay in [
        Relay::new(HTTP_PORT, bridge.http_socket.clone()),
        Relay::new(SOCKS_PORT, bridge.socks_socket.clone()),
    ] {
        command.push(OsString::from("--relay"));
        command.push(OsString::from(&relay));
    }
    match &resolved.execution {
        Execution::Unrestricted => command.push(OsString::from("--unrestricted-exec")),
        Execution::Only(allowlist) => {
            for path in allowlist {
                command.push(OsString::from("--exec"));
                command.push(path.into());
            }
        }
    }
    command.push(OsString::from("--"));
    command.push(resolved.program().into());
    command.extend(program_arguments.iter().cloned());
    command
}

/// A `bwrap` command line, built one mount at a time.
#[derive(Debug, Default)]
#[must_use]
struct Bwrap {
    arguments: Vec<OsString>,
}

impl Bwrap {
    /// New namespaces with the host's root mounted read-only.
    ///
    /// The task gets no network and sees only its own processes.
    fn isolated() -> Self {
        Self::default()
            .flags(&[
                "--new-session",
                "--die-with-parent",
                "--unshare-net",
                "--unshare-pid",
            ])
            .ro_bind(Path::new("/"), Path::new("/"))
            .option("--dev", Path::new("/dev"))
            .option("--proc", Path::new("/proc"))
    }

    /// Replaces the directories that hold the host's Unix sockets with empty ones.
    fn private_directories(mut self) -> Self {
        for directory in PRIVATE_DIRECTORIES {
            self = self.tmpfs(Path::new(directory));
        }
        // Old systems keep /var/run as a directory of its own instead of a link.
        let var_run = Path::new("/var/run");
        if fs::symlink_metadata(var_run).is_ok_and(|metadata| metadata.is_dir()) {
            self = self.tmpfs(var_run);
        }
        for path in KEPT_RUN_PATHS {
            self = self.keep(Path::new(path));
        }
        self
    }

    /// Mounts the paths the task may write, and the program when it lives in
    /// a private directory.
    fn writable_paths(mut self, resolved: &ResolvedSandbox) -> Self {
        // The private /tmp stands in for the host's, so a grant to write it
        // binds nothing. Paths inside it, such as a working directory, still bind.
        let bound = resolved
            .write_allow
            .iter()
            .filter(|path| !PRIVATE_DIRECTORIES.map(Path::new).contains(&path.as_path()))
            .collect::<Vec<_>>();
        for path in &bound {
            self = self.bind(path);
        }
        let program = resolved.program();
        if PRIVATE_DIRECTORIES
            .iter()
            .any(|directory| program.starts_with(directory))
            && !bound.iter().any(|path| program.starts_with(path))
        {
            self = self.ro_bind(program, program);
        }
        self
    }

    /// Keeps denied paths unwritable, even inside a writable directory.
    fn write_denies(mut self, resolved: &ResolvedSandbox, empty: &Path, plan: &MaskPlan) -> Self {
        // A parent bound onto itself becomes a mount point, and the kernel
        // refuses to rename a mount point. Without it a task moves the parent
        // aside and the deny mount goes with it. This bind must come before
        // the denies inside it, and a parent that does not exist yet cannot be
        // bound.
        for path in resolved.protected_ancestors() {
            if path.exists() {
                self = self.bind(&path);
            }
        }
        for path in &resolved.write_deny {
            if plan.covers(path) {
                self = self.ro_bind(empty, path);
            } else if path.exists() {
                self = self.ro_bind(path, path);
            }
            // A path outside every writable directory needs no mount.
        }
        self
    }

    /// Hides denied directories behind an empty tmpfs and denied files behind
    /// `/dev/null`.
    fn read_denies(mut self, paths: &PathSet) -> Self {
        for path in paths {
            self = if path.is_dir() {
                self.tmpfs(path)
            } else {
                self.ro_bind(Path::new("/dev/null"), path)
            };
        }
        self
    }

    /// Puts a host path back read-only, as a link when it is one.
    fn keep(self, path: &Path) -> Self {
        if let Ok(target) = fs::read_link(path) {
            self.mount("--symlink", &target, path)
        } else if path.exists() {
            self.ro_bind(path, path)
        } else {
            self
        }
    }

    fn tmpfs(self, path: &Path) -> Self {
        self.option("--tmpfs", path)
    }

    /// Mounts a host path writable at the same place.
    fn bind(self, path: &Path) -> Self {
        self.mount("--bind", path, path)
    }

    fn ro_bind(self, source: &Path, target: &Path) -> Self {
        self.mount("--ro-bind", source, target)
    }

    fn chdir(self, path: &Path) -> Self {
        self.option("--chdir", path)
    }

    /// Ends the options with the command to run inside the sandbox.
    fn run(mut self, command: Vec<OsString>) -> Vec<OsString> {
        self.arguments.push(OsString::from("--"));
        self.arguments.extend(command);
        self.arguments
    }

    fn mount(mut self, kind: &str, source: &Path, target: &Path) -> Self {
        self.arguments
            .extend([OsString::from(kind), source.into(), target.into()]);
        self
    }

    fn option(mut self, name: &str, value: &Path) -> Self {
        self.arguments.extend([OsString::from(name), value.into()]);
        self
    }

    fn flags(mut self, names: &[&str]) -> Self {
        self.arguments.extend(names.iter().map(OsString::from));
        self
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};

    use beez_test_support::{TemporaryDirectory, text};

    use super::{
        Bridge, Bwrap, MARKER_PREFIX, MaskPlan, SANDBOX_PORTS, bwrap_arguments, remove_dead_markers,
    };
    use crate::proxy::ProxyPorts;
    use crate::resolve::{ExecAllowlist, Execution, PathSet, ResolvedSandbox};

    /// A policy that may write `work` and must not write `.git/hooks` in it.
    fn resolved(work: &Path) -> ResolvedSandbox {
        ResolvedSandbox {
            allowed_domains: Vec::new(),
            localhost: false,
            read_deny: PathSet::default(),
            write_allow: PathSet::from([work.to_path_buf()]),
            write_deny: PathSet::from([work.join(".git/hooks")]),
            execution: Execution::Unrestricted,
            environment: Vec::new(),
            working_directory: work.to_path_buf(),
            program: PathBuf::from("/bin/bash"),
        }
    }

    fn bridge() -> Bridge {
        Bridge::start(ProxyPorts { http: 0, socks: 0 }).unwrap()
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

        let work = text(&work);
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

        let empty = text(bridge.empty_directory());
        let hooks = format!("{}/.git/hooks", text(&work));
        assert!(has_run(&sut, &["--ro-bind", &empty, &hooks]), "{sut:?}");
    }

    #[test]
    fn leaves_a_denied_path_outside_every_writable_directory_unmounted() {
        let directory = TemporaryDirectory::new("bwrap-unwritable-deny").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let mut policy = resolved(&work);
        policy.write_deny = PathSet::from([directory.join("elsewhere/.claude")]);
        let bridge = bridge();

        let sut = arguments(&policy, &bridge);

        let denied = directory.join("elsewhere/.claude");
        let denied = text(&denied);
        assert!(!sut.contains(&denied), "{sut:?}");
    }

    #[test]
    fn gives_every_sandbox_a_private_tmp_and_run_even_when_tmp_is_writable() {
        let directory = TemporaryDirectory::new("bwrap-tmp").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let mut writable_tmp = resolved(&work);
        writable_tmp.write_allow.extend([PathBuf::from("/tmp")]);
        let bridge = bridge();

        let sut = arguments(&writable_tmp, &bridge);

        assert!(has_run(&sut, &["--tmpfs", "/tmp"]), "{sut:?}");
        assert!(has_run(&sut, &["--tmpfs", "/run"]), "{sut:?}");
        assert!(!has_run(&sut, &["--bind", "/tmp", "/tmp"]), "{sut:?}");
    }

    #[test]
    fn keeps_a_program_in_the_host_tmp_visible() {
        let directory = TemporaryDirectory::new("bwrap-tmp-program").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let mut policy = resolved(&work);
        policy.write_allow.extend([PathBuf::from("/tmp")]);
        policy.program = PathBuf::from("/tmp/tools/claude");
        let bridge = bridge();

        let sut = arguments(&policy, &bridge);

        assert!(
            has_run(
                &sut,
                &["--ro-bind", "/tmp/tools/claude", "/tmp/tools/claude"]
            ),
            "{sut:?}"
        );
    }

    #[test]
    fn mounts_the_private_directories_before_anything_inside_them() {
        let directory = TemporaryDirectory::new("bwrap-tmp-order").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let bridge = bridge();

        let sut = arguments(&resolved(&work), &bridge);

        let tmp = run_at(&sut, &["--tmpfs", "/tmp"]).unwrap();
        let work = text(&work);
        let bridge_directory = text(bridge.directory.path());
        let work_bind = run_at(&sut, &["--bind", &work, &work]).unwrap();
        let bridge_bind = run_at(&sut, &["--bind", &bridge_directory, &bridge_directory]).unwrap();
        assert!(tmp < work_bind && tmp < bridge_bind, "{sut:?}");
    }

    #[test]
    fn keeps_a_run_path_as_a_link_or_a_read_only_bind() {
        let directory = TemporaryDirectory::new("bwrap-keep").unwrap();
        let program_directory = directory.join("wrappers");
        fs::create_dir(&program_directory).unwrap();
        let link = directory.join("current-system");
        std::os::unix::fs::symlink("/nix/store/system", &link).unwrap();

        let sut = Bwrap::default()
            .keep(&program_directory)
            .keep(&link)
            .keep(&directory.join("missing"))
            .arguments;

        let program_directory = text(&program_directory);
        let link = text(&link);
        assert_eq!(
            sut,
            [
                "--ro-bind",
                program_directory.as_str(),
                program_directory.as_str(),
                "--symlink",
                "/nix/store/system",
                link.as_str(),
            ]
            .map(OsString::from)
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
    fn masks_every_deny_while_other_runs_hold_and_give_back_the_same_masks() {
        let directory = TemporaryDirectory::new("bwrap-mask-concurrent").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let mut policy = resolved(&work);
        policy.write_deny = (0..20)
            .map(|index| work.join(format!(".deny-{index}/inner")))
            .collect();

        // Nothing in `work` is real content, so a deny that a run does not
        // mask would be bound onto itself, and a mask directory another run
        // removes before the sandbox starts makes `bwrap` fail.
        let unmasked = std::thread::scope(|scope| {
            let runs = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        (0..200)
                            .filter(|_| {
                                let plan = MaskPlan::of(&policy);
                                let unmasked =
                                    policy.write_deny.iter().any(|path| !plan.covers(path));
                                drop(plan.hold());
                                unmasked
                            })
                            .count()
                    })
                })
                .collect::<Vec<_>>();
            runs.into_iter()
                .map(|run| run.join().unwrap())
                .sum::<usize>()
        });

        assert_eq!(unmasked, 0);
        assert_eq!(fs::read_dir(&work).unwrap().count(), 0);
    }

    #[test]
    fn removes_markers_of_ended_runs_and_markers_without_an_owner() {
        let directory = TemporaryDirectory::new("bwrap-dead-markers").unwrap();
        let live = directory.join(&format!("{MARKER_PREFIX}{}-0", std::process::id()));
        let nameless = directory.join(&format!("{MARKER_PREFIX}-0"));
        let empty = directory.join(MARKER_PREFIX);
        for marker in [&live, &nameless, &empty] {
            fs::write(marker, b"").unwrap();
        }

        remove_dead_markers(directory.path());

        assert!(live.exists());
        assert!(!nameless.exists());
        assert!(!empty.exists());
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
        policy.read_deny = PathSet::from([keys.clone(), netrc.clone()]);
        let bridge = bridge();

        let sut = arguments(&policy, &bridge);

        let keys = text(&keys);
        let netrc = text(&netrc);
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

        let http = format!("{}={}", SANDBOX_PORTS.http, text(&bridge.http_socket));
        let socks = format!("{}={}", SANDBOX_PORTS.socks, text(&bridge.socks_socket));
        assert!(has_run(&sut, &["--relay", &http]), "{sut:?}");
        assert!(has_run(&sut, &["--relay", &socks]), "{sut:?}");
    }

    #[test]
    fn passes_one_exec_option_for_each_allowed_executable() {
        let directory = TemporaryDirectory::new("bwrap-executables").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let mut restricted = resolved(&work);
        restricted.execution = Execution::Only(ExecAllowlist::new(
            PathBuf::from("/bin/bash"),
            [PathBuf::from("/usr/bin")],
        ));
        let bridge = bridge();

        let unrestricted = arguments(&resolved(&work), &bridge);
        let sut = arguments(&restricted, &bridge);

        assert!(!unrestricted.iter().any(|argument| argument == "--exec"));
        assert!(
            unrestricted
                .iter()
                .any(|argument| argument == "--unrestricted-exec")
        );
        assert!(!sut.iter().any(|argument| argument == "--unrestricted-exec"));
        assert!(has_run(&sut, &["--exec", "/bin/bash"]), "{sut:?}");
        assert!(has_run(&sut, &["--exec", "/usr/bin"]), "{sut:?}");
    }

    #[test]
    fn always_passes_an_exec_option_when_execution_is_restricted() {
        let directory = TemporaryDirectory::new("bwrap-exec-program").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let mut restricted = resolved(&work);
        restricted.execution = Execution::Only(ExecAllowlist::new(PathBuf::from("/bin/bash"), []));
        let bridge = bridge();

        let sut = arguments(&restricted, &bridge);

        let init = run_at(&sut, &["/usr/local/bin/beez", "sandbox-init"]).unwrap();
        let program = run_at(&sut, &["--", "/bin/bash", "-c"]).unwrap();
        let exec = run_at(&sut, &["--exec", "/bin/bash"]).unwrap();
        assert!(init < exec && exec < program, "{sut:?}");
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
    fn mounts_nested_paths_parents_first_and_denies_after_writes() {
        let directory = TemporaryDirectory::new("bwrap-nested-order").unwrap();
        let work = directory.join("work");
        let sub = work.join("sub");
        for path in [work.join("b"), sub.join("a/hooks"), work.join("keys")] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(sub.join(".netrc"), "secret").unwrap();
        let mut policy = resolved(&work);
        policy.write_allow = PathSet::from([work.clone(), sub.clone()]);
        policy.write_deny = PathSet::from([work.join("b"), sub.join("a/hooks")]);
        policy.read_deny = PathSet::from([work.join("keys"), sub.join(".netrc")]);
        let bridge = bridge();

        let sut = arguments(&policy, &bridge);

        let [work, sub, a, b, hooks, keys, netrc] = [
            work.clone(),
            sub.clone(),
            sub.join("a"),
            work.join("b"),
            sub.join("a/hooks"),
            work.join("keys"),
            sub.join(".netrc"),
        ]
        .map(|path| text(&path));
        let expected = [
            "--bind",
            &work,
            &work,
            "--bind",
            &sub,
            &sub,
            "--bind",
            &sub,
            &sub,
            "--bind",
            &a,
            &a,
            "--ro-bind",
            &b,
            &b,
            "--ro-bind",
            &hooks,
            &hooks,
            "--tmpfs",
            &keys,
            "--ro-bind",
            "/dev/null",
            &netrc,
        ];
        assert!(has_run(&sut, &expected), "{sut:?}");
    }

    #[test]
    fn changes_to_the_working_directory() {
        let directory = TemporaryDirectory::new("bwrap-chdir").unwrap();
        let work = directory.join("work");
        fs::create_dir(&work).unwrap();
        let bridge = bridge();

        let sut = arguments(&resolved(&work), &bridge);

        let work = text(&work);
        assert!(has_run(&sut, &["--chdir", &work]), "{sut:?}");
    }
}
