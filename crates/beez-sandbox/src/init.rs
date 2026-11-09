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

use crate::init_request::{ExecRestriction, InitRequest};
use crate::proxy::loopback;
use crate::relay::Relay;
use crate::server::{Server, forward};

const RELAY_START_TIMEOUT: Duration = Duration::from_secs(5);

/// Reports why a sandbox helper could not set the sandbox up or keep serving.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    /// Starting a relay, or executing the program, failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The sandboxed program could not be executed.
    #[error("sandbox cannot run {}", .program.display())]
    Exec {
        /// The program the sandbox runs.
        program: PathBuf,
        /// Why `execve` failed.
        #[source]
        source: io::Error,
    },
    /// An allowed executable could not be opened for the Landlock ruleset.
    #[error("sandbox cannot open executable {}", .path.display())]
    OpenExecutable {
        /// The executable the policy allows.
        path: PathBuf,
        /// Why it could not be opened.
        #[source]
        source: landlock::PathFdError,
    },
    /// A step of building or applying the Landlock ruleset failed.
    #[error("sandbox {step}")]
    Landlock {
        /// The step that failed.
        step: &'static str,
        /// What Landlock reported.
        #[source]
        source: landlock::RulesetError,
    },
    /// The kernel cannot enforce the executable allowlist.
    #[error("sandbox cannot restrict executables: the kernel has no Landlock support")]
    NoLandlock,
    /// The relay stopped accepting connections.
    #[error("sandbox relay stopped accepting connections")]
    RelayStopped,
}

/// Starts the proxy relays, restricts execution, then replaces itself with the program.
///
/// Returns only on failure.
pub fn init(request: &InitRequest) -> Result<(), InitError> {
    let beez = std::env::current_exe()?;
    for relay in &request.relays {
        let mut child = Command::new(&beez)
            .arg("sandbox-relay")
            .arg(OsString::from(relay))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        await_relay(
            &mut child,
            loopback(relay.port().get()),
            RELAY_START_TIMEOUT,
        )?;
    }
    if let ExecRestriction::Only(executables) = &request.execution {
        restrict_execution(executables)?;
    }
    let source = Command::new(&request.program)
        .args(&request.arguments)
        .exec();
    Err(InitError::Exec {
        program: request.program.clone(),
        source,
    })
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

/// Names the Landlock step that failed, because the cause alone reads as a bare errno.
fn landlock_step(step: &'static str) -> impl FnOnce(landlock::RulesetError) -> InitError {
    move |source| InitError::Landlock { step, source }
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
fn restrict_execution(executables: &[PathBuf]) -> Result<(), InitError> {
    use landlock::{
        AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetError, RulesetStatus,
    };

    let rules = executables
        .iter()
        .map(|path| {
            PathFd::new(path)
                .map(|fd| PathBeneath::new(fd, AccessFs::Execute))
                .map_err(|source| InitError::OpenExecutable {
                    path: path.clone(),
                    source,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let status = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::Execute)
        .map_err(landlock_step("cannot handle execute access"))?
        .create()
        .map_err(landlock_step("cannot create the execute ruleset"))?
        .add_rules(rules.into_iter().map(Ok::<_, RulesetError>))
        .map_err(landlock_step("cannot add the executable rules"))?
        .restrict_self()
        .map_err(landlock_step("cannot apply the execute ruleset"))?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err(InitError::NoLandlock);
    }
    Ok(())
}

/// Accepts TCP connections on the loopback port of `relay` and forwards each
/// to its Unix socket.
///
/// Returns only when the relay stops accepting, which ends the sandbox helper.
pub fn relay(relay: &Relay) -> Result<(), InitError> {
    serve_relay(
        TcpListener::bind(loopback(relay.port().get()))?,
        relay.socket(),
    )
    .wait();

    Err(InitError::RelayStopped)
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
