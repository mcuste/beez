use std::collections::BTreeSet;
use std::io;
#[cfg(target_os = "linux")]
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use beez_policy::DomainRule;

use crate::server::Server;

mod http;
mod socks;
mod tls;

use tls::Hello;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Bounds the handshake only. An established tunnel has no idle timeout,
/// because a streaming response can stay silent for a long time.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// The port a rule without a port allows through a tunnel. Tunnels to it must
/// carry TLS that names the host they were opened for.
const TLS_PORT: u16 = 443;
/// The port a rule without a port allows for plain HTTP requests.
const PLAIN_HTTP_PORT: u16 = 80;

/// Cloud metadata services outside the loopback and link-local ranges.
///
/// They hand out the credentials of the machine they run on.
const METADATA_ADDRESSES: [IpAddr; 5] = [
    // Alibaba Cloud.
    IpAddr::V4(Ipv4Addr::new(100, 100, 100, 200)),
    // Azure WireServer.
    IpAddr::V4(Ipv4Addr::new(168, 63, 129, 16)),
    // AWS, over IPv6.
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)),
    // Google Cloud, over IPv6.
    IpAddr::V6(Ipv6Addr::new(0xfd20, 0xce, 0, 0, 0, 0, 0, 0x254)),
    // Oracle Cloud, over IPv6.
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0xc1, 0, 0, 0, 0, 0xa9fe, 0xa9fe)),
];

/// Loopback HTTP and SOCKS5 proxies that only connect to allowed hosts.
///
/// Both proxies see the target host name before any payload flows, so the
/// allowlist applies without inspecting traffic. Denied targets get a refusal
/// and a line on Beez's stderr.
#[derive(Debug)]
pub struct Proxy {
    http_port: u16,
    socks_port: u16,
    _http: Server,
    _socks: Server,
}

impl Proxy {
    /// Starts both proxies on ephemeral loopback ports.
    ///
    /// The proxies run outside the sandbox, so they also apply the `localhost`
    /// rule: unless it is set, an allowed name that resolves to a loopback or
    /// link-local address is refused.
    pub fn start(rules: Vec<DomainRule>, localhost: bool) -> io::Result<Self> {
        let rules = Arc::new(Rules {
            domains: rules,
            localhost,
            reported: Mutex::new(BTreeSet::new()),
        });
        let (http, http_port) = loopback_listener()?;
        let (socks, socks_port) = loopback_listener()?;
        let http_rules = Arc::clone(&rules);
        let http = Server::spawn(http, move |stream| {
            let _ = http::serve_http(stream, &http_rules);
        });
        let socks = Server::spawn(socks, move |stream| {
            let _ = socks::serve_socks(stream, &rules);
        });

        Ok(Self {
            http_port,
            socks_port,
            _http: http,
            _socks: socks,
        })
    }

    /// Port of the HTTP proxy, which also accepts `CONNECT`.
    #[must_use]
    pub fn http_port(&self) -> u16 {
        self.http_port
    }

    /// Port of the SOCKS5 proxy, which requires domain-name targets.
    #[must_use]
    pub fn socks_port(&self) -> u16 {
        self.socks_port
    }
}

/// A listener on an ephemeral loopback port, with the port it got.
pub(crate) fn loopback_listener() -> io::Result<(TcpListener, u16)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// The allowlist plus the targets already reported as denied.
struct Rules {
    domains: Vec<DomainRule>,
    localhost: bool,
    reported: Mutex<BTreeSet<String>>,
}

impl Rules {
    /// True when a rule allows the target. Reports each denied target once.
    ///
    /// A rule without a port allows `default_port`.
    fn permits(&self, host: &str, port: u16, default_port: u16) -> bool {
        if self
            .domains
            .iter()
            .any(|rule| rule.matches(host, port, default_port))
        {
            return true;
        }
        self.report(&format!("denied connection to {host}:{port}"));
        false
    }

    /// True when the policy allows connections to `address`.
    fn permits_address(&self, host: &str, address: IpAddr) -> bool {
        if self.localhost || !is_local(address) {
            return true;
        }
        self.report(&format!("denied connection to {host} ({address})"));
        false
    }

