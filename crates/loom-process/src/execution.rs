use std::ffi::OsString;
use std::path::PathBuf;

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
    pub(crate) fn into_parts(self) -> (PathBuf, Vec<OsString>) {
        match self {
            Self::Harness(call) => call.into_parts(),
            Self::Command(call) => call.into_parts(),
        }
    }
}
