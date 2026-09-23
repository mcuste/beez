use std::path::Path;

/// The path as text, for a manifest or an assertion message.
#[must_use]
pub fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
