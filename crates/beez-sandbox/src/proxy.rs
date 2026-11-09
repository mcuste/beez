use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};
#[cfg(target_os = "linux")]
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use beez_policy::DomainRule;

use crate::server::Server;
use crate::stream::pipe;

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

/// How many distinct denials are remembered so each is reported once.
///
/// Past this many, a new denial is still reported but no longer remembered,
/// so a task that tries endless targets cannot grow the set without bound.
const REPORTED_LIMIT: usize = 1024;

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
    ports: ProxyPorts,
    _http: Server,
    _socks: Server,
}

impl Proxy {
    /// Starts both proxies on ephemeral loopback ports.
    ///
    /// The proxies run outside the sandbox, so they also apply the `localhost`
    /// rule: unless it is set, an allowed name that resolves to a loopback or
    /// link-local address is refused.
    pub fn start(network: NetworkRules) -> io::Result<Self> {
        let rules = Arc::new(Rules {
            network,
            reported: Mutex::new(BTreeSet::new()),
        });
        let (http, http_port) = loopback_listener()?;
        let (socks, socks_port) = loopback_listener()?;
        let http_rules = Arc::clone(&rules);
        // Connections arrive on threads of their own, so their trace lines
        // name the request that started the proxy.
        let http_span = tracing::Span::current();
        let socks_span = http_span.clone();
        let http = Server::spawn(http, move |stream| {
            let _span = http_span.enter();
            if let Err(error) = http::serve_http(stream, &http_rules) {
                tracing::debug!(proxy = "http", %error, "proxy connection ended in an error");
            }
        });
        let socks = Server::spawn(socks, move |stream| {
            let _span = socks_span.enter();
            if let Err(error) = socks::serve_socks(stream, &rules) {
                tracing::debug!(proxy = "socks", %error, "proxy connection ended in an error");
            }
        });
        tracing::debug!(http = http_port, socks = socks_port, "proxies started");

        Ok(Self {
            ports: ProxyPorts {
                http: http_port,
                socks: socks_port,
            },
            _http: http,
            _socks: socks,
        })
    }

    /// Port of the HTTP proxy, which also accepts `CONNECT`.
    #[must_use]
    pub fn http_port(&self) -> u16 {
        self.ports.http
    }

    /// Port of the SOCKS5 proxy, which requires domain-name targets.
    #[must_use]
    pub fn socks_port(&self) -> u16 {
        self.ports.socks
    }

    /// Both ports, as the sandbox backends take them.
    pub(crate) fn ports(&self) -> ProxyPorts {
        self.ports
    }
}

/// The hosts the proxies may reach.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NetworkRules {
    /// Hosts a client may name.
    pub domains: Vec<DomainRule>,
    /// True when an allowed host may resolve to a loopback or link-local address.
    pub localhost: bool,
}

/// The loopback ports of the HTTP and SOCKS5 proxies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProxyPorts {
    /// The HTTP proxy, which also accepts `CONNECT`.
    pub(crate) http: u16,
    /// The SOCKS5 proxy.
    pub(crate) socks: u16,
}

/// A listener on an ephemeral loopback port, with the port it got.
pub(crate) fn loopback_listener() -> io::Result<(TcpListener, u16)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// A host and port a client asks the proxy to reach.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Target {
    host: String,
    port: u16,
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.host, self.port)
    }
}

/// How a client uses a connection, which sets the port a rule without a port allows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Access {
    /// A tunnel that carries any bytes, from `CONNECT` or SOCKS5.
    Tunnel,
    /// A plain HTTP request the proxy forwards.
    PlainHttp,
}

impl Access {
    /// The port a rule without a port allows for this access.
    fn default_port(self) -> u16 {
        match self {
            Self::Tunnel => TLS_PORT,
            Self::PlainHttp => PLAIN_HTTP_PORT,
        }
    }
}

/// The allowlist plus the targets already reported as denied.
struct Rules {
    network: NetworkRules,
    reported: Mutex<BTreeSet<String>>,
}

impl Rules {
    /// True when a rule allows the target. Reports each denied target once.
    ///
    /// A rule without a port allows the default port of `access`.
    fn permits(&self, target: &Target, access: Access) -> bool {
        let default_port = access.default_port();
        if self
            .network
            .domains
            .iter()
            .any(|rule| rule.matches(&target.host, target.port, default_port))
        {
            tracing::debug!(%target, ?access, "allowed: a rule names the host");
            return true;
        }
        tracing::debug!(%target, ?access, "denied: no rule names the host");
        self.report(&format!("denied connection to {target}"));
        false
    }

    /// True when the policy allows connections to `address`.
    fn permits_address(&self, host: &str, address: IpAddr) -> bool {
        if self.network.localhost || !is_local(address) {
            tracing::trace!(host, %address, "allowed address");
            return true;
        }
        tracing::debug!(host, %address, "denied: the address is local");
        self.report(&format!("denied connection to {host} ({address})"));
        false
    }

