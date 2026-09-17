use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use loom_core::{TaskIndex, TaskRequest, Workflow, WorkflowExecution};
use loom_policy::SandboxPolicy;
use loom_process::{
    ExecutionRequest, HarnessCall, OutputStream, ProcessCall, ProcessOutput, ProcessRunner,
};

/// Reports a process execution lifecycle event.
#[derive(Debug)]
pub enum RunEvent {
    /// A request has started.
    Started {
        /// Workflow task, if this request belongs to a workflow.
        task: Option<TaskIndex>,
    },
    /// A running request wrote one output line.
    Output {
        /// Workflow task, if this request belongs to a workflow.
        task: Option<TaskIndex>,
        /// Stream the line came from.
        stream: OutputStream,
        /// Line as the child wrote it, with its newline when there is one.
        line: Vec<u8>,
    },
    /// A request completed and captured its streams.
    Finished {
        /// Workflow task, if this request belongs to a workflow.
        task: Option<TaskIndex>,
        /// Captured process result.
        output: ProcessOutput,
        /// Time the request took.
        elapsed: Duration,
    },
    /// A request could not start or capture its output.
    Failed {
        /// Workflow task, if this request belongs to a workflow.
        task: Option<TaskIndex>,
        /// Category of the execution error.
        error_kind: io::ErrorKind,
        /// Time until the request failed.
        elapsed: Duration,
    },
    /// A workflow task never ran, because a dependency failed.
    Blocked {
        /// Workflow task that stays pending.
        task: TaskIndex,
    },
}

impl RunEvent {
    /// The workflow task this event belongs to, when it belongs to one.
    #[must_use]
    pub fn task(&self) -> Option<TaskIndex> {
        match self {
            Self::Started { task }
            | Self::Output { task, .. }
            | Self::Finished { task, .. }
            | Self::Failed { task, .. } => *task,
            Self::Blocked { task } => Some(*task),
        }
    }

    /// The position of the task this event belongs to.
    ///
    /// A direct request has one task of its own, at position zero.
    #[must_use]
    pub fn position(&self) -> usize {
        position(self.task())
    }
}

/// The position of a workflow task, or zero for a direct request.
fn position(task: Option<TaskIndex>) -> usize {
    task.map_or(0, TaskIndex::position)
}

/// Runs direct requests and validated workflows in one working directory.
#[derive(Clone, Debug)]
pub struct Runner {
    processes: ProcessRunner,
}

impl Runner {
    /// Runs tasks in `working_directory`.
    #[must_use]
    pub fn new(working_directory: impl Into<PathBuf>) -> Self {
        Self {
            processes: ProcessRunner::new(working_directory),
        }
    }

    /// Runs tasks in this process's own working directory.
    pub fn here() -> io::Result<Self> {
        ProcessRunner::here().map(|processes| Self { processes })
    }

