//! Helpers every CLI test binary shares.

use std::process::Command;

/// The `beez` binary under test, ready for its arguments.
pub(crate) fn beez() -> Command {
    Command::new(env!("CARGO_BIN_EXE_beez"))
}
