//! Workflow manifest parsing and static validation.

mod manifest;
mod sandbox;
mod schedule;

pub use manifest::{Manifest, ManifestError, load};
pub use schedule::{JobSchedule, Overlap};

/// The text of an error, because a manifest reports every error as text.
pub(crate) fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}
