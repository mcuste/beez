use std::ffi::OsString;
use std::io;

use loom_core::{SandboxPolicy, TaskIndex, TaskRequest, Workflow, WorkflowExecution};
use loom_process::{ExecutionRequest, HarnessCall, ProcessCall, ProcessOutput, ProcessRunner};

/// Reports a process execution lifecycle event.
#[derive(Debug)]
pub enum RunEvent {
    /// A request has started.
    Started {
        /// Workflow task, if this request belongs to a workflow.
        task: Option<TaskIndex>,
    },
    /// A request completed and captured its streams.
    Finished {
        /// Workflow task, if this request belongs to a workflow.
        task: Option<TaskIndex>,
        /// Captured process result.
        output: ProcessOutput,
    },
    /// A request could not start or capture its output.
    Failed {
        /// Workflow task, if this request belongs to a workflow.
        task: Option<TaskIndex>,
        /// Category of the execution error.
        error_kind: io::ErrorKind,
    },
}

/// Runs direct requests and validated workflows.
#[derive(Clone, Copy, Debug, Default)]
pub struct Runner;

impl Runner {
    /// Runs one request without a sandbox.
    pub fn run_request(
        &self,
        request: ExecutionRequest,
        on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
    ) -> io::Result<i32> {
        self.run_request_in(request, None, on_event)
    }

    /// Runs one request, inside a sandbox when a policy is given.
    pub fn run_request_in(
        &self,
        request: ExecutionRequest,
        sandbox: Option<&SandboxPolicy>,
        on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
    ) -> io::Result<i32> {
        on_event(&RunEvent::Started { task: None })?;
        match run_process(request, sandbox) {
            Ok(output) => {
                let status = output.status_code().unwrap_or(1);
                on_event(&RunEvent::Finished { task: None, output })?;
                Ok(status)
            }
            Err(error) => {
                on_event(&RunEvent::Failed {
                    task: None,
                    error_kind: error.kind(),
                })?;
                Err(error)
            }
        }
    }

    /// Runs ready workflow tasks concurrently until completion.
    pub fn run_workflow(
        &self,
        workflow: &Workflow,
        on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
    ) -> io::Result<i32> {
        let mut execution = workflow.execution();
        let mut exit_status = 0;
        while execution.has_pending() {
            let requests = start_ready_tasks(&mut execution, on_event)?;

            // Tasks stay pending only when a dependency failed, which already set the status.
            if requests.is_empty() {
                return Ok(exit_status);
            }

            let results = run_requests_concurrently(requests)?;

            finish_tasks(&mut execution, results, on_event, &mut exit_status)?;
        }
        Ok(exit_status)
    }
}

/// A started task's request and sandbox.
struct StartedTask {
    index: TaskIndex,
    request: TaskRequest,
    sandbox: Option<SandboxPolicy>,
}

/// Marks ready tasks as running and returns their requests.
fn start_ready_tasks(
    execution: &mut WorkflowExecution<'_>,
    on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
) -> io::Result<Vec<StartedTask>> {
    execution
        .ready()
        .into_iter()
        .map(|index| {
            let task = execution
                .start(index)
                .ok_or_else(|| io::Error::other("workflow task is not ready"))?;
            on_event(&RunEvent::Started { task: Some(index) })?;
            Ok(StartedTask {
                index,
                request: task.request().clone(),
                sandbox: task.sandbox().cloned(),
            })
        })
        .collect()
}

fn run_process(
    request: ExecutionRequest,
    sandbox: Option<&SandboxPolicy>,
) -> io::Result<ProcessOutput> {
    match sandbox {
        Some(policy) => ProcessRunner.run_sandboxed(request, policy),
        None => ProcessRunner.run(request),
    }
}

/// Runs task requests concurrently.
fn run_requests_concurrently(
    tasks: Vec<StartedTask>,
) -> io::Result<Vec<(TaskIndex, io::Result<ProcessOutput>)>> {
    std::thread::scope(|scope| {
        tasks
            .into_iter()
            .map(|task| {
                scope.spawn(move || {
                    let request = execution_request(task.request);
                    (task.index, run_process(request, task.sandbox.as_ref()))
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| io::Error::other("workflow task execution thread panicked"))
            })
            .collect()
    })
}

/// Records task results and reports completion events.
fn finish_tasks(
    execution: &mut WorkflowExecution<'_>,
    results: Vec<(TaskIndex, io::Result<ProcessOutput>)>,
    on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
    exit_status: &mut i32,
) -> io::Result<()> {
    let mut execution_error = None;

    for (index, result) in results {
        match result {
            Ok(output) => {
                let status = output.status_code().unwrap_or(1);
                if !execution.complete(index, status == 0) {
                    return Err(io::Error::other("workflow task is not running"));
                }
                on_event(&RunEvent::Finished {
                    task: Some(index),
                    output,
                })?;
                if status != 0 && *exit_status == 0 {
                    *exit_status = status;
                }
            }
            Err(error) => {
                if !execution.complete(index, false) {
                    return Err(io::Error::other("workflow task is not running"));
                }
                on_event(&RunEvent::Failed {
                    task: Some(index),
                    error_kind: error.kind(),
                })?;
                if execution_error.is_none() {
                    execution_error = Some(error);
                }
            }
        }
    }

    match execution_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn execution_request(request: TaskRequest) -> ExecutionRequest {
    match request {
        TaskRequest::Harness {
            harness,
            prompt,
            options,
        } => {
            let mut call = HarnessCall::new(harness, OsString::from(prompt));
            if let Some(model) = options.model() {
                call = call.model(model);
            }
            if let Some(effort) = options.effort() {
                call = call.effort(effort);
            }
            ExecutionRequest::Harness(call)
        }
        TaskRequest::Command { program, arguments } => ExecutionRequest::Command(
            arguments
                .into_iter()
                .fold(ProcessCall::new(program), |call, argument| {
                    call.argument(argument)
                }),
        ),
    }
}
