//! Linux backend: bubblewrap namespaces with the proxies bridged over Unix sockets.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::init::relay_argument;
use crate::proxy::loopback;
use crate::resolve::ResolvedSandbox;
use crate::server::{Server, UnixPathListener};
use crate::stream::pipe;

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
            std::env::temp_dir().join(format!("loom-sandbox-{}-{sequence}", std::process::id()));
        fs::create_dir(&path)?;
        let directory = SocketDirectory(path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        }
        let empty = directory.path().join("empty");
        fs::create_dir(&empty)?;
        let http_socket = directory.path().join("http.sock");
        let socks_socket = directory.path().join("socks.sock");
        let http = forward(&http_socket, http_port)?;
        let socks = forward(&socks_socket, socks_port)?;

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

fn forward(socket: &Path, port: u16) -> io::Result<Server> {
    let listener = UnixPathListener::bind(socket.to_path_buf())?;
    Ok(Server::spawn(listener, move |client| {
        if let Ok(upstream) = TcpStream::connect(loopback(port)) {
            let _ = pipe(client, upstream);
        }
    }))
}

/// Wraps the program in `bwrap`, entered through `loom sandbox-init`.
pub(crate) fn command(
    resolved: &ResolvedSandbox,
    bridge: &Bridge,
    arguments: &[OsString],
) -> io::Result<Command> {
    let bwrap = crate::resolve::find_on_path(Path::new("bwrap")).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "sandbox requires bubblewrap; install the bwrap package",
        )
    })?;
    let loom = std::env::current_exe()?;

    let mut command = Command::new(bwrap);
    command.args(bwrap_arguments(resolved, bridge, &loom, arguments));
    Ok(command)
}

/// Every `bwrap` argument, ending with the program to run inside the sandbox.
///
/// The arguments after `sandbox-init` are read back by Loom's own
/// `sandbox-init` command, so their names must match its options.
fn bwrap_arguments(
    resolved: &ResolvedSandbox,
    bridge: &Bridge,
    loom: &Path,
    program_arguments: &[OsString],
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
    for path in &resolved.write_deny {
        if path.exists() {
            bind(&mut arguments, "--ro-bind", path, path);
        } else if inside_write_allow(resolved, path) {
            // A mount cannot mask a path that does not exist, so an empty
            // read-only directory takes its place. A deny outside every
            // writable directory needs no mount.
            bind(&mut arguments, "--ro-bind", bridge.empty_directory(), path);
        }
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
    arguments.push(loom.into());
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

/// True when a writable directory contains `path`, so a mount must mask it.
fn inside_write_allow(resolved: &ResolvedSandbox, path: &Path) -> bool {
    resolved
        .write_allow
        .iter()
        .any(|allowed| path.starts_with(allowed) && path != allowed.as_path())
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

    use loom_test_support::TemporaryDirectory;

    use super::{Bridge, HTTP_PORT, SOCKS_PORT, bwrap_arguments};
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
            Path::new("/usr/local/bin/loom"),
            &[OsString::from("-c"), OsString::from("exit 0")],
        )
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
    }

    /// True when `expected` appears as consecutive arguments.
    fn has_run(arguments: &[String], expected: &[&str]) -> bool {
        arguments
            .windows(expected.len())
            .any(|window| window.iter().zip(expected).all(|(one, two)| one == two))
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
            has_run(&sut, &["--", "/usr/local/bin/loom", "sandbox-init"]),
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
