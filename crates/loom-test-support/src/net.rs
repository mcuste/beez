use std::io;
use std::net::{Ipv4Addr, TcpListener};

/// A listener on an ephemeral loopback port, with the port it got.
pub fn loopback_listener() -> io::Result<(TcpListener, u16)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// A loopback listener that never accepts, to check that nothing reaches it.
pub fn unused_origin() -> io::Result<(u16, TcpListener)> {
    let (listener, port) = loopback_listener()?;
    listener.set_nonblocking(true)?;
    Ok((port, listener))
}

/// Checks that nothing connected to `origin`.
///
/// # Panics
///
/// Panics when a connection reached the listener.
pub fn assert_never_reached(origin: &TcpListener) {
    let refused = origin.accept().err();
    assert!(
        refused.is_some_and(|error| error.kind() == io::ErrorKind::WouldBlock),
        "a connection reached the origin"
    );
}
