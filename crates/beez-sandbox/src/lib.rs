//! Operating-system sandboxes for Beez task processes.

#[cfg(target_os = "linux")]
mod bubblewrap;
mod diagnostic;
#[cfg(target_os = "linux")]
mod init;
mod init_request;
#[cfg(any(target_os = "linux", test))]
mod mask_marker;
mod proxy;
mod relay;
mod resolve;
mod sandbox;
#[cfg(target_os = "macos")]
mod seatbelt;
mod server;
mod stream;

pub use diagnostic::set_diagnostic_sink;
#[cfg(target_os = "linux")]
pub use init::{InitError, init, relay};
pub use init_request::{
    EXEC_FLAG, ExecRestriction, InitRequest, RELAY_FLAG, UNRESTRICTED_EXEC_FLAG,
};
pub use proxy::{NetworkRules, Proxy};
pub use relay::Relay;
pub use sandbox::{SandboxError, SandboxedCommand};
