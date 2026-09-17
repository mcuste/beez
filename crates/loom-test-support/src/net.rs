use std::io;
use std::net::{Ipv4Addr, TcpListener};

/// A loopback listener that never accepts, to check that nothing reaches it.
pub fn unused_origin() -> io::Result<(u16, TcpListener)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
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
