//! Hidden helper commands Beez runs inside its own Linux sandbox.

use std::ffi::OsString;
use std::path::PathBuf;

use beez_sandbox::{EXEC_FLAG, RELAY_FLAG, Relay, UNRESTRICTED_EXEC_FLAG};
#[cfg(any(target_os = "linux", test))]
use beez_sandbox::{ExecRestriction, InitRequest};
use clap::Args;
use clap::builder::{OsStringValueParser, TypedValueParser};

#[derive(Debug, Args)]
pub(crate) struct SandboxInit {
    /// Loopback port and Unix socket pair to bridge, as PORT=SOCKET.
    #[arg(long = RELAY_FLAG, value_name = "PORT=SOCKET", value_parser = relay_parser())]
    relays: Vec<Relay>,
    /// File or directory that may execute. Required unless `--unrestricted-exec`.
    #[arg(
        long = EXEC_FLAG,
        value_name = "PATH",
        required_unless_present = "unrestricted_exec"
    )]
    executables: Vec<PathBuf>,
    /// Lets every program execute, in place of `--exec`.
    #[arg(long = UNRESTRICTED_EXEC_FLAG, conflicts_with = "executables")]
    unrestricted_exec: bool,
    /// Program and arguments to replace this process with.
    #[arg(last = true, required = true)]
    command: Vec<OsString>,
}

impl SandboxInit {
    /// The setup the arguments ask for.
    #[cfg(any(target_os = "linux", test))]
    fn request(&self) -> anyhow::Result<InitRequest> {
        let Some((program, arguments)) = self.command.split_first() else {
            anyhow::bail!("sandbox-init needs a program");
        };
        let execution = if self.unrestricted_exec {
            ExecRestriction::Unrestricted
        } else {
            ExecRestriction::Only(self.executables.clone())
        };
        Ok(InitRequest {
            relays: self.relays.clone(),
            execution,
            program: PathBuf::from(program),
            arguments: arguments.to_vec(),
        })
    }
}

#[derive(Debug, Args)]
pub(crate) struct SandboxRelay {
    /// Loopback port to listen on and Unix socket that receives each
    /// connection, as PORT=SOCKET.
    #[arg(value_name = "PORT=SOCKET", value_parser = relay_parser())]
    relay: Relay,
}

/// Reads a `PORT=SOCKET` argument, which need not be UTF-8.
fn relay_parser() -> impl TypedValueParser<Value = Relay> {
    OsStringValueParser::new().try_map(|argument| Relay::try_from(argument.as_os_str()))
}

/// Sets the sandbox up from inside, then becomes the sandboxed program.
#[cfg(target_os = "linux")]
pub(crate) fn init(init: &SandboxInit) -> anyhow::Result<i32> {
    beez_sandbox::init(&init.request()?)?;
    Ok(0)
}

/// Sets the sandbox up from inside, then becomes the sandboxed program.
#[cfg(not(target_os = "linux"))]
pub(crate) fn init(_init: &SandboxInit) -> anyhow::Result<i32> {
    unsupported()
}

/// Forwards loopback connections to a Unix socket.
#[cfg(target_os = "linux")]
pub(crate) fn relay(relay: &SandboxRelay) -> anyhow::Result<i32> {
    beez_sandbox::relay(&relay.relay)?;
    Ok(0)
}

/// Forwards loopback connections to a Unix socket.
#[cfg(not(target_os = "linux"))]
pub(crate) fn relay(_relay: &SandboxRelay) -> anyhow::Result<i32> {
    unsupported()
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> anyhow::Result<i32> {
    anyhow::bail!("sandbox helper commands run only inside a Linux sandbox")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::num::NonZeroU16;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    use beez_sandbox::{ExecRestriction, InitRequest, Relay};
    use clap::Parser;

    use super::{SandboxInit, SandboxRelay};

    #[derive(Debug, Parser)]
    struct InitCommand {
        #[command(flatten)]
        init: SandboxInit,
    }

    #[derive(Debug, Parser)]
    struct RelayCommand {
        #[command(flatten)]
        relay: SandboxRelay,
    }

    fn relay(port: u16, socket: &str) -> Relay {
        Relay::new(NonZeroU16::new(port).unwrap(), PathBuf::from(socket))
    }

    /// The request `sandbox-init` reads from the arguments `request` writes.
    fn read_back(request: &InitRequest) -> InitRequest {
        let arguments =
            std::iter::once(OsString::from("sandbox-init")).chain(request.to_arguments());

        InitCommand::try_parse_from(arguments)
            .unwrap()
            .init
            .request()
            .unwrap()
    }

    #[test]
    fn reads_back_every_request_the_sandbox_writes() {
        for execution in [
            ExecRestriction::Unrestricted,
            ExecRestriction::Only(vec![PathBuf::from("/bin/bash"), PathBuf::from("/usr/bin")]),
        ] {
            let request = InitRequest {
                relays: vec![
                    relay(3128, "/tmp/beez/http.sock"),
                    relay(1080, "/tmp/beez=odd/socks.sock"),
                ],
                execution,
                program: PathBuf::from("/bin/bash"),
                arguments: vec![
                    OsString::from("-c"),
                    OsString::from("--exec"),
                    OsString::from_vec(b"\xff".to_vec()),
                ],
            };

            assert_eq!(read_back(&request), request);
        }
    }

    #[test]
    fn rejects_a_relay_it_cannot_use() {
        let error = InitCommand::try_parse_from([
            "sandbox-init",
            "--relay",
            "0=/tmp/http.sock",
            "--unrestricted-exec",
            "--",
            "/bin/true",
        ])
        .unwrap_err();

        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn reads_the_relay_of_the_relay_helper() {
        let sut = RelayCommand::try_parse_from(["sandbox-relay", "3128=/tmp/http.sock"]).unwrap();

        assert_eq!(sut.relay.relay, relay(3128, "/tmp/http.sock"));
    }
}
