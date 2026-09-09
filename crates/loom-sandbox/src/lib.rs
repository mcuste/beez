//! Operating-system sandboxes for Loom task processes.

#[cfg(target_os = "linux")]
mod bubblewrap;
#[cfg(target_os = "linux")]
mod init;
mod proxy;
mod resolve;
mod sandbox;
#[cfg(target_os = "macos")]
mod seatbelt;
mod server;
mod stream;

#[cfg(target_os = "linux")]
pub use init::{init, parse_relay, relay};
pub use proxy::Proxy;
pub use resolve::ResolvedSandbox;
pub use sandbox::SandboxedCommand;
