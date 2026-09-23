//! Entry point that runs inside the Linux sandbox before the program starts.

use std::ffi::{OsStr, OsString};
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::proxy::loopback;
use crate::server::{Server, forward};

const RELAY_START_TIMEOUT: Duration = Duration::from_secs(5);

/// Starts the proxy relays, restricts execution, then replaces itself with the program.
///
/// Returns only on failure.
pub fn init(
    relays: &[(u16, PathBuf)],
    executables: Option<&[PathBuf]>,
    program: &Path,
    arguments: &[OsString],
) -> io::Result<()> {
    let beez = std::env::current_exe()?;
    for (port, socket) in relays {
        Command::new(&beez)
            .arg("sandbox-relay")
            .arg("--listen")
            .arg(loopback(*port).to_string())
            .arg("--socket")
            .arg(socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        wait_for_listener(loopback(*port), RELAY_START_TIMEOUT)?;
    }
    if let Some(executables) = executables {
        restrict_execution(executables)?;
    }
    Err(Command::new(program).args(arguments).exec())
}

/// The `PORT=SOCKET` argument that asks `sandbox-init` to relay one port.
pub(crate) fn relay_argument(port: u16, socket: &Path) -> OsString {
    let mut argument = OsString::from(format!("{port}="));
    argument.push(socket);
    argument
}

/// Reads a `PORT=SOCKET` relay argument.
pub fn parse_relay(argument: &OsStr) -> io::Result<(u16, PathBuf)> {
    let bytes = argument.as_bytes();
    let separator = bytes
        .iter()
        .position(|byte| *byte == b'=')
        .ok_or_else(|| invalid("relay must be PORT=SOCKET"))?;
    let port = bytes.get(..separator).unwrap_or_default();
    let socket = bytes.get(separator + 1..).unwrap_or_default();
    let port = std::str::from_utf8(port)
        .ok()
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(|| invalid("relay port must be a number from 1 to 65535"))?;
    if socket.is_empty() {
        return Err(invalid("relay must name a socket"));
    }
    Ok((port, PathBuf::from(OsString::from_vec(socket.to_vec()))))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_owned())
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
    use std::ffi::{OsStr, OsString};
    use std::io::{self, Read, Write};
    use std::net::{Ipv4Addr, Shutdown, TcpStream};
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::Duration;

    use beez_test_support::TemporaryDirectory;

    use super::{Server, parse_relay, relay_argument, serve_relay, wait_for_listener};
    use crate::proxy::{loopback, loopback_listener};

    #[test]
    fn reads_back_every_relay_argument_it_writes() {
        for socket in [
            PathBuf::from("/tmp/beez/http.sock"),
            PathBuf::from("/tmp/beez=odd/socks.sock"),
            PathBuf::from(OsString::from_vec(b"/tmp/beez-\xff/http.sock".to_vec())),
        ] {
            let argument = relay_argument(3128, &socket);

            assert_eq!(parse_relay(&argument).unwrap(), (3128, socket));
        }
    }

    #[test]
    fn rejects_relay_arguments_it_cannot_use() {
        for argument in ["3128", "=/tmp/http.sock", "0=/tmp/http.sock", "3128="] {
            let error = parse_relay(OsStr::new(argument)).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{argument}");
        }
    }

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

    /// Serves `socket` on an ephemeral loopback port, which it returns with
    /// the server that must stay alive while the test runs.
    fn start_relay(socket: &Path) -> (Server, u16) {
        let (listener, port) = loopback_listener().unwrap();

        (serve_relay(listener, socket), port)
    }
}
