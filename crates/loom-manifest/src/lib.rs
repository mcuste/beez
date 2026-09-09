//! Workflow manifest parsing and static validation.

mod manifest;
mod sandbox;

pub use manifest::{ManifestError, load};
