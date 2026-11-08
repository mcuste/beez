//! Hidden helper commands Beez runs inside its own Linux sandbox.

use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct SandboxInit {
    /// Loopback port and Unix socket pair to bridge, as PORT=SOCKET.
    #[arg(long = "relay", value_name = "PORT=SOCKET")]
    relays: Vec<OsString>,
    /// File or directory that may execute. Required unless `--unrestricted-exec`.
    #[arg(
        long = "exec",
        value_name = "PATH",
        required_unless_present = "unrestricted_exec"
    )]
    executables: Vec<PathBuf>,
    /// Lets every program execute, in place of `--exec`.
    #[arg(long, conflicts_with = "executables")]
    unrestricted_exec: bool,
    /// Program and arguments to replace this process with.
    #[arg(last = true, required = true)]
    command: Vec<OsString>,
}

#[derive(Debug, Args)]
pub(crate) struct SandboxRelay {
    /// Loopback address to listen on.
    #[arg(long)]
    listen: SocketAddr,
    /// Unix socket that receives each connection.
    #[arg(long)]
    socket: PathBuf,
}

/// Sets the sandbox up from inside, then becomes the sandboxed program.
#[cfg(target_os = "linux")]
pub(crate) fn init(init: &SandboxInit) -> io::Result<i32> {
    let relays = init
        .relays
        .iter()
        .map(|relay| beez_sandbox::Relay::try_from(relay.as_os_str()))
        .collect::<io::Result<Vec<_>>>()?;
    let executables = (!init.unrestricted_exec).then_some(init.executables.as_slice());
    let Some((program, arguments)) = init.command.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sandbox-init needs a program",
        ));
    };
    beez_sandbox::init(&relays, executables, program.as_ref(), arguments)?;
    Ok(0)
}

/// Sets the sandbox up from inside, then becomes the sandboxed program.
#[cfg(not(target_os = "linux"))]
pub(crate) fn init(_init: &SandboxInit) -> io::Result<i32> {
    Err(unsupported())
}

/// Forwards loopback connections to a Unix socket.
#[cfg(target_os = "linux")]
pub(crate) fn relay(relay: &SandboxRelay) -> io::Result<i32> {
    beez_sandbox::relay(relay.listen, &relay.socket)?;
    Ok(0)
}

/// Forwards loopback connections to a Unix socket.
#[cfg(not(target_os = "linux"))]
pub(crate) fn relay(_relay: &SandboxRelay) -> io::Result<i32> {
    Err(unsupported())
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "sandbox helper commands run only inside a Linux sandbox",
    )
}
