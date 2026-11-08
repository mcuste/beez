//! Workflow manifest parsing and static validation.

mod manifest;
mod sandbox;
mod schedule;

pub use manifest::{Manifest, ManifestError, load};
pub use schedule::{JobSchedule, Overlap};
