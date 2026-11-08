use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use beez_core::{
    Completion, Task, TaskIndex, TaskOutput, TaskRequest, Workflow, WorkflowExecution,
};
use beez_policy::SandboxPolicy;
use beez_process::{
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
    /// A workflow task never ran, because a dependency failed or another task
    /// could not start.
    Blocked {
        /// Workflow task that stays pending.
        task: TaskIndex,
    },
}

impl RunEvent {
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
        on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
    ) -> io::Result<i32> {
        on_event(&RunEvent::Started { task: None })?;
        let job = Job {
            task: None,
            request,
            sandbox,
        };
        let mut outcomes = run_requests_concurrently(&self.processes, vec![job], on_event)?;
        let Some(outcome) = outcomes.pop() else {
            return Err(io::Error::other("request produced no result"));
        };
        outcome.map(|output| exit_status(&output))
    }

    /// Runs ready workflow tasks concurrently until completion.
    pub fn run_workflow(
        &self,
        workflow: &Workflow,
        on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
    ) -> io::Result<i32> {
        let mut run = WorkflowRun::new(workflow);
        while run.execution.has_pending() {
            let tasks = run.start_ready(on_event)?;

            // Tasks stay pending only when a dependency failed, which already set the status.
            if tasks.is_empty() {
                run.block_pending(on_event)?;
                return Ok(run.exit_status);
            }

            let jobs = run.jobs(&tasks)?;
            let outcomes = run_requests_concurrently(&self.processes, jobs, on_event)?;
            let finished = tasks
                .iter()
                .zip(outcomes)
                .map(|(task, outcome)| FinishedTask {
                    index: task.index,
                    outcome,
                })
                .collect();

            // The run stops here, so no pending task will ever start.
            if let Err(error) = run.record(finished) {
                return run.block_pending(on_event).and(Err(error));
            }
        }
        Ok(run.exit_status)
    }
}

/// The state of one workflow run.
struct WorkflowRun<'workflow> {
    workflow: &'workflow Workflow,
    execution: WorkflowExecution<'workflow>,
    /// Captured output of finished tasks that a later task reads.
    outputs: HashMap<TaskIndex, ProcessOutput>,
    /// The status of the first failing task, in declaration order.
    exit_status: i32,
}

impl<'workflow> WorkflowRun<'workflow> {
    fn new(workflow: &'workflow Workflow) -> Self {
        Self {
            workflow,
            execution: workflow.execution(),
            outputs: HashMap::new(),
            exit_status: 0,
        }
    }

