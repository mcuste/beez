use std::io;
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
    directory.executable(
        name,
        format!("#!/bin/sh\nprintf '[%s]' \"$@\"\nprintf '[%s]' \"$@\" >&2\nexit {status}\n"),
    )
}
