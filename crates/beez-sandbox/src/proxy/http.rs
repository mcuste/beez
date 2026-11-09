//! HTTP proxy, including `CONNECT` tunnels.

use std::io::{self, Read, Write};
use std::net::TcpStream;

use super::{Access, ConnectError, HANDSHAKE_TIMEOUT, Rules, Target, connect, relay, tunnel};

/// Bounds the buffer when a client never ends its request head.
const HEAD_LIMIT: usize = 64 * 1024;

/// Reads the request head and returns it with any bytes that followed it.
fn read_head(stream: &mut TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before the request head ended",
            ));
        }
        buffer.extend_from_slice(chunk.get(..read).unwrap_or_default());
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let rest = buffer.split_off(end + 4);
            return Ok((buffer, rest));
        }
        if buffer.len() > HEAD_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head is too large",
            ));
        }
    }
}

/// Splits `host:port` or `[v6]:port`, defaulting to `default_port`.
fn split_authority(authority: &str, default_port: u16) -> Option<Target> {
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, rest)| rest);
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, port) = rest.split_once(']')?;
        let port = match port.strip_prefix(':') {
            Some(port) => port.parse().ok()?,
            None => default_port,
        };
        return Some(Target {
            host: host.to_owned(),
            port,
        });
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (authority, default_port),
    };
    Some(Target {
        host: host.to_owned(),
        port,
    })
}

fn respond(stream: &mut TcpStream, status: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()
}

/// What a request asks the proxy to do.
#[derive(Debug, Eq, PartialEq)]
enum Request {
    /// A `CONNECT` tunnel to the target.
    Tunnel(Target),
    /// A plain request, with the head to forward to the target.
    Plain { target: Target, head: String },
}

impl Request {
    fn target(&self) -> &Target {
        match self {
            Self::Tunnel(target) | Self::Plain { target, .. } => target,
        }
    }

    fn access(&self) -> Access {
        match self {
            Self::Tunnel(_) => Access::Tunnel,
            Self::Plain { .. } => Access::PlainHttp,
        }
    }
}

/// Reads the target out of a request head, or `None` when the head is malformed.
fn parse_request(head: &[u8]) -> Option<Request> {
    let head = std::str::from_utf8(head).ok()?;
    let request_line = head.split("\r\n").next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next()?;
    let target = parts.next()?;

    if method.eq_ignore_ascii_case("CONNECT") {
        return split_authority(target, Access::Tunnel.default_port()).map(Request::Tunnel);
    }
    let version = parts.next()?;
    let rest = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("HTTP://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let checked = split_authority(authority, Access::PlainHttp.default_port())?;
    let path = rest
        .strip_prefix(authority)
        .unwrap_or_default()
        .split('#')
        .next()
        .unwrap_or_default();
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    let forward = rewrite_head(
        head,
        &format!("{method} {path} {version}"),
        &host_header(&checked),
    );
    Some(Request::Plain {
        target: checked,
        head: forward,
    })
}

/// The `Host` header for a checked target, with the port when it is not 80.
fn host_header(target: &Target) -> String {
    let Target { host, port } = target;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.clone()
    };
    if *port == Access::PlainHttp.default_port() {
        host
    } else {
        format!("{host}:{port}")
    }
}

/// The status line answering a connection that failed.
///
/// Only a policy refusal is a 403. Any other failure, including an operating
/// system that refuses the connection, is the gateway's.
fn connect_failure_status(error: &ConnectError) -> &'static str {
    match error {
        ConnectError::Denied => "403 Forbidden",
        ConnectError::Unresolved | ConnectError::Io => "502 Bad Gateway",
    }
}

pub(super) fn serve_http(mut client: TcpStream, rules: &Rules) -> io::Result<()> {
    client.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let (head, early_body) = match read_head(&mut client) {
        Ok(parts) => parts,
        Err(error) => {
            let _ = respond(&mut client, "400 Bad Request");
            return Err(error);
        }
    };
    let Some(request) = parse_request(&head) else {
        tracing::debug!(bytes = head.len(), "malformed HTTP proxy request");
        return respond(&mut client, "400 Bad Request");
    };
    tracing::trace!(target = %request.target(), access = ?request.access(), "HTTP proxy request");

    if !rules.permits(request.target(), request.access()) {
        return respond(&mut client, "403 Forbidden");
    }
    let mut upstream = match connect(rules, request.target()) {
        Ok(upstream) => upstream,
        Err(error) => {
            tracing::debug!(target = %request.target(), ?error, "HTTP proxy cannot connect");
            return respond(&mut client, connect_failure_status(&error));
        }
    };
    match request {
        Request::Plain { head, .. } => {
            upstream.write_all(head.as_bytes())?;
            relay(client, upstream, &early_body)
        }
        Request::Tunnel(target) => {
            client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
            client.flush()?;
            tunnel(rules, client, upstream, &target, early_body)
        }
    }
}

/// Headers the proxy drops and writes again itself.
const REPLACED_HEADERS: [&str; 4] = [
    "host",
    "connection",
    "proxy-connection",
    "proxy-authorization",
];