    /// Writes each message once on Beez's stderr.
    fn report(&self, message: &str) {
        let first_time = self
            .reported
            .lock()
            .map_or(true, |mut reported| reported.insert(message.to_owned()));
        if first_time {
            crate::diagnostic::report(message);
        }
    }
}

/// Reads the start of a tunnel to `host` and decides whether it may go on.
///
/// A tunnel to port 443 must open with a TLS `ClientHello` that names `host`.
/// Shared hosting serves many sites on one address and picks the site from
/// that name, so without the check a tunnel to an allowed host on a CDN could
/// reach any other site the CDN serves. An encrypted `ClientHello` hides the
/// name, so it is refused too. Tunnels to other ports are not read.
///
/// Answers every byte read, for the caller to send on, or `None` after a
/// refusal.
fn open_tunnel(
    rules: &Rules,
    client: &mut TcpStream,
    host: &str,
    port: u16,
    received: Vec<u8>,
) -> io::Result<Option<Vec<u8>>> {
    if port != TLS_PORT {
        return Ok(Some(received));
    }
    let received = tls::read_first_record(client, received)?;
    let refusal = match tls::parse(&received) {
        Hello::Other => format!("denied a tunnel to {host}:{port} that did not start TLS"),
        Hello::Tls {
            encrypted: true, ..
        } => format!("denied an encrypted TLS hello through a tunnel to {host}:{port}"),
        Hello::Tls {
            server_name: None, ..
        } if host.parse::<IpAddr>().is_ok() => return Ok(Some(received)),
        Hello::Tls {
            server_name: None, ..
        } => format!("denied TLS without a server name through a tunnel to {host}:{port}"),
        Hello::Tls {
            server_name: Some(name),
            ..
        } if same_host(&name, host) => return Ok(Some(received)),
        Hello::Tls {
            server_name: Some(name),
            ..
        } => format!("denied TLS to {name} through a tunnel to {host}:{port}"),
    };
    rules.report(&refusal);
    Ok(None)
}

/// True when two DNS names name the same host.
fn same_host(one: &str, other: &str) -> bool {
    one.trim_end_matches('.')
        .eq_ignore_ascii_case(other.trim_end_matches('.'))
}

/// True for addresses that reach this machine, its link, or a cloud metadata service.
///
/// Link-local addresses count, because they carry the cloud metadata service.
/// Private ranges do not: a task that names an internal host reaches it. An
/// IPv6 address that carries an IPv4 address counts when that address does.
fn is_local(address: IpAddr) -> bool {
    if METADATA_ADDRESSES.contains(&address) {
        return true;
    }
    match address {
        IpAddr::V4(address) => {
            // 0.0.0.0/8 means this host.
            address.is_loopback()
                || address.is_link_local()
                || address.octets().first() == Some(&0)
                || address.is_broadcast()
        }
        IpAddr::V6(address) => {
            if let Some(embedded) = embedded_ipv4(address) {
                return is_local(IpAddr::V4(embedded));
            }
            let leading = address.segments().first().copied().unwrap_or_default();
            // fe80::/10 is link local.
            address.is_loopback() || address.is_unspecified() || leading & 0xffc0 == 0xfe80
        }
    }
}

/// The IPv4 address an IPv6 address stands for, when it stands for one.
///
/// Covers IPv4-mapped and IPv4-compatible addresses, the well-known NAT64
/// prefix `64:ff9b::/96`, and 6to4 addresses in `2002::/16`.
fn embedded_ipv4(address: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(embedded) = address.to_ipv4() {
        // `::` and `::1` are IPv6 addresses of their own.
        return (!address.is_unspecified() && !address.is_loopback()).then_some(embedded);
    }
    match address.segments() {
        [0x64, 0xff9b, 0, 0, 0, 0, high, low] | [0x2002, high, low, ..] => Some(
            Ipv4Addr::from_bits((u32::from(high) << 16) | u32::from(low)),
        ),
        _ => None,
    }
}

