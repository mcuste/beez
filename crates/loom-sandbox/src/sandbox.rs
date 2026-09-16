use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::Command;

use loom_policy::{HeadlessHarness, SandboxPolicy};

use crate::proxy::Proxy;
use crate::resolve::ResolvedSandbox;

/// A program wrapped in the platform sandbox, with its proxy kept alive.
#[derive(Debug)]
pub struct SandboxedCommand {
    command: Command,
    _proxy: Proxy,
    #[cfg(target_os = "linux")]
    _bridge: crate::bubblewrap::Bridge,
    // The guard gives the mask directories back once the child has ended.
    #[cfg(target_os = "linux")]
    _masks: crate::bubblewrap::HeldMasks,
}

impl SandboxedCommand {
    /// Prepares `program` to run under `policy` in `working_directory`.
    ///
    /// The sandbox itself is enforced by the operating system: Seatbelt on
    /// macOS and bubblewrap with Landlock on Linux. Other platforms return
    /// `Unsupported`.
    ///
    /// On Linux the sandbox re-executes the running program as its own first
    /// process inside the namespace, so the caller must be the `loom` binary,
    /// which serves the `sandbox-init` and `sandbox-relay` commands.
    pub fn new(
        policy: &SandboxPolicy,
        harness: Option<HeadlessHarness>,
        program: &Path,
        arguments: &[OsString],
        working_directory: &Path,
    ) -> io::Result<Self> {
        let home = std::env::home_dir()
            .ok_or_else(|| io::Error::other("sandbox needs the home directory"))?;
        let resolved =
            ResolvedSandbox::resolve(policy, harness, program, working_directory, &home)?;
        let proxy = Proxy::start(resolved.allowed_domains.clone(), resolved.localhost)?;

        #[cfg(target_os = "macos")]
        {
            let mut command = crate::seatbelt::command(
                &resolved,
                proxy.http_port(),
                proxy.socks_port(),
                arguments,
            );
            configure(
                &mut command,
                &resolved,
                proxy.http_port(),
                proxy.socks_port(),
            );
            Ok(Self {
                command,
                _proxy: proxy,
            })
        }

        #[cfg(target_os = "linux")]
        {
            use crate::bubblewrap::{Bridge, HTTP_PORT, SOCKS_PORT};

            let bridge = Bridge::start(proxy.http_port(), proxy.socks_port())?;
            let (mut command, masks) = crate::bubblewrap::command(&resolved, &bridge, arguments)?;
            configure(&mut command, &resolved, HTTP_PORT, SOCKS_PORT);
            Ok(Self {
                command,
                _proxy: proxy,
                _bridge: bridge,
                _masks: masks,
            })
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (resolved, proxy, arguments);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "sandbox is supported on macOS and Linux only",
            ))
        }
    }

    /// The wrapped command, for adjusting stdio before running it.
    pub fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }
}

/// Points every common HTTP client at the proxies and applies the harness environment.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn configure(command: &mut Command, resolved: &ResolvedSandbox, http_port: u16, socks_port: u16) {
    let http = format!("http://127.0.0.1:{http_port}");
    let socks = format!("socks5h://127.0.0.1:{socks_port}");
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
