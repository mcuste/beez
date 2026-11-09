use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use beez_core::{
    Completion, ExecutionError, OutputReference, Task, TaskIndex, TaskOutput, TaskRequest,
    Workflow, WorkflowExecution,
};
use beez_policy::SandboxPolicy;
use beez_process::{
    Cancellation, ExecutionRequest, HarnessCall, OutputStream, ProcessCall, ProcessError,
    ProcessOutput, ProcessRunner,
};

/// Reports why a request or a workflow could not run to the end.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A request could not start or capture its output.
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// The event callback failed, so the run stopped.
    #[error(transparent)]
    Report(#[from] io::Error),
    /// A task reads the output of a task that left none.
    #[error("{0} has no output yet")]
    MissingOutput(OutputReference),
    /// The workflow's own bookkeeping refused a step.
    #[error(transparent)]
    Execution(#[from] ExecutionError),
    /// A thread that ran a request panicked.
    #[error("workflow task execution thread panicked")]
    Panicked,
    /// A request ended without reporting its outcome.
    #[error("request produced no result")]
    NoResult,
}

impl RunError {
    /// The category of the underlying I/O error, or `Other` when there is none.
    #[must_use]
    pub fn kind(&self) -> io::ErrorKind {
        match self {
            Self::Process(error) => error.kind(),
            Self::Report(error) => error.kind(),
            Self::MissingOutput(_) | Self::Execution(_) | Self::Panicked | Self::NoResult => {
                io::ErrorKind::Other
            }
        }
    }
}

/// Reports a process execution lifecycle event.
///
/// An event borrows its payload, so a callback that keeps it must copy it.
#[derive(Clone, Copy, Debug)]
pub enum RunEvent<'event> {
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
        line: &'event [u8],
    },
    /// A request completed and captured its streams.
    Finished {
        /// Workflow task, if this request belongs to a workflow.
        task: Option<TaskIndex>,
        /// Captured process result.
        output: &'event ProcessOutput,
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
    /// A workflow task never ran, because a dependency failed or another task
    /// could not start.
    Blocked {
        /// Workflow task that stays pending.
        task: TaskIndex,
    },
}

impl RunEvent<'_> {
    /// The position of the task this event belongs to.
    ///
    /// A direct request has one task of its own, at position zero.
    #[must_use]
    pub fn position(&self) -> usize {
        let task = match self {
            Self::Started { task }
            | Self::Output { task, .. }
            | Self::Finished { task, .. }
            | Self::Failed { task, .. } => *task,
            Self::Blocked { task } => Some(*task),
        };
        task.map_or(0, TaskIndex::position)
    }
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
        on_event: &mut impl FnMut(&RunEvent<'_>) -> io::Result<()>,
    ) -> Result<i32, RunError> {
        let _run = tracing::info_span!("request").entered();
        tracing::info!(sandboxed = sandbox.is_some(), "request started");
        on_event(&RunEvent::Started { task: None })?;
        let job = Job {
            task: None,
            request,
            sandbox,
            span: tracing::Span::current(),
        };
        let mut outcomes = run_requests_concurrently(&self.processes, vec![job], on_event)?;
        let Some(outcome) = outcomes.pop() else {
            return Err(RunError::NoResult);
        };
        Ok(exit_status(&outcome?))
    }

    /// Runs ready workflow tasks concurrently until completion.
    pub fn run_workflow(
        &self,
        workflow: &Workflow,
        on_event: &mut impl FnMut(&RunEvent<'_>) -> io::Result<()>,
    ) -> Result<i32, RunError> {
        let _run = tracing::info_span!("workflow", tasks = workflow.tasks().len()).entered();
        tracing::info!("workflow started");
        let mut run = WorkflowRun::new(workflow);
        while run.execution.has_pending() {
            let tasks = run.start_ready(on_event)?;

            // Tasks stay pending only when a dependency failed, which already set the status.
            if tasks.is_empty() {
                run.block_pending(on_event)?;
                tracing::info!(status = run.exit_status(), "workflow ended early");
                return Ok(run.exit_status());
            }

            let jobs = match run.jobs(&tasks) {
                Ok(jobs) => jobs,
                Err(error) => return run.abandon(&tasks, error, on_event),
            };
            let outcomes = run_requests_concurrently(&self.processes, jobs, on_event)?;
            let finished = tasks.iter().map(|(index, _)| *index).zip(outcomes);

            // The run stops here, so no pending task will ever start.
            if let Err(error) = run.record(finished) {
                tracing::warn!(%error, "workflow stopped on an execution error");
                run.block_pending(on_event)?;
                return Err(error);
            }
        }
        tracing::info!(status = run.exit_status(), "workflow finished");
        Ok(run.exit_status())
    }
}

