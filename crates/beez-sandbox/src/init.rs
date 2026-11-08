//! Entry point that runs inside the Linux sandbox before the program starts.

use std::ffi::OsString;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::proxy::loopback;
use crate::relay::Relay;
use crate::server::{Server, forward};

const RELAY_START_TIMEOUT: Duration = Duration::from_secs(5);

/// Starts the proxy relays, restricts execution, then replaces itself with the program.
///
/// Returns only on failure.
pub fn init(
    relays: &[Relay],
    executables: Option<&[PathBuf]>,
    program: &Path,
    arguments: &[OsString],
) -> io::Result<()> {
    let beez = std::env::current_exe()?;
    for relay in relays {
        let address = loopback(relay.port().get());
        let mut child = Command::new(&beez)
            .arg("sandbox-relay")
            .arg("--listen")
            .arg(address.to_string())
            .arg("--socket")
            .arg(relay.socket())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        await_relay(&mut child, address, RELAY_START_TIMEOUT)?;
    }
    if let Some(executables) = executables {
        restrict_execution(executables)?;
    }
    Err(Command::new(program).args(arguments).exec())
}

/// Waits for the relay `child` to listen on `address`.
///
/// A relay that does not start in time is killed and reaped, so it cannot
/// start listening after `sandbox-init` has given up on it.
fn await_relay(child: &mut Child, address: SocketAddr, timeout: Duration) -> io::Result<()> {
    wait_for_listener(address, timeout).inspect_err(|_| {
        let _ = child.kill();
        let _ = child.wait();
    })
}

/// Names the step that failed, because the cause alone reads as a bare errno.
fn context(step: &str, error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("sandbox {step}: {error}"))
}

fn wait_for_listener(address: SocketAddr, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while TcpStream::connect(address).is_err() {
        if Instant::now() > deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("sandbox relay on {address} did not start"),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// Denies `execve` on every path outside the allowlist, for this process and its children.
fn restrict_execution(executables: &[PathBuf]) -> io::Result<()> {
    use landlock::{
        AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetError, RulesetStatus,
    };

    let rules = executables
        .iter()
        .map(|path| {
            PathFd::new(path)
                .map(|fd| PathBeneath::new(fd, AccessFs::Execute))
                .map_err(|error| {
                    context(&format!("cannot open executable {}", path.display()), error)
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let status = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::Execute)
        .map_err(|error| context("cannot handle execute access", error))?
        .create()
        .map_err(|error| context("cannot create the execute ruleset", error))?
        .add_rules(rules.into_iter().map(Ok::<_, RulesetError>))
        .map_err(|error| context("cannot add the executable rules", error))?
        .restrict_self()
        .map_err(|error| context("cannot apply the execute ruleset", error))?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err(io::Error::other(
            "sandbox cannot restrict executables: the kernel has no Landlock support",
        ));
    }
    Ok(())
}

/// Accepts loopback TCP connections and forwards each to a Unix socket.
///
/// Returns only when the relay stops accepting, which ends the sandbox helper.
pub fn relay(listen: SocketAddr, socket: &Path) -> io::Result<()> {
    serve_relay(TcpListener::bind(listen)?, socket).wait();

    Err(io::Error::other(
        "sandbox relay stopped accepting connections",
    ))
}

/// Forwards every connection `listener` accepts to `socket`.
fn serve_relay(listener: TcpListener, socket: &Path) -> Server {
    let socket = socket.to_path_buf();

    forward(listener, move || UnixStream::connect(&socket))
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Write};
    use std::net::{Ipv4Addr, Shutdown, TcpStream};
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    use beez_test_support::TemporaryDirectory;

    use super::{Server, await_relay, serve_relay, wait_for_listener};
    use crate::proxy::{loopback, loopback_listener};

    #[test]
    fn forwards_a_relayed_connection_to_the_unix_socket() {
        let directory = TemporaryDirectory::new("relay").unwrap();
        let socket = directory.join("http.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let upstream = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            stream.read_to_end(&mut request).unwrap();
            stream.write_all(b"answer").unwrap();
            request
        });
        let (_relay, port) = start_relay(&socket);

        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        client.write_all(b"request").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut answer = Vec::new();
        client.read_to_end(&mut answer).unwrap();

        assert_eq!(upstream.join().unwrap(), b"request");
        assert_eq!(answer, b"answer");
    }

    #[test]
    fn stops_waiting_for_a_listener_that_never_starts() {
        let (listener, port) = loopback_listener().unwrap();
        drop(listener);

        let error = wait_for_listener(loopback(port), Duration::from_millis(50)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn kills_a_relay_that_does_not_start_listening() {
        let (listener, port) = loopback_listener().unwrap();
        drop(listener);
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();

        let error = await_relay(&mut child, loopback(port), Duration::from_millis(50)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(child.try_wait().unwrap().is_some());
    }

    /// Serves `socket` on an ephemeral loopback port, which it returns with
    /// the server that must stay alive while the test runs.
    fn start_relay(socket: &Path) -> (Server, u16) {
        let (listener, port) = loopback_listener().unwrap();

        (serve_relay(listener, socket), port)
    }
}
