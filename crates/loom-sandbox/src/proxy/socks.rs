//! SOCKS5 proxy. It takes domain-name targets only, so the allowlist can match.

use std::io::{self, Read, Write};
use std::net::TcpStream;

use super::{HANDSHAKE_TIMEOUT, Rules, connect};
use crate::stream::pipe;

const SOCKS_VERSION: u8 = 5;
const SOCKS_NO_AUTH: u8 = 0;
const SOCKS_NO_ACCEPTABLE_AUTH: u8 = 0xFF;
const SOCKS_CONNECT: u8 = 1;
const SOCKS_ADDRESS_IPV4: u8 = 1;
const SOCKS_ADDRESS_DOMAIN: u8 = 3;
const SOCKS_ADDRESS_IPV6: u8 = 4;
const SOCKS_SUCCEEDED: u8 = 0;
const SOCKS_GENERAL_FAILURE: u8 = 1;
const SOCKS_NOT_ALLOWED: u8 = 2;
const SOCKS_HOST_UNREACHABLE: u8 = 4;
const SOCKS_COMMAND_UNSUPPORTED: u8 = 7;
const SOCKS_ADDRESS_UNSUPPORTED: u8 = 8;

fn read_exact<const N: usize>(stream: &mut TcpStream) -> io::Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn socks_reply(stream: &mut TcpStream, status: u8) -> io::Result<()> {
    stream.write_all(&[
        SOCKS_VERSION,
        status,
        0,
        SOCKS_ADDRESS_IPV4,
        0,
        0,
        0,
        0,
        0,
        0,
    ])?;
    stream.flush()
}

/// Agrees to use no authentication. `false` means the client already got a refusal.
fn negotiate_auth(client: &mut TcpStream) -> io::Result<bool> {
    let [version, method_count] = read_exact::<2>(client)?;
    if version != SOCKS_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported SOCKS version",
        ));
    }
    let mut methods = vec![0_u8; usize::from(method_count)];
    client.read_exact(&mut methods)?;
    if !methods.contains(&SOCKS_NO_AUTH) {
        client.write_all(&[SOCKS_VERSION, SOCKS_NO_ACCEPTABLE_AUTH])?;
        return Ok(false);
    }
    client.write_all(&[SOCKS_VERSION, SOCKS_NO_AUTH])?;
    Ok(true)
}

/// Reads the requested host and port. `None` means the client already got a refusal.
fn read_target(client: &mut TcpStream) -> io::Result<Option<(String, u16)>> {
    let [_, command, _, address_type] = read_exact::<4>(client)?;
    if command != SOCKS_CONNECT {
        socks_reply(client, SOCKS_COMMAND_UNSUPPORTED)?;
        return Ok(None);
    }
    let host = match address_type {
        SOCKS_ADDRESS_DOMAIN => {
            let [length] = read_exact::<1>(client)?;
            let mut name = vec![0_u8; usize::from(length)];
            client.read_exact(&mut name)?;
            String::from_utf8_lossy(&name).into_owned()
        }
        // The allowlist names hosts, so a literal address can never match it.
        SOCKS_ADDRESS_IPV4 => {
            let _ = read_exact::<6>(client)?;
            crate::diagnostic::report("denied SOCKS connection to a literal IPv4 address");
            socks_reply(client, SOCKS_NOT_ALLOWED)?;
            return Ok(None);
        }
        SOCKS_ADDRESS_IPV6 => {
            let _ = read_exact::<18>(client)?;
            crate::diagnostic::report("denied SOCKS connection to a literal IPv6 address");
            socks_reply(client, SOCKS_NOT_ALLOWED)?;
            return Ok(None);
        }
        _ => {
            socks_reply(client, SOCKS_ADDRESS_UNSUPPORTED)?;
            return Ok(None);
        }
    };
    let port = u16::from_be_bytes(read_exact::<2>(client)?);
    Ok(Some((host, port)))
}

pub(super) fn serve_socks(mut client: TcpStream, rules: &Rules) -> io::Result<()> {
    client.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    if !negotiate_auth(&mut client)? {
        return Ok(());
    }
    let Some((host, port)) = read_target(&mut client)? else {
        return Ok(());
    };

    if !rules.permits(&host, port) {
        return socks_reply(&mut client, SOCKS_NOT_ALLOWED);
    }
    let upstream = match connect(rules, &host, port) {
        Ok(upstream) => upstream,
        Err(error) => {
            let status = match error.kind() {
                io::ErrorKind::NotFound => SOCKS_HOST_UNREACHABLE,
                io::ErrorKind::PermissionDenied => SOCKS_NOT_ALLOWED,
                _ => SOCKS_GENERAL_FAILURE,
            };
            let _ = socks_reply(&mut client, status);
            return Err(error);
        }
    };
    client.set_read_timeout(None)?;
    socks_reply(&mut client, SOCKS_SUCCEEDED)?;
    pipe(client, upstream)
}
