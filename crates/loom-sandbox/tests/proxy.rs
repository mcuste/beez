//! Proxy allowlist tests against a local origin server.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use loom_core::DomainRule;
use loom_sandbox::Proxy;

const TIMEOUT: Duration = Duration::from_secs(5);
const SOCKS_GREETING: [u8; 3] = [5, 1, 0];

/// Accepts one connection and echoes every byte until the peer stops writing.
fn echo_origin() -> io::Result<(u16, JoinHandle<io::Result<Vec<u8>>>)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        let mut received = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Ok(received);
            }
            let bytes = chunk.get(..read).unwrap_or_default();
            received.extend_from_slice(bytes);
            stream.write_all(bytes)?;
        }
    });
    Ok((port, handle))
}

/// A listener that never accepts, to check the proxy does not connect to it.
fn unused_origin() -> io::Result<(u16, TcpListener)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    Ok((port, listener))
}

fn assert_never_reached(origin: &TcpListener) {
    let refused = origin.accept().err();
    assert!(
        refused.is_some_and(|error| error.kind() == io::ErrorKind::WouldBlock),
        "the proxy connected to the origin"
    );
}

/// A proxy that allows `texts`, and local addresses when `localhost` is set.
fn proxy(texts: &[&str], localhost: bool) -> io::Result<Proxy> {
    let rules = texts
        .iter()
        .map(|text| text.parse::<DomainRule>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    Proxy::start(rules, localhost)
}

fn connect(port: u16) -> io::Result<TcpStream> {
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    Ok(stream)
}

/// Sends `request`, signals end of output, and reads the whole answer.
fn exchange(port: u16, request: &[u8]) -> io::Result<Vec<u8>> {
    let mut client = connect(port)?;
    client.write_all(request)?;
    client.shutdown(Shutdown::Write)?;
    Ok(read_all(&mut client))
}

fn read_all(stream: &mut TcpStream) -> Vec<u8> {
    let mut output = Vec::new();
    let _ = stream.read_to_end(&mut output);
    output
}

/// Opens a SOCKS5 connection and reads the answer to the greeting.
fn socks_client(port: u16) -> io::Result<(TcpStream, [u8; 2])> {
    let mut client = connect(port)?;
    client.write_all(&SOCKS_GREETING)?;
    let mut greeting = [0_u8; 2];
    client.read_exact(&mut greeting)?;
    Ok((client, greeting))
}

/// A SOCKS5 `CONNECT` request for a domain-name target.
fn socks_request(host: &str, port: u16) -> io::Result<Vec<u8>> {
    let length = u8::try_from(host.len())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut request = vec![5, 1, 0, 3, length];
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(&port.to_be_bytes());
    Ok(request)
}

fn socks_reply(client: &mut TcpStream) -> io::Result<[u8; 10]> {
    let mut reply = [0_u8; 10];
    client.read_exact(&mut reply)?;
    Ok(reply)
}

#[test]
fn tunnels_connect_requests_to_allowed_hosts() {
    let (origin_port, origin) = echo_origin().unwrap();
    let proxy = proxy(&["localhost"], true).unwrap();
    let mut client = connect(proxy.http_port()).unwrap();

    write!(
        client,
        "CONNECT localhost:{origin_port} HTTP/1.1\r\nHost: localhost\r\n\r\n"
    )
    .unwrap();
    let mut response = [0_u8; 39];
    client.read_exact(&mut response).unwrap();
    client.write_all(b"ping").unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let echoed = read_all(&mut client);

    assert_eq!(
        &response[..],
        b"HTTP/1.1 200 Connection Established\r\n\r\n"
    );
    assert_eq!(echoed, b"ping");
    assert_eq!(origin.join().unwrap().unwrap(), b"ping");
}

#[test]
fn tunnels_bytes_that_arrive_with_the_connect_request() {
    let (origin_port, origin) = echo_origin().unwrap();
    let proxy = proxy(&["localhost"], true).unwrap();

    let response = exchange(
        proxy.http_port(),
        format!("CONNECT localhost:{origin_port} HTTP/1.1\r\n\r\nping").as_bytes(),
    )
    .unwrap();

    assert_eq!(response, b"HTTP/1.1 200 Connection Established\r\n\r\nping");
    assert_eq!(origin.join().unwrap().unwrap(), b"ping");
}

#[test]
fn refuses_connect_requests_to_other_hosts() {
    let (origin_port, origin) = unused_origin().unwrap();
    let proxy = proxy(&["example.com"], true).unwrap();

    let response = exchange(
        proxy.http_port(),
        format!("CONNECT localhost:{origin_port} HTTP/1.1\r\n\r\n").as_bytes(),
    )
    .unwrap();

    assert!(response.starts_with(b"HTTP/1.1 403 Forbidden"));
    assert_never_reached(&origin);
}

#[test]
fn honours_the_port_of_a_rule() {
    let (origin_port, origin) = unused_origin().unwrap();
    let proxy = proxy(&["localhost:1"], true).unwrap();

    let response = exchange(
        proxy.http_port(),
        format!("CONNECT localhost:{origin_port} HTTP/1.1\r\n\r\n").as_bytes(),
    )
    .unwrap();

    assert!(response.starts_with(b"HTTP/1.1 403 Forbidden"));
    assert_never_reached(&origin);
}

#[test]
fn matches_wildcard_rules_against_subdomains_only() {
    let proxy = proxy(&["*.localhost"], true).unwrap();

    let response = exchange(proxy.http_port(), b"CONNECT localhost:9 HTTP/1.1\r\n\r\n").unwrap();

    assert!(response.starts_with(b"HTTP/1.1 403 Forbidden"));
}

#[test]
fn refuses_an_allowed_host_that_resolves_to_a_local_address() {
    let (origin_port, origin) = unused_origin().unwrap();
    let proxy = proxy(&["localhost"], false).unwrap();

    let response = exchange(
        proxy.http_port(),
        format!("CONNECT localhost:{origin_port} HTTP/1.1\r\n\r\n").as_bytes(),
    )
    .unwrap();

    assert!(response.starts_with(b"HTTP/1.1 403 Forbidden"));
    assert_never_reached(&origin);
}

#[test]
fn forwards_plain_http_requests_with_a_closing_connection() {
    let (origin_port, origin) = echo_origin().unwrap();
    let proxy = proxy(&["localhost"], true).unwrap();

    let response = exchange(
        proxy.http_port(),
        format!(
            "GET http://localhost:{origin_port}/path HTTP/1.1\r\nHost: localhost:{origin_port}\r\nProxy-Connection: keep-alive\r\n\r\n"
        )
        .as_bytes(),
    )
    .unwrap();

    let forwarded = String::from_utf8(origin.join().unwrap().unwrap()).unwrap();
    assert_eq!(
        forwarded,
        format!(
            "GET http://localhost:{origin_port}/path HTTP/1.1\r\nHost: localhost:{origin_port}\r\nConnection: close\r\n\r\n"
        )
    );
    assert_eq!(response, forwarded.as_bytes());
}

#[test]
fn forwards_a_body_that_arrives_with_the_request_head() {
    let (origin_port, origin) = echo_origin().unwrap();
    let proxy = proxy(&["localhost"], true).unwrap();

    exchange(
        proxy.http_port(),
        format!(
            "POST http://localhost:{origin_port}/ HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\n\r\nbody"
        )
        .as_bytes(),
    )
    .unwrap();

    let forwarded = String::from_utf8(origin.join().unwrap().unwrap()).unwrap();
    assert!(forwarded.ends_with("\r\n\r\nbody"), "{forwarded}");
}

#[test]
fn serves_socks5_connections_to_allowed_domains() {
    let (origin_port, origin) = echo_origin().unwrap();
    let proxy = proxy(&["localhost"], true).unwrap();
    let (mut client, greeting) = socks_client(proxy.socks_port()).unwrap();

    client
        .write_all(&socks_request("localhost", origin_port).unwrap())
        .unwrap();
    let reply = socks_reply(&mut client).unwrap();
    client.write_all(b"pong").unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let echoed = read_all(&mut client);

    assert_eq!(greeting, [5, 0]);
    assert_eq!(reply.get(..2), Some(&[5, 0][..]));
    assert_eq!(echoed, b"pong");
    assert_eq!(origin.join().unwrap().unwrap(), b"pong");
}

#[test]
fn refuses_socks5_connections_to_other_domains() {
    let (origin_port, origin) = unused_origin().unwrap();
    let proxy = proxy(&["example.com"], true).unwrap();
    let (mut client, _) = socks_client(proxy.socks_port()).unwrap();

    client
        .write_all(&socks_request("localhost", origin_port).unwrap())
        .unwrap();

    assert_eq!(
        socks_reply(&mut client).unwrap().get(..2),
        Some(&[5, 2][..])
    );
    assert_never_reached(&origin);
}

#[test]
fn refuses_socks5_connections_to_literal_addresses() {
    let proxy = proxy(&["localhost"], true).unwrap();
    let (mut client, _) = socks_client(proxy.socks_port()).unwrap();

    client.write_all(&[5, 1, 0, 1, 127, 0, 0, 1, 0, 9]).unwrap();

    assert_eq!(
        socks_reply(&mut client).unwrap().get(..2),
        Some(&[5, 2][..])
    );
}

#[test]
fn refuses_socks5_connections_to_a_local_address() {
    let (origin_port, origin) = unused_origin().unwrap();
    let proxy = proxy(&["localhost"], false).unwrap();
    let (mut client, _) = socks_client(proxy.socks_port()).unwrap();

    client
        .write_all(&socks_request("localhost", origin_port).unwrap())
        .unwrap();

    assert_eq!(
        socks_reply(&mut client).unwrap().get(..2),
        Some(&[5, 2][..])
    );
    assert_never_reached(&origin);
}

#[test]
fn stops_listening_when_dropped() {
    let proxy = proxy(&[], false).unwrap();
    let port = proxy.http_port();
    drop(proxy);

    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
}

#[test]
fn rejects_malformed_http_requests() {
    let proxy = proxy(&["localhost"], true).unwrap();

    for head in [
        &b"GET\r\n\r\n"[..],
        &b"GET /path HTTP/1.1\r\n\r\n"[..],
        &b"CONNECT localhost:notaport HTTP/1.1\r\n\r\n"[..],
        &b"GET http://local\xffhost/ HTTP/1.1\r\n\r\n"[..],
    ] {
        let response = exchange(proxy.http_port(), head).unwrap();

        assert!(
            response.starts_with(b"HTTP/1.1 400 Bad Request"),
            "head {head:?} answered {}",
            String::from_utf8_lossy(&response)
        );
    }
}

#[test]
fn closes_a_socks4_handshake_without_a_reply() {
    let proxy = proxy(&["localhost"], true).unwrap();

    let response = exchange(proxy.socks_port(), &[4, 1]).unwrap();

    assert!(response.is_empty(), "{response:?}");
}

#[test]
fn refuses_socks5_clients_without_a_supported_authentication_method() {
    let proxy = proxy(&["localhost"], true).unwrap();

    let response = exchange(proxy.socks_port(), &[5, 1, 2]).unwrap();

    assert_eq!(response, [5, 255]);
}

#[test]
fn rejects_socks5_commands_other_than_connect() {
    let proxy = proxy(&["localhost"], true).unwrap();
    let (mut client, _) = socks_client(proxy.socks_port()).unwrap();

    client.write_all(&[5, 2, 0, 3]).unwrap();

    assert_eq!(
        socks_reply(&mut client).unwrap().get(..2),
        Some(&[5, 7][..])
    );
}

#[test]
fn rejects_unsupported_socks5_address_types() {
    let proxy = proxy(&["localhost"], true).unwrap();
    let (mut client, _) = socks_client(proxy.socks_port()).unwrap();

    client.write_all(&[5, 1, 0, 9]).unwrap();

    assert_eq!(
        socks_reply(&mut client).unwrap().get(..2),
        Some(&[5, 8][..])
    );
}