/// Rewrites a plain request for the origin.
///
/// The request line takes the origin form, and `Host` names the host the
/// proxy checked, so a server that picks its site from `Host` serves that one.
/// The upstream closes after one response, so the connection cannot be reused
/// for a different host.
fn rewrite_head(head: &str, request_line: &str, host: &str) -> String {
    let mut rewritten = format!("{request_line}\r\nHost: {host}\r\n");
    for line in head.split("\r\n").skip(1).filter(|line| !line.is_empty()) {
        let name = line.split(':').next().unwrap_or_default().trim();
        if REPLACED_HEADERS
            .iter()
            .any(|replaced| name.eq_ignore_ascii_case(replaced))
        {
            continue;
        }
        rewritten.push_str(line);
        rewritten.push_str("\r\n");
    }
    rewritten.push_str("Connection: close\r\n\r\n");
    rewritten
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::net::{Ipv4Addr, TcpStream};
    use std::thread;

    use super::{
        Request, connect_failure_status, host_header, parse_request, read_head, rewrite_head,
        split_authority,
    };
    use crate::proxy::{ConnectError, Target, loopback_listener};

    fn target(host: &str, port: u16) -> Target {
        Target {
            host: host.to_owned(),
            port,
        }
    }

    /// The head a plain request forwards, or `None` for a tunnel.
    fn forwarded(request: &Request) -> Option<&str> {
        match request {
            Request::Plain { head, .. } => Some(head),
            Request::Tunnel(_) => None,
        }
    }

    /// A connected loopback pair, client end first.
    fn socket_pair() -> io::Result<(TcpStream, TcpStream)> {
        let (listener, port) = loopback_listener()?;
        let client = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
        let (server, _) = listener.accept()?;
        Ok((client, server))
    }

    #[test]
    fn splits_the_head_from_the_bytes_after_it() {
        let (mut client, mut server) = socket_pair().unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\nbody")
            .unwrap();

        let (head, rest) = read_head(&mut server).unwrap();

        assert_eq!(head, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(rest, b"body");
    }

    #[test]
    fn refuses_a_head_that_never_ends() {
        let (mut client, mut server) = socket_pair().unwrap();
        let writer = thread::spawn(move || {
            let junk = vec![b'a'; 4096];
            for _ in 0..20 {
                if client.write_all(&junk).is_err() {
                    return;
                }
            }
        });

        let error = read_head(&mut server).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        drop(server);
        let _ = writer.join();
    }

    #[test]
    fn splits_authorities_with_and_without_ports() {
        assert_eq!(
            split_authority("example.com:8443", 443),
            Some(target("example.com", 8443))
        );
        assert_eq!(
            split_authority("example.com", 80),
            Some(target("example.com", 80))
        );
        assert_eq!(
            split_authority("user:secret@example.com", 80),
            Some(target("example.com", 80))
        );
        assert_eq!(split_authority("[::1]:3000", 80), Some(target("::1", 3000)));
        assert_eq!(split_authority("[::1]", 80), Some(target("::1", 80)));
        assert_eq!(split_authority("example.com:http", 80), None);
    }

    #[test]
    fn parses_tunnel_and_plain_targets_and_rejects_the_rest() {
        let tunnel = parse_request(b"CONNECT example.com:8443 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(tunnel, Request::Tunnel(target("example.com", 8443)));
        let default_tunnel = parse_request(b"CONNECT example.com HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(default_tunnel, Request::Tunnel(target("example.com", 443)));

        let plain = parse_request(b"GET http://example.com/a?b#c HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(plain.target(), &target("example.com", 80));
        assert_eq!(
            forwarded(&plain),
            Some("GET /a?b HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")
        );

        assert!(parse_request(b"GET\r\n\r\n").is_none());
        assert!(parse_request(b"GET /path HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_request(b"CONNECT host:notaport HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_request(b"GET http://local\xffhost/ HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_request(b"GET http://example.com/\r\n\r\n").is_none());
    }

    #[test]
    fn rewrites_the_head_to_close_after_one_response() {
        let head = "GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\nProxy-Connection: keep-alive\r\nConnection: keep-alive\r\nAccept: */*\r\n\r\n";

        assert_eq!(
            rewrite_head(head, "GET / HTTP/1.1", "example.com"),
            "GET / HTTP/1.1\r\nHost: example.com\r\nAccept: */*\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn names_the_checked_host_whatever_host_the_client_sent() {
        let request = parse_request(
            b"GET http://api.example.com/v1 HTTP/1.1\r\nHost: evil.example\r\nhost: evil.example\r\nProxy-Authorization: x\r\n\r\n",
        )
        .unwrap();

        assert_eq!(
            forwarded(&request),
            Some("GET /v1 HTTP/1.1\r\nHost: api.example.com\r\nConnection: close\r\n\r\n")
        );
    }

    #[test]
    fn writes_the_port_and_brackets_into_the_host_header() {
        assert_eq!(host_header(&target("example.com", 80)), "example.com");
        assert_eq!(
            host_header(&target("example.com", 8080)),
            "example.com:8080"
        );
        assert_eq!(host_header(&target("::1", 3000)), "[::1]:3000");
        assert_eq!(
            forwarded(&parse_request(b"GET http://example.com:8080 HTTP/1.1\r\n\r\n").unwrap()),
            Some("GET / HTTP/1.1\r\nHost: example.com:8080\r\nConnection: close\r\n\r\n")
        );
    }

    #[test]
    fn answers_only_a_policy_refusal_with_forbidden() {
        assert_eq!(
            connect_failure_status(&ConnectError::Denied),
            "403 Forbidden"
        );
        assert_eq!(
            connect_failure_status(&ConnectError::Unresolved),
            "502 Bad Gateway"
        );
        assert_eq!(connect_failure_status(&ConnectError::Io), "502 Bad Gateway");
    }
}
