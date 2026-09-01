//! Process execution for coding harnesses and direct process calls.

mod execution;
mod harness;
mod process_call;
mod runner;

pub use execution::ExecutionRequest;
pub use harness::HarnessCall;
pub use process_call::ProcessCall;
pub use runner::{ProcessOutput, ProcessRunner};
