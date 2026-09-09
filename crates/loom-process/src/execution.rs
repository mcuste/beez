use std::ffi::OsString;
use std::path::PathBuf;

use loom_core::HeadlessHarness;

use crate::harness::HarnessCall;
use crate::process_call::ProcessCall;

/// A process invocation Loom can run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionRequest {
    /// Sends a prompt to a coding harness.
    Harness(HarnessCall),
    /// Runs a direct process call.
    Command(ProcessCall),
}

impl ExecutionRequest {
    /// The harness this request prompts, if any.
    #[must_use]
    pub fn harness(&self) -> Option<HeadlessHarness> {
        match self {
            Self::Harness(call) => Some(call.harness()),
            Self::Command(_) => None,
        }
    }

    pub(crate) fn into_parts(self) -> (PathBuf, Vec<OsString>) {
        match self {
            Self::Harness(call) => call.into_parts(),
            Self::Command(call) => call.into_parts(),
        }
    }
}
