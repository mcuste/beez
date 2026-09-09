//! HTTP proxy, including `CONNECT` tunnels.

use std::io::{self, Read, Write};
use std::net::TcpStream;

use super::{HANDSHAKE_TIMEOUT, Rules, connect};
use crate::stream::pipe;

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
fn split_authority(authority: &str, default_port: u16) -> Option<(String, u16)> {
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, rest)| rest);
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, port) = rest.split_once(']')?;
        let port = match port.strip_prefix(':') {
            Some(port) => port.parse().ok()?,
            None => default_port,
        };
        return Some((host.to_owned(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_owned(), port.parse().ok()?)),
        None => Some((authority.to_owned(), default_port)),
    }
}

fn respond(stream: &mut TcpStream, status: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()
}

/// The target a request names, and the head to forward when it is not a tunnel.
struct Request {
    host: String,
    port: u16,
    forward: Option<String>,
}

/// Reads the target out of a request head, or `None` when the head is malformed.
fn parse_request(head: &[u8]) -> Option<Request> {
    let head = std::str::from_utf8(head).ok()?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let method = request_line.next()?;
    let target = request_line.next()?;

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_authority(target, 443)?;
        return Some(Request {
            host,
            port,
            forward: None,
        });
    }
    let rest = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("HTTP://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, port) = split_authority(authority, 80)?;
    Some(Request {
        host,
        port,
        forward: Some(rewrite_head(head)),
    })
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
        return respond(&mut client, "400 Bad Request");
    };

    if !rules.permits(&request.host, request.port) {
        return respond(&mut client, "403 Forbidden");
    }
    let mut upstream = match connect(rules, &request.host, request.port) {
        Ok(upstream) => upstream,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return respond(&mut client, "403 Forbidden");
        }
        Err(error) => {
            let _ = respond(&mut client, "502 Bad Gateway");
            return Err(error);
        }
    };
    client.set_read_timeout(None)?;

    if let Some(head) = request.forward {
        upstream.write_all(head.as_bytes())?;
    } else {
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
        client.flush()?;
    }
    upstream.write_all(&early_body)?;
    pipe(client, upstream)
}

/// Forces the upstream to close after one response so the connection cannot
/// be reused for a different host.
fn rewrite_head(head: &str) -> String {
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut rewritten = format!("{request_line}\r\n");
    for line in lines.filter(|line| !line.is_empty()) {
        let name = line.split(':').next().unwrap_or_default().trim();
        if name.eq_ignore_ascii_case("connection") || name.eq_ignore_ascii_case("proxy-connection")
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
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::thread;

    use super::{parse_request, read_head, rewrite_head, split_authority};

    /// A connected loopback pair, client end first.
    fn socket_pair() -> io::Result<(TcpStream, TcpStream)> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let client = TcpStream::connect(listener.local_addr()?)?;
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
            Some(("example.com".into(), 8443))
        );
        assert_eq!(
            split_authority("example.com", 80),
            Some(("example.com".into(), 80))
        );
        assert_eq!(
            split_authority("user:secret@example.com", 80),
            Some(("example.com".into(), 80))
        );
        assert_eq!(
            split_authority("[::1]:3000", 80),
            Some(("::1".into(), 3000))
        );
        assert_eq!(split_authority("[::1]", 80), Some(("::1".into(), 80)));
        assert_eq!(split_authority("example.com:http", 80), None);
    }

    #[test]
    fn parses_tunnel_and_plain_targets_and_rejects_the_rest() {
        let tunnel = parse_request(b"CONNECT example.com:8443 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!((tunnel.host.as_str(), tunnel.port), ("example.com", 8443));
        assert!(tunnel.forward.is_none());

        let plain = parse_request(b"GET http://example.com/a?b HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!((plain.host.as_str(), plain.port), ("example.com", 80));
        assert!(plain.forward.is_some());

        assert!(parse_request(b"GET\r\n\r\n").is_none());
        assert!(parse_request(b"GET /path HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_request(b"CONNECT host:notaport HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_request(b"GET http://local\xffhost/ HTTP/1.1\r\n\r\n").is_none());
    }

    #[test]
    fn rewrites_the_head_to_close_after_one_response() {
        let head = "GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\nProxy-Connection: keep-alive\r\nConnection: keep-alive\r\nAccept: */*\r\n\r\n";

        assert_eq!(
            rewrite_head(head),
            "GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\nAccept: */*\r\nConnection: close\r\n\r\n"
        );
    }
}
