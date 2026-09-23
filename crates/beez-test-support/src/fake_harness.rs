use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::directory::TemporaryDirectory;

/// Writes an executable harness stub into `directory`.
///
/// The stub prints each argument it receives as `[argument]` on stdout and
/// stderr, then exits with `status`, so tests can assert argument boundaries.
pub fn fake_harness(
    directory: &TemporaryDirectory,
    name: &str,
    status: i32,
) -> io::Result<PathBuf> {
    let program = directory.join(name);
    fs::write(
        &program,
        format!("#!/bin/sh\nprintf '[%s]' \"$@\"\nprintf '[%s]' \"$@\" >&2\nexit {status}\n"),
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;

    Ok(program)
}
