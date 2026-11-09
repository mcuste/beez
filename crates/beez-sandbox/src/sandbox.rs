use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::Command;

use beez_policy::{HeadlessHarness, SandboxPolicy};

#[cfg(target_os = "linux")]
use crate::bubblewrap as backend;
use crate::proxy::{Proxy, ProxyPorts};
use crate::resolve::ResolvedSandbox;
#[cfg(target_os = "macos")]
use crate::seatbelt as backend;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use unsupported as backend;

/// Variables that point a program at a host socket the sandbox hides or refuses.
///
/// The sandbox stops the connection either way. Removing them makes a tool
/// report that no agent or bus is there, instead of a permission error.
const HOST_SOCKET_VARIABLES: [&str; 4] = [
    "SSH_AUTH_SOCK",
    "GPG_AGENT_INFO",
    "DBUS_SESSION_BUS_ADDRESS",
    "DBUS_SYSTEM_BUS_ADDRESS",
];

/// A program wrapped by a sandbox backend, before the proxy settings go in.
#[derive(Debug)]
pub(crate) struct Wrapped {
    /// The command that runs the program inside the sandbox.
    pub(crate) command: Command,
    /// The proxy ports as the program inside the sandbox reaches them.
    pub(crate) ports: ProxyPorts,
    /// What the backend keeps alive until the program has ended.
    pub(crate) guard: backend::Guard,
}

/// Reports why a program could not be wrapped in the platform sandbox.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The sandbox resolves `~` paths, but the home directory is unknown.
    #[error("sandbox needs the home directory")]
    NoHome,
    /// Resolving the policy, starting the proxy, or wrapping the program failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A program wrapped in the platform sandbox, with its proxy kept alive.
#[derive(Debug)]
pub struct SandboxedCommand {
    command: Command,
    _proxy: Proxy,
    // Dropped last, once the child has ended. On Linux it stops the bridge
    // before removing its sockets, then gives the mask directories back.
    _guard: backend::Guard,
}

impl SandboxedCommand {
    /// Prepares `program` to run under `policy` in `working_directory`.
    ///
    /// The sandbox itself is enforced by the operating system: Seatbelt on
    /// macOS and bubblewrap with Landlock on Linux. Other platforms return
    /// `Unsupported`.
    ///
    /// On Linux the sandbox re-executes the running program as its own first
    /// process inside the namespace, so the caller must be the `beez` binary,
    /// which serves the `sandbox-init` and `sandbox-relay` commands.
    pub fn new(
        policy: &SandboxPolicy,
        harness: Option<HeadlessHarness>,
        program: &Path,
        arguments: &[OsString],
        working_directory: &Path,
    ) -> Result<Self, SandboxError> {
        let home = std::env::home_dir().ok_or(SandboxError::NoHome)?;
        let resolved =
            ResolvedSandbox::resolve(policy, harness, program, working_directory, &home)?;
        resolved.trace();
        let proxy = Proxy::start(resolved.network.clone())?;

        let Wrapped {
            mut command,
            ports,
            guard,
        } = backend::wrap(&resolved, proxy.ports(), arguments)?;
        configure(&mut command, &resolved, ports);
        Ok(Self {
            command,
            _proxy: proxy,
            _guard: guard,
        })
    }

    /// The wrapped command, for adjusting stdio before running it.
    pub fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }
}

/// Points every common HTTP client at the proxies and applies the harness environment.
fn configure(command: &mut Command, resolved: &ResolvedSandbox, ports: ProxyPorts) {
    let http = format!("http://127.0.0.1:{}", ports.http);
    let socks = format!("socks5h://127.0.0.1:{}", ports.socks);
    for name in HOST_SOCKET_VARIABLES {
        command.env_remove(name);
    }
    command
        .current_dir(&resolved.working_directory)
        .env("HTTP_PROXY", &http)
        .env("http_proxy", &http)
        .env("HTTPS_PROXY", &http)
        .env("https_proxy", &http)
        .env("ALL_PROXY", &socks)
        .env("all_proxy", &socks)
        .env("NO_PROXY", "localhost,127.0.0.1,::1")
        .env("no_proxy", "localhost,127.0.0.1,::1")
        .env("NODE_USE_ENV_PROXY", "1")
        .envs(
            resolved
                .environment
                .iter()
                .map(|(name, value)| (name, value)),
        );
}

/// The backend of platforms without a sandbox.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod unsupported {
    use std::ffi::OsString;
    use std::io;

    use super::Wrapped;
    use crate::proxy::ProxyPorts;
    use crate::resolve::ResolvedSandbox;

    /// Nothing to keep alive.
    pub(crate) type Guard = ();

    pub(crate) fn wrap(
        _resolved: &ResolvedSandbox,
        _ports: ProxyPorts,
        _arguments: &[OsString],
    ) -> io::Result<Wrapped> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "sandbox is supported on macOS and Linux only",
        ))
    }
}
