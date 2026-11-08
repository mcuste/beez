//! The `PORT=SOCKET` argument that asks `sandbox-init` to relay one port.

use std::ffi::{OsStr, OsString};
use std::io;
use std::num::NonZeroU16;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// A loopback port inside the Linux sandbox and the Unix socket it forwards to.
///
/// It reads and writes as `PORT=SOCKET`, the form the `sandbox-init` command
/// takes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Relay {
    port: NonZeroU16,
    socket: PathBuf,
}

impl Relay {
    /// A relay of `port` to `socket`.
    #[must_use]
    pub fn new(port: NonZeroU16, socket: PathBuf) -> Self {
        Self { port, socket }
    }

    /// The loopback port the relay listens on.
    #[must_use]
    pub fn port(&self) -> NonZeroU16 {
        self.port
    }

    /// The Unix socket each connection goes to.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl TryFrom<&OsStr> for Relay {
    type Error = io::Error;

    /// Reads a `PORT=SOCKET` relay argument.
    fn try_from(argument: &OsStr) -> io::Result<Self> {
        let bytes = argument.as_bytes();
        let separator = bytes
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or_else(|| invalid("relay must be PORT=SOCKET"))?;
        let port = bytes.get(..separator).unwrap_or_default();
        let socket = bytes.get(separator + 1..).unwrap_or_default();
        let port = std::str::from_utf8(port)
            .ok()
            .and_then(|port| port.parse::<NonZeroU16>().ok())
            .ok_or_else(|| invalid("relay port must be a number from 1 to 65535"))?;
        if socket.is_empty() {
            return Err(invalid("relay must name a socket"));
        }
        Ok(Self::new(
            port,
            PathBuf::from(OsString::from_vec(socket.to_vec())),
        ))
    }
}

impl From<&Relay> for OsString {
    /// Writes the relay as `PORT=SOCKET`.
    fn from(relay: &Relay) -> Self {
        let mut argument = OsString::from(format!("{}=", relay.port));
        argument.push(&relay.socket);
        argument
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_owned())
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::io;
    use std::num::NonZeroU16;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    use super::Relay;

    #[test]
    fn reads_back_every_relay_argument_it_writes() {
        let port = NonZeroU16::new(3128).unwrap();
        for socket in [
            PathBuf::from("/tmp/beez/http.sock"),
            PathBuf::from("/tmp/beez=odd/socks.sock"),
            PathBuf::from(OsString::from_vec(b"/tmp/beez-\xff/http.sock".to_vec())),
        ] {
            let relay = Relay::new(port, socket);
            let argument = OsString::from(&relay);

            assert_eq!(Relay::try_from(argument.as_os_str()).unwrap(), relay);
        }
    }

    #[test]
    fn writes_the_port_then_the_socket() {
        let relay = Relay::new(
            NonZeroU16::new(1080).unwrap(),
            PathBuf::from("/tmp/socks.sock"),
        );

        assert_eq!(OsString::from(&relay), "1080=/tmp/socks.sock");
    }

    #[test]
    fn rejects_relay_arguments_it_cannot_use() {
        for argument in [
            "3128",
            "=/tmp/http.sock",
            "0=/tmp/http.sock",
            "65536=/tmp/http.sock",
            "3128=",
        ] {
            let error = Relay::try_from(OsStr::new(argument)).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{argument}");
        }
    }
}
