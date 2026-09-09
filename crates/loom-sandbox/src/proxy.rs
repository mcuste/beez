use std::collections::BTreeSet;
use std::io;
#[cfg(target_os = "linux")]
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use loom_core::DomainRule;

use crate::server::Server;

mod http;
mod socks;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Bounds the handshake only. An established tunnel has no idle timeout,
/// because a streaming response can stay silent for a long time.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Loopback HTTP and SOCKS5 proxies that only connect to allowed hosts.
///
/// Both proxies see the target host name before any payload flows, so the
/// allowlist applies without inspecting traffic. Denied targets get a refusal
/// and a line on Loom's stderr.
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
        let http = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let socks = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let http_port = http.local_addr()?.port();
        let socks_port = socks.local_addr()?.port();
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

/// The allowlist plus the targets already reported as denied.
struct Rules {
    domains: Vec<DomainRule>,
    localhost: bool,
    reported: Mutex<BTreeSet<String>>,
}

impl Rules {
    /// True when a rule allows the target. Reports each denied target once.
    fn permits(&self, host: &str, port: u16) -> bool {
        if self.domains.iter().any(|rule| rule.matches(host, port)) {
            return true;
        }
        self.report(&format!("{host}:{port}"));
        false
    }

    /// True when the policy allows connections to `address`.
    fn permits_address(&self, host: &str, address: IpAddr) -> bool {
        if self.localhost || !is_local(address) {
            return true;
        }
        self.report(&format!("{host} ({address})"));
        false
    }

    /// Writes one line per denied target on Loom's stderr.
    fn report(&self, target: &str) {
        let first_time = self
            .reported
            .lock()
            .map_or(true, |mut reported| reported.insert(target.to_owned()));
        if first_time {
            crate::diagnostic::report(&format!("denied connection to {target}"));
        }
    }
}

/// True for addresses that reach this machine or its link.
///
/// Link-local addresses count, because they carry the cloud metadata service.
/// Private ranges do not: a task that names an internal host reaches it.
fn is_local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_broadcast()
        }
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return is_local(IpAddr::V4(mapped));
            }
            let leading = address.segments().first().copied().unwrap_or_default();
            // fe80::/10 is link local.
            address.is_loopback() || address.is_unspecified() || leading & 0xffc0 == 0xfe80
        }
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
