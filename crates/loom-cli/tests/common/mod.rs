//! Helpers every CLI test binary shares.

use std::process::Command;

/// The `loom` binary under test, ready for its arguments.
pub(crate) fn loom() -> Command {
    Command::new(env!("CARGO_BIN_EXE_loom"))
}
