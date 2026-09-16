//! Sandbox policy a task declares, and the harnesses it can run.

mod domain;
mod executable;
mod filesystem;
mod group;
mod harness;
mod named;
mod network;
mod parse;
mod path;
mod profile;
mod sandbox;

pub use domain::{DomainRule, DomainRuleError};
pub use executable::ExecutablePolicy;
pub use filesystem::FilesystemPolicy;
pub(crate) use group::PathGroup;
pub use group::{DomainGroup, ExecutableGroup, GroupError};
pub use harness::{HarnessOptions, HeadlessHarness, HeadlessHarnessError};
pub use network::NetworkPolicy;
pub use parse::parse_all;
pub use path::{SandboxPath, SandboxPathError};
pub use profile::HarnessProfile;
pub use sandbox::SandboxPolicy;
pub(crate) use sandbox::extend;
