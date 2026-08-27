//! Validated workflow identifiers and dependency DAGs.

mod task;
mod workflow;

pub use task::{Task, TaskDefinition, TaskId, TaskIdError, TaskIndex};
pub use workflow::{Workflow, WorkflowError};
