use std::ffi::OsString;
use std::path::Path;

/// `directory` before the host `PATH`, which the sandbox needs for its own tools.
#[must_use]
pub fn extended_path(directory: &Path) -> OsString {
    let mut path = OsString::from(directory);
    if let Some(host) = std::env::var_os("PATH") {
        path.push(":");
        path.push(host);
    }

    path
}