    /// Marks ready tasks as running and returns them.
    fn start_ready(
        &mut self,
        on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
    ) -> io::Result<Vec<StartedTask<'workflow>>> {
        self.execution
            .ready()
            .into_iter()
            .map(|index| {
                let task = self.execution.start(index).map_err(io::Error::other)?;
                on_event(&RunEvent::Started { task: Some(index) })?;
                Ok(StartedTask { index, task })
            })
            .collect()
    }

    /// The requests of started tasks, with their output placeholders filled.
    fn jobs(&self, tasks: &[StartedTask<'workflow>]) -> io::Result<Vec<Job<'workflow>>> {
        tasks
            .iter()
            .map(|task| {
                let request = render_request(task.task, &self.outputs)?;
                Ok(Job {
                    task: Some(task.index),
                    request: execution_request(request),
                    sandbox: task.task.sandbox(),
                })
            })
            .collect()
    }

    /// Records task outcomes and picks the workflow exit status.
    ///
    /// Outcomes are sorted into declaration order, which keeps the reported
    /// status and error the same from run to run.
    fn record(&mut self, mut finished: Vec<FinishedTask>) -> io::Result<()> {
        finished.sort_by_key(|task| task.index);
        let mut execution_error = None;

        for FinishedTask { index, outcome } in finished {
            self.execution
                .complete(index, completion(&outcome))
                .map_err(io::Error::other)?;
            match outcome {
                Ok(output) => {
                    if !output.succeeded() && self.exit_status == 0 {
                        self.exit_status = exit_status(&output);
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

        execution_error.map_or(Ok(()), Err)
    }

    /// Reports every task that has not started as blocked.
    fn block_pending(
        &self,
        on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
    ) -> io::Result<()> {
        self.execution
            .pending()
            .into_iter()
            .try_for_each(|index| on_event(&RunEvent::Blocked { task: index }))
    }
}

/// Fills the output placeholders of a task's request from the finished tasks.
fn render_request(
    task: &Task,
    outputs: &HashMap<TaskIndex, ProcessOutput>,
) -> io::Result<TaskRequest> {
    task.render_request(|source, stream| {
        outputs
            .get(&source)
            .map(|output| output_text(output, stream))
    })
    .map_err(|reference| io::Error::other(format!("{reference} has no output yet")))
}

/// One captured stream as text, without the newlines that end it.
fn output_text(output: &ProcessOutput, stream: TaskOutput) -> String {
    let bytes = match stream {
        TaskOutput::Stdout => output.stdout(),
        TaskOutput::Stderr => output.stderr(),
    };
    String::from_utf8_lossy(bytes)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

/// A started task and its position.
struct StartedTask<'workflow> {
    index: TaskIndex,
    task: &'workflow Task,
}

/// A workflow task's outcome.
struct FinishedTask {
    index: TaskIndex,
    outcome: Outcome,
}

/// One request to run.
struct Job<'policy> {
    /// Workflow task, if this request belongs to a workflow.
    task: Option<TaskIndex>,
    request: ExecutionRequest,
    sandbox: Option<&'policy SandboxPolicy>,
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
type Outcome = io::Result<ProcessOutput>;

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
    on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
) -> io::Result<Vec<Outcome>> {
    let (sender, receiver) = mpsc::channel::<Message>();
    let mut outcomes = jobs.iter().map(|_| None).collect::<Vec<Option<Outcome>>>();

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
                    },
                )| {
                    let sender = sender.clone();
                    scope.spawn(move || {
                        let started = Instant::now();
                        let sink = |stream, line: &[u8]| {
                            let line = line.to_vec();
                            let _ = sender.send(Message::Line { task, stream, line });
                        };
                        let outcome = runner.run_streaming(request, sandbox, &sink);
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
        for message in receiver {
            match message {
                Message::Line { task, stream, line } => {
                    on_event(&RunEvent::Output { task, stream, line })?;
                }
                Message::Done {
                    job,
                    task,
                    outcome,
                    elapsed,
                } => {
                    let outcome = report_done(task, outcome, elapsed, on_event)?;
                    if let Some(slot) = outcomes.get_mut(job) {
                        *slot = Some(outcome);
                    }
                }
            }
        }

        for handle in handles {
            handle
                .join()
                .map_err(|_| io::Error::other("workflow task execution thread panicked"))?;
        }
        io::Result::Ok(())
    })?;

    outcomes
        .into_iter()
        .map(|outcome| outcome.ok_or_else(|| io::Error::other("request produced no result")))
        .collect()
}

/// Reports a request's end and hands its outcome back.
fn report_done(
    task: Option<TaskIndex>,
    outcome: Outcome,
    elapsed: Duration,
    on_event: &mut impl FnMut(&RunEvent) -> io::Result<()>,
) -> io::Result<Outcome> {
    match outcome {
        Ok(output) => {
            let event = RunEvent::Finished {
                task,
                output,
                elapsed,
            };
            on_event(&event)?;
            // Take the output back from the event instead of copying its streams.
            let RunEvent::Finished { output, .. } = event else {
                return Err(io::Error::other("finished event changed kind"));
            };
            Ok(Ok(output))
        }
        Err(error) => {
            on_event(&RunEvent::Failed {
                task,
                error_kind: error.kind(),
                elapsed,
            })?;
            Ok(Err(error))
        }
    }
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
