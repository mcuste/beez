//! Helpers every CLI test binary shares.
//!
//! Each binary declares this module `pub`, so a helper one binary leaves
//! unused raises no dead-code warning there.

use std::path::Path;
use std::process::Command;

use beez_test_support::TemporaryDirectory;

/// The `beez` binary under test, ready for its arguments.
///
/// Tracing stays off, even when the shell that runs the tests turns it on,
/// so every test sees Beez's own output alone.
pub fn beez() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_beez"));
    command.env_remove("BEEZ_TRACE");
    command
}

/// `beez` run from `directory`, logging into its `logs` subdirectory.
pub fn beez_in(directory: &TemporaryDirectory) -> Command {
    let mut command = beez();
    command
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"));
    command
}

/// `beez` with `arguments`, using `root` as its Beez root.
pub fn beez_at_root(root: &Path, arguments: &[&str]) -> Command {
    let mut command = beez();
    command.args(arguments).arg("--root").arg(root);
    command
}