    /// Runs one request, inside a sandbox when a policy is given.
    pub fn run_request_in(
        &self,
        request: ExecutionRequest,
        sandbox: Option<&SandboxPolicy>,
        on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
    ) -> io::Result<i32> {
        on_event(&RunEvent::Started { task: None })?;
        let mut outcomes = run_requests_concurrently(
            &self.processes,
            vec![(None, request, sandbox.cloned())],
            on_event,
        )?;
        let Some((_, outcome)) = outcomes.pop() else {
            return Err(io::Error::other("request produced no result"));
        };
        let status = task_status(&outcome);
        outcome.map(|_| status)
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
            let tasks = start_ready_tasks(&mut execution, on_event)?;

            // Tasks stay pending only when a dependency failed, which already set the status.
            if tasks.is_empty() {
                for index in execution.pending() {
                    on_event(&RunEvent::Blocked { task: index })?;
                }
                return Ok(exit_status);
            }

            let jobs = tasks
                .into_iter()
                .map(|task| {
                    (
                        Some(task.index),
                        execution_request(task.request),
                        task.sandbox,
                    )
                })
                .collect();
            let outcomes = run_requests_concurrently(&self.processes, jobs, on_event)?;

            record_outcomes(&mut execution, outcomes, &mut exit_status)?;
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

/// One request to run, with the task it belongs to.
type Job = (Option<TaskIndex>, ExecutionRequest, Option<SandboxPolicy>);

/// What a task thread sends back while it runs.
enum Message {
    /// One output line of a running request.
    Line(Option<TaskIndex>, OutputStream, Vec<u8>),
    /// A request ended.
    Done(Option<TaskIndex>, io::Result<ProcessOutput>, Duration),
}

/// A finished request's exit status, or the error that stopped it.
///
/// `None` inside `Ok` means the process died from a signal.
type Outcome = io::Result<Option<i32>>;

/// The status a request counts as. A signal or an execution error counts as 1.
fn task_status(outcome: &Outcome) -> i32 {
    outcome.as_ref().map_or(1, |status| status.unwrap_or(1))
}

/// Runs requests concurrently, reporting each line and each result as it arrives.
///
/// Outcomes come back in completion order, not in the order of `jobs`.
fn run_requests_concurrently(
    runner: &ProcessRunner,
    jobs: Vec<Job>,
    on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
) -> io::Result<Vec<(Option<TaskIndex>, Outcome)>> {
    let (sender, receiver) = mpsc::channel::<Message>();

    std::thread::scope(|scope| {
        let handles = jobs
            .into_iter()
            .map(|(task, request, sandbox)| {
                // A Sender is not Sync, so the two reader threads share it under a lock.
                let sender = Mutex::new(sender.clone());
                scope.spawn(move || {
                    let started = Instant::now();
                    let sink = |stream, line: &[u8]| {
                        if let Ok(sender) = sender.lock() {
                            let _ = sender.send(Message::Line(task, stream, line.to_vec()));
                        }
                    };
                    let result = runner.run_streaming(request, sandbox.as_ref(), &sink);
                    // The reader threads have ended, so no line follows this.
                    if let Ok(sender) = sender.lock() {
                        let _ = sender.send(Message::Done(task, result, started.elapsed()));
                    }
                })
            })
            .collect::<Vec<_>>();
        drop(sender);

        // Reporting runs here, because the callback belongs to the caller's thread.
        let mut outcomes = Vec::new();
        for message in receiver {
            match message {
                Message::Line(task, stream, line) => {
                    on_event(&RunEvent::Output { task, stream, line })?;
                }
                Message::Done(task, Ok(output), elapsed) => {
                    outcomes.push((task, Ok(output.status_code())));
                    on_event(&RunEvent::Finished {
                        task,
                        output,
                        elapsed,
                    })?;
                }
                Message::Done(task, Err(error), elapsed) => {
                    on_event(&RunEvent::Failed {
                        task,
                        error_kind: error.kind(),
                        elapsed,
                    })?;
                    outcomes.push((task, Err(error)));
                }
            }
        }

        for handle in handles {
            handle
                .join()
                .map_err(|_| io::Error::other("workflow task execution thread panicked"))?;
        }
        Ok(outcomes)
    })
}

/// Records task outcomes and picks the workflow exit status.
///
/// Outcomes arrive in completion order, so this sorts them into declaration
/// order. That keeps the reported status the same from run to run.
fn record_outcomes(
    execution: &mut WorkflowExecution<'_>,
    mut outcomes: Vec<(Option<TaskIndex>, Outcome)>,
    exit_status: &mut i32,
) -> io::Result<()> {
    outcomes.sort_by_key(|(task, _)| position(*task));
    let mut execution_error = None;

    for (task, outcome) in outcomes {
        let Some(index) = task else {
            continue;
        };
        let status = task_status(&outcome);
        if !execution.complete(index, status == 0) {
            return Err(io::Error::other("workflow task is not running"));
        }
        match outcome {
            Ok(_) => {
                if status != 0 && *exit_status == 0 {
                    *exit_status = status;
                }
            }
            Err(error) => {
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
        } => ExecutionRequest::Harness(HarnessCall::new(harness, prompt).options(&options)),
        TaskRequest::Command { program, arguments } => {
            ExecutionRequest::Command(ProcessCall::new(program).arguments(arguments))
        }
    }
}