    /// Writes each message once on Beez's stderr.
    ///
    /// Only the first [`REPORTED_LIMIT`] messages are remembered; later new
    /// ones are written every time.
    fn report(&self, message: &str) {
        let first_time = self.reported.lock().map_or(true, |mut reported| {
            if reported.contains(message) {
                false
            } else {
                if reported.len() < REPORTED_LIMIT {
                    reported.insert(message.to_owned());
                }
                true
            }
        });
        if first_time {
            crate::diagnostic::report(message);
        }
    }
}

/// Checks the start of a tunnel to `target`, then copies bytes both ways.
///
/// `received` holds the bytes the client sent before the tunnel opened.
fn tunnel(
    rules: &Rules,
    mut client: TcpStream,
    upstream: TcpStream,
    target: &Target,
    received: Vec<u8>,
) -> io::Result<()> {
    let Some(received) = open_tunnel(rules, &mut client, target, received)? else {
        return Ok(());
    };
    relay(client, upstream, &received)
}

/// Ends the handshake: sends `early` bytes upstream and copies bytes both ways.
fn relay(client: TcpStream, mut upstream: TcpStream, early: &[u8]) -> io::Result<()> {
    client.set_read_timeout(None)?;
    upstream.write_all(early)?;
    pipe(client, upstream)
}

/// Reads the start of a tunnel to `target` and decides whether it may go on.
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
    target: &Target,
    received: Vec<u8>,
) -> io::Result<Option<Vec<u8>>> {
    if target.port != TLS_PORT {
        return Ok(Some(received));
    }
    let received = tls::read_first_record(client, received)?;
    let hello = tls::parse(&received);
    tracing::trace!(%target, ?hello, "TLS hello");
    match tunnel_verdict(&hello, target) {
        Ok(()) => {
            tracing::debug!(%target, "allowed: the TLS hello names the tunnel host");
            Ok(Some(received))
        }
        Err(refusal) => {
            tracing::debug!(%target, %refusal, "denied: TLS hello");
            rules.report(&refusal);
            Ok(None)
        }
    }
}