/// The state of one workflow run.
struct WorkflowRun<'workflow> {
    workflow: &'workflow Workflow,
    execution: WorkflowExecution<'workflow>,
    /// Captured output of finished tasks that a later task reads.
    outputs: HashMap<TaskIndex, ProcessOutput>,
    /// The status of the first failing task, in declaration order.
    first_failure: Option<i32>,
}

impl<'workflow> WorkflowRun<'workflow> {
    fn new(workflow: &'workflow Workflow) -> Self {
        Self {
            workflow,
            execution: workflow.execution(),
            outputs: HashMap::new(),
            first_failure: None,
        }
    }

    /// The status the workflow exits with: the first failure's, or zero.
    fn exit_status(&self) -> i32 {
        self.first_failure.unwrap_or(0)
    }

    /// Marks ready tasks as running and returns them.
    fn start_ready(
        &mut self,
        on_event: &mut impl FnMut(&RunEvent<'_>) -> io::Result<()>,
    ) -> io::Result<Vec<StartedTask<'workflow>>> {
        let tasks = self.execution.start_ready();
        for (index, task) in &tasks {
            tracing::info!(task = %task.id(), "task started");
            on_event(&RunEvent::Started { task: Some(*index) })?;
        }
        Ok(tasks)
    }

    /// The requests of started tasks, with their output placeholders filled.
    fn jobs(&self, tasks: &[StartedTask<'workflow>]) -> Result<Vec<Job<'workflow>>, RunError> {
        tasks
            .iter()
            .map(|(index, task)| {
                let request = render_request(task, &self.outputs)?;
                Ok(Job {
                    task: Some(*index),
                    request: execution_request(request),
                    sandbox: task.sandbox(),
                    span: tracing::info_span!("task", task = %task.id()),
                })
            })
            .collect()
    }

    /// Ends a run whose started tasks cannot run: each fails with `error`,
    /// and every pending task is blocked, as after any other execution error.
    fn abandon(
        &mut self,
        tasks: &[StartedTask<'workflow>],
        error: RunError,
        on_event: &mut impl FnMut(&RunEvent<'_>) -> io::Result<()>,
    ) -> Result<i32, RunError> {
        tracing::warn!(%error, "cannot build the requests of started tasks");
        for (index, _) in tasks {
            self.execution.complete(*index, Completion::Failed)?;
            on_event(&RunEvent::Failed {
                task: Some(*index),
                error_kind: error.kind(),
                elapsed: Duration::ZERO,
            })?;
        }
        self.block_pending(on_event)?;
        Err(error)
    }

    /// Records task outcomes, in declaration order, and picks the workflow
    /// exit status.
    ///
    /// The order keeps the reported status and error the same from run to run.
    fn record(
        &mut self,
        finished: impl IntoIterator<Item = (TaskIndex, Outcome)>,
    ) -> Result<(), RunError> {
        let mut execution_error = None;

        for (index, outcome) in finished {
            self.execution.complete(index, completion(&outcome))?;
            match outcome {
                Ok(output) => {
                    if !output.succeeded() && self.first_failure.is_none() {
                        tracing::debug!(
                            task = %self.task_id(index),
                            status = exit_status(&output),
                            "first failing task sets the workflow status"
                        );
                        self.first_failure = Some(exit_status(&output));
                    }
                    if self.workflow.is_read(index) {
                        self.outputs.insert(index, output);
                    }
                }
                Err(error) => {
                    execution_error = execution_error.or(Some(error));
                }
            }
        }

        execution_error.map_or(Ok(()), |error| Err(RunError::Process(error)))
    }

    /// Reports every task that has not started as blocked.
    fn block_pending(
        &self,
        on_event: &mut impl FnMut(&RunEvent<'_>) -> io::Result<()>,
    ) -> io::Result<()> {
        self.execution.pending().into_iter().try_for_each(|index| {
            tracing::info!(task = %self.task_id(index), "task blocked");
            on_event(&RunEvent::Blocked { task: index })
        })
    }

    /// The ID of a task, for trace lines.
    fn task_id(&self, index: TaskIndex) -> String {
        self.workflow.task(index).map_or_else(
            || format!("#{}", index.position()),
            |task| task.id().to_string(),
        )
    }
}

/// Fills the output placeholders of a task's request from the finished tasks.
fn render_request(
    task: &Task,
    outputs: &HashMap<TaskIndex, ProcessOutput>,
) -> Result<TaskRequest, RunError> {
    task.render_request(|source, stream| {
        outputs
            .get(&source)
            .map(|output| output_text(output, stream))
    })
    .map_err(RunError::MissingOutput)
}

/// One captured stream as text, without the newlines that end it.
fn output_text(output: &ProcessOutput, stream: TaskOutput) -> String {
    String::from_utf8_lossy(output.stream(output_stream(stream)))
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

/// The process stream a placeholder's task output names.
fn output_stream(output: TaskOutput) -> OutputStream {
    match output {
        TaskOutput::Stdout => OutputStream::Stdout,
        TaskOutput::Stderr => OutputStream::Stderr,
    }
}

/// A started task and its position.
type StartedTask<'workflow> = (TaskIndex, &'workflow Task);

/// One request to run.
#[derive(Debug)]
struct Job<'policy> {
    /// Workflow task, if this request belongs to a workflow.
    task: Option<TaskIndex>,
    request: ExecutionRequest,
    sandbox: Option<&'policy SandboxPolicy>,
    /// The span the request runs in, on its own thread.
    span: tracing::Span,
}

/// What a request thread sends back while it runs.
enum Message {
    /// One output line of a running request.
    Line {
        task: Option<TaskIndex>,
        stream: OutputStream,
        line: Vec<u8>,
    },
    /// A request ended.
    Done {
        /// Position of the request's job.
        job: usize,
        task: Option<TaskIndex>,
        outcome: Outcome,
        elapsed: Duration,
    },
}

/// A finished request's captured output, or the error that stopped it.
type Outcome = Result<ProcessOutput, ProcessError>;

/// Whether a task counts as succeeded. A signal or an execution error counts as a failure.
fn completion(outcome: &Outcome) -> Completion {
    match outcome {
        Ok(output) if output.succeeded() => Completion::Succeeded,
        _ => Completion::Failed,
    }
}

/// The status a finished request counts as. A signal counts as 1.
fn exit_status(output: &ProcessOutput) -> i32 {
    output.status_code().unwrap_or(1)
}

/// Runs requests concurrently, reporting each line and each result as it arrives.
///
/// Outcomes come back in the order of `jobs`.
fn run_requests_concurrently(
    runner: &ProcessRunner,
    jobs: Vec<Job<'_>>,
    on_event: &mut impl FnMut(&RunEvent<'_>) -> io::Result<()>,
) -> Result<Vec<Outcome>, RunError> {
    let (sender, receiver) = mpsc::channel::<Message>();
    let mut outcomes = jobs.iter().map(|_| None).collect::<Vec<Option<Outcome>>>();
    let cancel = Cancellation::new();

    std::thread::scope(|scope| {
        let handles = jobs
            .into_iter()
            .enumerate()
            .map(
                |(
                    job,
                    Job {
                        task,
                        request,
                        sandbox,
                        span,
                    },
                )| {
                    let sender = sender.clone();
                    let cancel = &cancel;
                    scope.spawn(move || {
                        let _span = span.entered();
                        let started = Instant::now();
                        let sink = |stream, line: &[u8]| {
                            let line = line.to_vec();
                            let _ = sender.send(Message::Line { task, stream, line });
                        };
                        let outcome = runner.run_cancellable(request, sandbox, &sink, cancel);
                        trace_outcome(&outcome, started.elapsed());
                        // The reader threads have ended, so no line follows this.
                        let _ = sender.send(Message::Done {
                            job,
                            task,
                            outcome,
                            elapsed: started.elapsed(),
                        });
                    })
                },
            )
            .collect::<Vec<_>>();
        drop(sender);

        // Reporting runs here, because the callback belongs to the caller's thread.
        // A callback that fails ends the run, so nothing waits for the
        // requests that are still running.
        if let Err(error) = report(receiver, &mut outcomes, on_event) {
            tracing::warn!(%error, "reporting failed, cancelling the running requests");
            cancel.cancel();
            return Err(RunError::Report(error));
        }

        for handle in handles {
            handle.join().map_err(|_| RunError::Panicked)?;
        }
        Ok(())
    })?;

    outcomes
        .into_iter()
        .map(|outcome| outcome.ok_or(RunError::NoResult))
        .collect()
}

/// Traces how one request ended.
fn trace_outcome(outcome: &Outcome, elapsed: Duration) {
    match outcome {
        Ok(output) => tracing::info!(
            status = ?output.status_code(),
            succeeded = output.succeeded(),
            ?elapsed,
            "finished"
        ),
        Err(error) => tracing::warn!(%error, ?elapsed, "could not run"),
    }
}

/// Reports every message until each request has ended, keeping each
/// outcome in the slot of its job.
fn report(
    receiver: mpsc::Receiver<Message>,
    outcomes: &mut [Option<Outcome>],
    on_event: &mut impl FnMut(&RunEvent<'_>) -> io::Result<()>,
) -> io::Result<()> {
    for message in receiver {
        match message {
            Message::Line { task, stream, line } => {
                on_event(&RunEvent::Output {
                    task,
                    stream,
                    line: &line,
                })?;
            }
            Message::Done {
                job,
                task,
                outcome,
                elapsed,
            } => {
                report_done(task, &outcome, elapsed, on_event)?;
                if let Some(slot) = outcomes.get_mut(job) {
                    *slot = Some(outcome);
                }
            }
        }
    }
    Ok(())
}

/// Reports a request's end.
fn report_done(
    task: Option<TaskIndex>,
    outcome: &Outcome,
    elapsed: Duration,
    on_event: &mut impl FnMut(&RunEvent<'_>) -> io::Result<()>,
) -> io::Result<()> {
    on_event(&match outcome {
        Ok(output) => RunEvent::Finished {
            task,
            output,
            elapsed,
        },
        Err(error) => RunEvent::Failed {
            task,
            error_kind: error.kind(),
            elapsed,
        },
    })
}

fn execution_request(request: TaskRequest) -> ExecutionRequest {
    match request {
        TaskRequest::Harness {
            harness,
            prompt,
            options,
        } => ExecutionRequest::Harness(HarnessCall::new(harness, prompt).options(options)),
        TaskRequest::Command { program, arguments } => {
            ExecutionRequest::Command(ProcessCall::new(program).arguments(arguments))
        }
    }
}

#[cfg(test)]
mod tests {
    use beez_core::{Completion, TaskDefinition, TaskRequest, Workflow};

    use super::{RunEvent, WorkflowRun};

    fn task(id: &str, depends_on: &[&str], arguments: &[&str]) -> TaskDefinition {
        TaskDefinition::new(
            id.parse().unwrap(),
            depends_on.iter().map(|id| id.parse().unwrap()).collect(),
            TaskRequest::command(
                "echo".to_owned(),
                arguments
                    .iter()
                    .map(|&argument| argument.to_owned())
                    .collect(),
            ),
        )
    }

    /// A request that cannot be built ends the run like any execution error:
    /// its task fails and every pending task is blocked.
    #[test]
    fn blocks_pending_tasks_when_a_request_cannot_be_built() {
        let workflow = Workflow::try_from(vec![
            task("inspect", &[], &[]),
            task("review", &["inspect"], &["{{ tasks.inspect.stdout }}"]),
            task("test", &["review"], &[]),
        ])
        .unwrap();
        let mut run = WorkflowRun::new(&workflow);
        let mut events = Vec::new();
        let mut on_event = |event: &RunEvent<'_>| {
            events.push(format!("{event:?}"));
            Ok(())
        };
        // `inspect` succeeds without leaving the output `review` reads.
        for (index, _) in run.start_ready(&mut on_event).unwrap() {
            run.execution
                .complete(index, Completion::Succeeded)
                .unwrap();
        }
        let review = run.start_ready(&mut on_event).unwrap();
        let error = run.jobs(&review).unwrap_err();

        let error = run.abandon(&review, error, &mut on_event).unwrap_err();

        assert_eq!(
            error.to_string(),
            "{{ tasks.inspect.stdout }} has no output yet"
        );
        assert_eq!(
            events,
            [
                "Started { task: Some(TaskIndex(0)) }",
                "Started { task: Some(TaskIndex(1)) }",
                "Failed { task: Some(TaskIndex(1)), error_kind: Other, elapsed: 0ns }",
                "Blocked { task: TaskIndex(2) }",
            ]
        );
    }
}