/// Connects to the first allowed address of `host`.
///
/// A `PermissionDenied` error means the policy refused every address, which
/// the proxies answer with a refusal rather than a gateway error.
fn connect(rules: &Rules, host: &str, port: u16) -> io::Result<TcpStream> {
    let mut last_error = io::Error::new(io::ErrorKind::NotFound, format!("{host} did not resolve"));
    for address in (host, port).to_socket_addrs()? {
        if !rules.permits_address(host, address.ip()) {
            last_error = io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{host} resolves to local address {}", address.ip()),
            );
            continue;
        }
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

/// The loopback address of a proxy port.
#[cfg(target_os = "linux")]
pub(crate) fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io::Write;
    use std::net::{IpAddr, Ipv4Addr, TcpStream};
    use std::sync::Mutex;

    use super::tls::tests::{client_hello, encrypted_hello_extension, server_name_extension};
    use super::{Rules, is_local, loopback_listener, open_tunnel};

    fn rules() -> Rules {
        Rules {
            domains: Vec::new(),
            localhost: false,
            reported: Mutex::new(BTreeSet::new()),
        }
    }

    /// Runs `open_tunnel` for `host:port` on a client that sends `bytes`.
    fn open(host: &str, port: u16, bytes: &[u8]) -> Option<Vec<u8>> {
        let (listener, listen_port) = loopback_listener().unwrap();
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).unwrap();
        client.write_all(bytes).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        open_tunnel(&rules(), &mut server, host, port, Vec::new()).unwrap()
    }

    #[test]
    fn opens_a_tls_tunnel_that_names_its_host() {
        let hello = client_hello(&[server_name_extension("API.example.com.")]);

        assert_eq!(open("api.example.com", 443, &hello), Some(hello));
    }

    #[test]
    fn refuses_a_tls_tunnel_that_names_another_host() {
        let hello = client_hello(&[server_name_extension("evil.example")]);

        assert_eq!(open("api.example.com", 443, &hello), None);
    }

    #[test]
    fn refuses_an_encrypted_hello_and_a_hello_without_a_name() {
        let encrypted = client_hello(&[
            server_name_extension("api.example.com"),
            encrypted_hello_extension(),
        ]);
        let nameless = client_hello(&[]);

        assert_eq!(open("api.example.com", 443, &encrypted), None);
        assert_eq!(open("api.example.com", 443, &nameless), None);
    }

    #[test]
    fn opens_a_nameless_tls_tunnel_to_an_address() {
        let nameless = client_hello(&[]);

        assert_eq!(open("10.0.0.5", 443, &nameless), Some(nameless));
    }

    #[test]
    fn refuses_plain_text_on_the_tls_port() {
        assert_eq!(
            open(
                "api.example.com",
                443,
                b"GET / HTTP/1.1\r\nHost: evil\r\n\r\n"
            ),
            None
        );
    }

    #[test]
    fn leaves_tunnels_to_other_ports_unread() {
        assert_eq!(open("github.com", 22, b"SSH-2.0-x\r\n"), Some(Vec::new()));
    }

    fn local(text: &str) -> bool {
        is_local(text.parse::<IpAddr>().unwrap())
    }

    #[test]
    fn counts_loopback_link_local_and_this_network_as_local() {
        for text in [
            "127.0.0.1",
            "127.1.2.3",
            "169.254.169.254",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "::1",
            "::",
            "fe80::1",
        ] {
            assert!(local(text), "{text}");
        }
    }

    #[test]
    fn counts_cloud_metadata_services_as_local() {
        for text in [
            "100.100.100.200",
            "168.63.129.16",
            "fd00:ec2::254",
            "fd20:ce::254",
            "fd00:c1::a9fe:a9fe",
        ] {
            assert!(local(text), "{text}");
        }
    }

    #[test]
    fn counts_ipv6_forms_of_a_local_ipv4_address_as_local() {
        for text in [
            "::ffff:127.0.0.1",
            "::127.0.0.1",
            "::169.254.169.254",
            "64:ff9b::169.254.169.254",
            "64:ff9b::7f00:1",
            "2002:a9fe:a9fe::1",
            "2002:7f00:1::",
        ] {
            assert!(local(text), "{text}");
        }
    }

    #[test]
    fn leaves_public_and_private_addresses_reachable() {
        for text in [
            "1.1.1.1",
            "10.0.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "2606:4700:4700::1111",
            "fd00::1",
            "64:ff9b::1.1.1.1",
            "2002:101:101::1",
        ] {
            assert!(!local(text), "{text}");
        }
    }
}