/// Whether a TLS tunnel to `target` that starts with `hello` may go on.
///
/// The error is the refusal to report.
fn tunnel_verdict(hello: &Hello, target: &Target) -> Result<(), String> {
    let Hello::Tls {
        server_name,
        encrypted,
    } = hello
    else {
        return Err(format!(
            "denied a tunnel to {target} that did not start TLS"
        ));
    };
    if *encrypted {
        return Err(format!(
            "denied an encrypted TLS hello through a tunnel to {target}"
        ));
    }
    let host = target.host.as_str();
    match server_name {
        // A client sends no name for a tunnel opened to an address.
        None if host.parse::<IpAddr>().is_ok() => Ok(()),
        None => Err(format!(
            "denied TLS without a server name through a tunnel to {target}"
        )),
        Some(name) if same_host(name, host) => Ok(()),
        Some(name) => Err(format!("denied TLS to {name} through a tunnel to {target}")),
    }
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

/// Why [`connect`] reached no address.
#[derive(Debug)]
enum ConnectError {
    /// The policy refused the last address tried.
    Denied,
    /// The host resolved to no address.
    Unresolved,
    /// Resolving, or connecting to the last address tried, failed.
    Io,
}

/// Connects to the first allowed address of `target`.
///
/// When every address fails, the error is the outcome of the last one tried.
fn connect(rules: &Rules, target: &Target) -> Result<TcpStream, ConnectError> {
    let host = target.host.as_str();
    let mut last_error = ConnectError::Unresolved;
    let addresses = (host, target.port).to_socket_addrs().map_err(|error| {
        tracing::debug!(%target, %error, "cannot resolve the host");
        ConnectError::Io
    })?;
    for address in addresses {
        if !rules.permits_address(host, address.ip()) {
            last_error = ConnectError::Denied;
            continue;
        }
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => {
                tracing::debug!(%target, %address, "connected upstream");
                return Ok(stream);
            }
            Err(error) => {
                tracing::debug!(%target, %address, %error, "cannot connect upstream");
                last_error = ConnectError::Io;
            }
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

    use super::tls::Hello;
    use super::tls::tests::{client_hello, encrypted_hello_extension, server_name_extension};
    use super::{
        NetworkRules, REPORTED_LIMIT, Rules, Target, is_local, loopback_listener, open_tunnel,
        tunnel_verdict,
    };

    fn rules() -> Rules {
        Rules {
            network: NetworkRules::default(),
            reported: Mutex::new(BTreeSet::new()),
        }
    }

    /// Runs `open_tunnel` for `host:port` on a client that sends `bytes`.
    fn open(host: &str, port: u16, bytes: &[u8]) -> Option<Vec<u8>> {
        open_reporting(host, port, bytes).0
    }

    /// Like [`open`], with the messages the refusal reported.
    fn open_reporting(host: &str, port: u16, bytes: &[u8]) -> (Option<Vec<u8>>, Vec<String>) {
        let (listener, listen_port) = loopback_listener().unwrap();
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).unwrap();
        client.write_all(bytes).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let target = Target {
            host: host.to_owned(),
            port,
        };
        let rules = rules();
        let opened = open_tunnel(&rules, &mut server, &target, Vec::new()).unwrap();
        let reported = rules.reported.into_inner().unwrap().into_iter().collect();
        (opened, reported)
    }

    #[test]
    fn decides_every_kind_of_tls_tunnel_start() {
        let named = |name: &str| client_hello(&[server_name_extension(name)]);
        let encrypted_named = client_hello(&[
            server_name_extension("api.example.com"),
            encrypted_hello_extension(),
        ]);
        let encrypted_nameless = client_hello(&[encrypted_hello_extension()]);
        let nameless = client_hello(&[]);
        let plain = b"GET / HTTP/1.1\r\n\r\n".to_vec();
        // Host, first bytes, and the refusal, or `None` when the tunnel opens.
        let cases: [(&str, Vec<u8>, Option<&str>); 10] = [
            (
                "api.example.com",
                plain,
                Some("denied a tunnel to api.example.com:443 that did not start TLS"),
            ),
            (
                "api.example.com",
                encrypted_named,
                Some("denied an encrypted TLS hello through a tunnel to api.example.com:443"),
            ),
            (
                "10.0.0.5",
                encrypted_nameless,
                Some("denied an encrypted TLS hello through a tunnel to 10.0.0.5:443"),
            ),
            ("10.0.0.5", nameless.clone(), None),
            ("::1", nameless.clone(), None),
            (
                "api.example.com",
                nameless,
                Some("denied TLS without a server name through a tunnel to api.example.com:443"),
            ),
            ("api.example.com", named("api.example.com"), None),
            ("api.example.com.", named("API.Example.com"), None),
            (
                "api.example.com",
                named("evil.example"),
                Some("denied TLS to evil.example through a tunnel to api.example.com:443"),
            ),
            (
                "10.0.0.5",
                named("evil.example"),
                Some("denied TLS to evil.example through a tunnel to 10.0.0.5:443"),
            ),
        ];

        for (host, bytes, refusal) in cases {
            let (opened, reported) = open_reporting(host, 443, &bytes);

            match refusal {
                None => {
                    assert_eq!(opened, Some(bytes), "{host}");
                    assert!(reported.is_empty(), "{host}: {reported:?}");
                }
                Some(refusal) => {
                    assert_eq!(opened, None, "{host}");
                    assert_eq!(reported, [refusal], "{host}");
                }
            }
        }
    }

    #[test]
    fn leaves_tunnels_to_other_ports_unread() {
        assert_eq!(open("github.com", 22, b"SSH-2.0-x\r\n"), Some(Vec::new()));
    }

    #[test]
    fn judges_a_tls_hello_against_the_tunnel_host() {
        let tls = |server_name: Option<&str>, encrypted| Hello::Tls {
            server_name: server_name.map(str::to_owned),
            encrypted,
        };
        let target = |host: &str| Target {
            host: host.to_owned(),
            port: 443,
        };
        let cases = [
            (
                Hello::Other,
                "a.example",
                Err("denied a tunnel to a.example:443 that did not start TLS"),
            ),
            (
                tls(Some("a.example"), true),
                "a.example",
                Err("denied an encrypted TLS hello through a tunnel to a.example:443"),
            ),
            (
                tls(None, true),
                "10.0.0.5",
                Err("denied an encrypted TLS hello through a tunnel to 10.0.0.5:443"),
            ),
            (tls(None, false), "10.0.0.5", Ok(())),
            (tls(None, false), "::1", Ok(())),
            (
                tls(None, false),
                "a.example",
                Err("denied TLS without a server name through a tunnel to a.example:443"),
            ),
            (tls(Some("A.Example."), false), "a.example", Ok(())),
            (
                tls(Some("b.example"), false),
                "a.example",
                Err("denied TLS to b.example through a tunnel to a.example:443"),
            ),
            (
                tls(Some("b.example"), false),
                "10.0.0.5",
                Err("denied TLS to b.example through a tunnel to 10.0.0.5:443"),
            ),
        ];

        for (hello, host, expected) in cases {
            assert_eq!(
                tunnel_verdict(&hello, &target(host)),
                expected.map_err(str::to_owned),
                "{hello:?} to {host}"
            );
        }
    }

    #[test]
    fn reports_a_denial_once() {
        let sut = rules();

        sut.report("denied connection to a:1");
        sut.report("denied connection to a:1");

        assert_eq!(sut.reported.lock().unwrap().len(), 1);
    }

    #[test]
    fn stops_remembering_denials_past_the_limit() {
        let sut = rules();

        for index in 0..=REPORTED_LIMIT {
            sut.report(&format!("denied connection to host{index}:443"));
        }

        assert_eq!(sut.reported.lock().unwrap().len(), REPORTED_LIMIT);
    }

    #[test]
    fn writes_a_target_as_host_and_port() {
        let target = Target {
            host: "::1".to_owned(),
            port: 3000,
        };

        assert_eq!(target.to_string(), "::1:3000");
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
