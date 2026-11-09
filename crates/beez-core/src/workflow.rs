use std::collections::HashMap;

use crate::task::{ResolvedReference, Task, TaskDefinition, TaskId, TaskIndex, TaskRequest};
use crate::template::{Template, TemplateError};

/// Reports invalid workflow dependencies.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WorkflowError {
    /// The workflow declares no tasks.
    #[error("workflow must define at least one task")]
    Empty,
    /// An ID occurs more than once.
    #[error("duplicate task ID {0}")]
    DuplicateTask(TaskId),
    /// A required predecessor is absent.
    #[error("task {task} depends on unknown task {dependency}")]
    UnknownDependency {
        /// The task that declares the dependency.
        task: TaskId,
        /// The missing predecessor.
        dependency: TaskId,
    },
    /// A task depends directly on itself.
    #[error("task {0} depends on itself")]
    SelfDependency(TaskId),
    /// A dependency cycle, with the first ID repeated last.
    #[error("workflow dependency cycle: {}", cycle(.0))]
    Cycle(Vec<TaskId>),
    /// A placeholder in a prompt or an argument names no task output.
    ///
    /// The placeholder detail is part of this one-line validation message,
    /// not a separate cause.
    #[error("task {task}: {error}")]
    InvalidTemplate {
        /// The task whose text holds the placeholder.
        task: TaskId,
        /// What is wrong with the placeholder.
        error: TemplateError,
    },
    /// A task reads its own output.
    #[error("task {0} reads its own output")]
    SelfOutput(TaskId),
    /// A task reads the output of a task that does not exist.
    #[error("task {task} reads the output of unknown task {producer}")]
    OutputOfUnknownTask {
        /// The task that reads the output.
        task: TaskId,
        /// The task the placeholder names.
        producer: TaskId,
    },
    /// A task reads the output of a task it does not depend on.
    #[error("task {task} reads the output of task {producer}, so it must depend on it")]
    OutputWithoutDependency {
        /// The task that reads the output.
        task: TaskId,
        /// The task the placeholder names.
        producer: TaskId,
    },
}

/// The tasks of a cycle, joined by arrows.
fn cycle(tasks: &[TaskId]) -> String {
    tasks
        .iter()
        .map(TaskId::as_str)
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// How a running task ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    /// The task succeeded, so its dependents may start.
    Succeeded,
    /// The task failed, so its dependents stay blocked.
    Failed,
}

/// Reports a task that is not in the state an execution step needs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExecutionError {
    /// The task is not running.
    #[error("workflow task is not running")]
    NotRunning(TaskIndex),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl From<Completion> for TaskStatus {
    fn from(completion: Completion) -> Self {
        match completion {
            Completion::Succeeded => Self::Succeeded,
            Completion::Failed => Self::Failed,
        }
    }
}

/// A validated, acyclic collection of workflow tasks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workflow {
    tasks: Vec<Task>,
}

impl Workflow {
    /// Returns tasks in declaration order.
    #[must_use]
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// Returns every task with its index, in declaration order.
    pub fn indexed(&self) -> impl Iterator<Item = (TaskIndex, &Task)> {
        self.tasks
            .iter()
            .enumerate()
            .map(|(position, task)| (TaskIndex(position), task))
    }

    /// The IDs of the tasks `task` depends on, in the order it names them.
    pub fn dependency_ids<'workflow>(
        &'workflow self,
        task: &'workflow Task,
    ) -> impl Iterator<Item = &'workflow TaskId> {
        task.dependencies()
            .iter()
            .filter_map(|index| self.task(*index))
            .map(Task::id)
    }

    /// Starts tracking one execution of this workflow.
    #[must_use]
    pub fn execution(&self) -> WorkflowExecution<'_> {
        WorkflowExecution {
            workflow: self,
            statuses: vec![TaskStatus::Pending; self.tasks.len()],
        }
    }

    /// Returns the task at `index`.
    #[must_use]
    pub fn task(&self, index: TaskIndex) -> Option<&Task> {
        self.tasks.get(index.position())
    }

    /// True when another task reads the output of the task at `index`.
    #[must_use]
    pub fn is_read(&self, index: TaskIndex) -> bool {
        self.task(index).is_some_and(|task| task.read_by_dependents)
    }

    /// True when every task runs inside a sandbox.
    #[must_use]
    pub fn all_tasks_sandboxed(&self) -> bool {
        self.tasks.iter().all(|task| task.sandbox.is_some())
    }
}

impl TryFrom<Vec<TaskDefinition>> for Workflow {
    type Error = WorkflowError;

    fn try_from(definitions: Vec<TaskDefinition>) -> Result<Self, Self::Error> {
        if definitions.is_empty() {
            return Err(WorkflowError::Empty);
        }

        let graph = Graph::resolve(&definitions)?;
        if let Some(cycle) = graph.find_cycle() {
            return Err(WorkflowError::Cycle(
                cycle
                    .iter()
                    .filter_map(|index| definitions.get(index.position()))
                    .map(|definition| definition.id.clone())
                    .collect(),
            ));
        }

        let Graph {
            indexes,
            dependencies,
        } = graph;
        let mut tasks = definitions
            .into_iter()
            .zip(dependencies)
            .map(|(definition, dependencies)| build_task(&indexes, definition, dependencies))
            .collect::<Result<Vec<_>, _>>()?;

        let sources = tasks
            .iter()
            .flat_map(Task::reads)
            .copied()
            .collect::<Vec<_>>();
        for source in sources {
            if let Some(task) = tasks.get_mut(source.position()) {
                task.read_by_dependents = true;
            }
        }

        Ok(Self { tasks })
    }
}

/// Tracks task readiness and completion for one workflow execution.
#[derive(Debug)]
pub struct WorkflowExecution<'workflow> {
    workflow: &'workflow Workflow,
    statuses: Vec<TaskStatus>,
}

impl<'workflow> WorkflowExecution<'workflow> {
    /// Marks every pending task whose dependencies succeeded as running, and
    /// returns them in declaration order.
    pub fn start_ready(&mut self) -> Vec<(TaskIndex, &'workflow Task)> {
        let ready = self
            .workflow
            .indexed()
            .filter_map(|(index, _)| Some((index, self.ready_task(index)?)))
            .collect::<Vec<_>>();
        for (index, _) in &ready {
            if let Some(status) = self.statuses.get_mut(index.position()) {
                *status = TaskStatus::Running;
            }
        }
        ready
    }

    /// Records a running task's outcome.
    pub fn complete(
        &mut self,
        index: TaskIndex,
        completion: Completion,
    ) -> Result<(), ExecutionError> {
        match self.statuses.get_mut(index.position()) {
            Some(status) if *status == TaskStatus::Running => {
                *status = completion.into();
                Ok(())
            }
            _ => Err(ExecutionError::NotRunning(index)),
        }
    }

    /// True while tasks remain pending.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.statuses.contains(&TaskStatus::Pending)
    }

    /// Returns every task that has not started yet.
    #[must_use]
    pub fn pending(&self) -> Vec<TaskIndex> {
        self.statuses
            .iter()
            .enumerate()
            .filter(|(_, status)| **status == TaskStatus::Pending)
            .map(|(index, _)| TaskIndex(index))
            .collect()
    }

    /// The task at `index`, when it has not started and every dependency
    /// succeeded.
    fn ready_task(&self, index: TaskIndex) -> Option<&'workflow Task> {
        let task = self.workflow.task(index)?;
        let ready = self.statuses.get(index.position()) == Some(&TaskStatus::Pending)
            && task.dependencies().iter().all(|dependency| {
                self.statuses.get(dependency.position()) == Some(&TaskStatus::Succeeded)
            });
        ready.then_some(task)
    }
}

/// The task positions and predecessors of a workflow under validation.
struct Graph {
    indexes: HashMap<TaskId, TaskIndex>,
    /// The predecessors of every task, in declaration order.
    dependencies: Vec<Vec<TaskIndex>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Visit {
    Unseen,
    Visiting,
    Complete,
}

impl Graph {
    /// Indexes every task, then resolves every dependency ID.
    fn resolve(definitions: &[TaskDefinition]) -> Result<Self, WorkflowError> {
        let mut indexes = HashMap::with_capacity(definitions.len());
        for (position, definition) in definitions.iter().enumerate() {
            if indexes
                .insert(definition.id.clone(), TaskIndex(position))
                .is_some()
            {
                return Err(WorkflowError::DuplicateTask(definition.id.clone()));
            }
        }

        let dependencies = definitions
            .iter()
            .enumerate()
            .map(|(position, definition)| {
                resolve_dependencies(&indexes, definition, TaskIndex(position))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            indexes,
            dependencies,
        })
    }

    /// The first cycle as the positions on it, first repeated last.
    fn find_cycle(&self) -> Option<Vec<TaskIndex>> {
        let mut states = vec![Visit::Unseen; self.dependencies.len()];
        let mut trail = Vec::new();

        (0..self.dependencies.len()).find_map(|position| {
            self.visit(TaskIndex(position), &mut states, &mut trail)
                .err()
        })
    }

    fn visit(
        &self,
        index: TaskIndex,
        states: &mut [Visit],
        trail: &mut Vec<TaskIndex>,
    ) -> Result<(), Vec<TaskIndex>> {
        match states
            .get(index.position())
            .copied()
            .unwrap_or(Visit::Complete)
        {
            Visit::Complete => return Ok(()),
            Visit::Visiting => {
                let start = trail
                    .iter()
                    .position(|task| *task == index)
                    .unwrap_or_default();
                return Err(trail
                    .iter()
                    .skip(start)
                    .copied()
                    .chain(std::iter::once(index))
                    .collect());
            }
            Visit::Unseen => {}
        }

        if let Some(state) = states.get_mut(index.position()) {
            *state = Visit::Visiting;
        }
        trail.push(index);

        for dependency in self
            .dependencies
            .get(index.position())
            .into_iter()
            .flatten()
        {
            self.visit(*dependency, states, trail)?;
        }

        let _ = trail.pop();
        if let Some(state) = states.get_mut(index.position()) {
            *state = Visit::Complete;
        }
        Ok(())
    }
}

fn resolve_dependencies(
    indexes: &HashMap<TaskId, TaskIndex>,
    definition: &TaskDefinition,
    task_index: TaskIndex,
) -> Result<Vec<TaskIndex>, WorkflowError> {
    definition
        .depends_on
        .iter()
        .map(|dependency_id| {
            let Some(&dependency) = indexes.get(dependency_id) else {
                return Err(WorkflowError::UnknownDependency {
                    task: definition.id.clone(),
                    dependency: dependency_id.clone(),
                });
            };
            if dependency == task_index {
                return Err(WorkflowError::SelfDependency(definition.id.clone()));
            }
            Ok(dependency)
        })
        .collect()
}

/// Builds one task of an acyclic graph, once its templates resolve.
fn build_task(
    indexes: &HashMap<TaskId, TaskIndex>,
    definition: TaskDefinition,
    dependencies: Vec<TaskIndex>,
) -> Result<Task, WorkflowError> {
    let template = resolve_templates(indexes, &definition, &dependencies)?;
    Ok(Task {
        id: definition.id,
        dependencies,
        reads: distinct_sources(&template),
        read_by_dependents: false,
        request: definition.request,
        template,
        sandbox: definition.sandbox,
    })
}

/// Parses the texts of the request and resolves the task every
/// placeholder reads.
///
/// A transitive dependency has also finished when the task starts, but
/// requiring a direct one keeps the data flow visible in `depends_on`.
fn resolve_templates(
    indexes: &HashMap<TaskId, TaskIndex>,
    definition: &TaskDefinition,
    dependencies: &[TaskIndex],
) -> Result<TaskRequest<Template<ResolvedReference>>, WorkflowError> {
    let task = || definition.id.clone();
    definition.request.try_map_texts(|text| {
        Template::parse(text)
            .map_err(|error| WorkflowError::InvalidTemplate {
                task: task(),
                error,
            })?
            .try_map_references(|reference| {
                let source = reference.task();
                if *source == definition.id {
                    return Err(WorkflowError::SelfOutput(task()));
                }
                let Some(&index) = indexes.get(source) else {
                    return Err(WorkflowError::OutputOfUnknownTask {
                        task: task(),
                        producer: source.clone(),
                    });
                };
                if !dependencies.contains(&index) {
                    return Err(WorkflowError::OutputWithoutDependency {
                        task: task(),
                        producer: source.clone(),
                    });
                }
                Ok(ResolvedReference {
                    source: index,
                    reference,
                })
            })
    })
}

/// The distinct tasks the request reads, in the order it first reads them.
fn distinct_sources(request: &TaskRequest<Template<ResolvedReference>>) -> Vec<TaskIndex> {
    let mut sources = Vec::new();
    for resolved in request.texts().flat_map(Template::references) {
        if !sources.contains(&resolved.source) {
            sources.push(resolved.source);
        }
    }
    sources
}

#[cfg(test)]
mod tests {
    use beez_policy::{HarnessOptions, HeadlessHarness, SandboxPolicy};

    use crate::task::{TaskDefinition, TaskIndex, TaskRequest};

    use super::{Completion, ExecutionError, Workflow, WorkflowError, WorkflowExecution};

    fn definition(id: &str, depends_on: &[&str], request: TaskRequest) -> TaskDefinition {
        TaskDefinition::new(
            id.parse().unwrap(),
            depends_on
                .iter()
                .map(|dependency| dependency.parse().unwrap())
                .collect(),
            request,
        )
    }

    fn task(id: &str, depends_on: &[&str]) -> TaskDefinition {
        definition(
            id,
            depends_on,
            TaskRequest::command("true".into(), Vec::new()),
        )
    }

    fn prompt_task(id: &str, depends_on: &[&str], prompt: &str) -> TaskDefinition {
        definition(
            id,
            depends_on,
            TaskRequest::harness(
                HeadlessHarness::Claude,
                prompt.to_owned(),
                HarnessOptions::default(),
            ),
        )
    }

    /// Starts every ready task and returns their indexes.
    fn start(execution: &mut WorkflowExecution<'_>) -> Vec<TaskIndex> {
        execution
            .start_ready()
            .into_iter()
            .map(|(index, _)| index)
            .collect()
    }

    fn complete(
        execution: &mut WorkflowExecution<'_>,
        tasks: &[TaskIndex],
        completion: Completion,
    ) {
        for index in tasks {
            assert_eq!(execution.complete(*index, completion), Ok(()));
        }
    }

    #[test]
    fn rejects_a_workflow_without_tasks() {
        let error = Workflow::try_from(Vec::new()).unwrap_err();

        assert_eq!(error, WorkflowError::Empty);
        assert_eq!(error.to_string(), "workflow must define at least one task");
    }

    #[test]
    fn does_not_start_a_task_before_its_dependencies_succeed() {
        let workflow =
            Workflow::try_from(vec![task("prepare", &[]), task("test", &["prepare"])]).unwrap();
        let mut sut = workflow.execution();

        assert_eq!(start(&mut sut), [TaskIndex(0)]);
        assert_eq!(sut.pending(), [TaskIndex(1)]);
    }

    #[test]
    fn does_not_start_a_task_twice() {
        let workflow = Workflow::try_from(vec![task("prepare", &[])]).unwrap();
        let mut sut = workflow.execution();
        assert_eq!(start(&mut sut), [TaskIndex(0)]);

        assert_eq!(start(&mut sut), Vec::<TaskIndex>::new());
    }

    #[test]
    fn does_not_complete_a_task_that_has_not_started() {
        let workflow = Workflow::try_from(vec![task("prepare", &[])]).unwrap();
        let mut sut = workflow.execution();

        assert_eq!(
            sut.complete(TaskIndex(0), Completion::Succeeded),
            Err(ExecutionError::NotRunning(TaskIndex(0)))
        );
        assert_eq!(start(&mut sut), [TaskIndex(0)]);
    }

    #[test]
    fn does_not_change_a_completed_task_outcome() {
        let workflow =
            Workflow::try_from(vec![task("prepare", &[]), task("test", &["prepare"])]).unwrap();
        let mut sut = workflow.execution();
        assert_eq!(start(&mut sut), [TaskIndex(0)]);
        complete(&mut sut, &[TaskIndex(0)], Completion::Succeeded);

        assert_eq!(
            sut.complete(TaskIndex(0), Completion::Failed),
            Err(ExecutionError::NotRunning(TaskIndex(0)))
        );
        assert_eq!(start(&mut sut), [TaskIndex(1)]);
    }

    /// A caller tells a finished workflow from a blocked one by both signals.
    #[test]
    fn reports_no_pending_task_after_every_task_succeeds() {
        let workflow = Workflow::try_from(vec![
            task("prepare", &[]),
            task("build", &[]),
            task("test", &["prepare", "build"]),
        ])
        .unwrap();
        let mut sut = workflow.execution();
        let first = start(&mut sut);
        complete(&mut sut, &first, Completion::Succeeded);
        let second = start(&mut sut);
        complete(&mut sut, &second, Completion::Succeeded);

        assert!(!sut.has_pending());
        assert_eq!(start(&mut sut), Vec::<TaskIndex>::new());
    }

    #[test]
    fn reports_no_pending_task_after_the_only_task_fails() {
        let workflow = Workflow::try_from(vec![task("prepare", &[])]).unwrap();
        let mut sut = workflow.execution();
        assert_eq!(start(&mut sut), [TaskIndex(0)]);

        assert_eq!(sut.complete(TaskIndex(0), Completion::Failed), Ok(()));

        assert!(!sut.has_pending());
    }

    #[test]
    fn starts_every_ready_task_at_once() {
        let workflow = Workflow::try_from(vec![
            task("prepare", &[]),
            task("build", &[]),
            task("test", &["prepare", "build"]),
        ])
        .unwrap();
        let mut sut = workflow.execution();

        let started = sut
            .start_ready()
            .into_iter()
            .map(|(index, task)| (index, task.id().as_str()))
            .collect::<Vec<_>>();

        assert_eq!(
            started,
            [(TaskIndex(0), "prepare"), (TaskIndex(1), "build")]
        );
        assert!(sut.start_ready().is_empty());
        assert_eq!(sut.pending(), [TaskIndex(2)]);
    }

    #[test]
    fn releases_a_task_after_every_dependency_succeeds() {
        let workflow = Workflow::try_from(vec![
            task("prepare", &[]),
            task("build", &[]),
            task("test", &["prepare", "build"]),
        ])
        .unwrap();
        let mut sut = workflow.execution();
        assert_eq!(start(&mut sut), [TaskIndex(0), TaskIndex(1)]);
        complete(
            &mut sut,
            &[TaskIndex(0), TaskIndex(1)],
            Completion::Succeeded,
        );

        assert_eq!(start(&mut sut), [TaskIndex(2)]);
    }

    #[test]
    fn does_not_release_a_task_while_one_dependency_is_running() {
        let workflow = Workflow::try_from(vec![
            task("prepare", &[]),
            task("build", &[]),
            task("test", &["prepare", "build"]),
        ])
        .unwrap();
        let mut sut = workflow.execution();
        assert_eq!(start(&mut sut), [TaskIndex(0), TaskIndex(1)]);
        complete(&mut sut, &[TaskIndex(0)], Completion::Succeeded);

        assert_eq!(start(&mut sut), Vec::<TaskIndex>::new());
        assert_eq!(sut.pending(), [TaskIndex(2)]);
    }

    #[test]
    fn blocks_tasks_when_any_dependency_fails() {
        let workflow = Workflow::try_from(vec![
            task("prepare", &[]),
            task("build", &[]),
            task("test", &["prepare", "build"]),
        ])
        .unwrap();
        let mut sut = workflow.execution();
        assert_eq!(start(&mut sut), [TaskIndex(0), TaskIndex(1)]);
        complete(&mut sut, &[TaskIndex(0)], Completion::Succeeded);

        assert_eq!(sut.complete(TaskIndex(1), Completion::Failed), Ok(()));

        assert_eq!(start(&mut sut), Vec::<TaskIndex>::new());
        assert!(sut.has_pending());
    }

    #[test]
    fn reports_only_the_tasks_that_have_not_started() {
        let workflow = Workflow::try_from(vec![
            task("prepare", &[]),
            task("build", &["prepare"]),
            task("test", &["build"]),
        ])
        .unwrap();
        let mut sut = workflow.execution();
        assert_eq!(start(&mut sut), [TaskIndex(0)]);
        complete(&mut sut, &[TaskIndex(0)], Completion::Succeeded);
        assert_eq!(start(&mut sut), [TaskIndex(1)]);

        assert_eq!(sut.pending(), [TaskIndex(2)]);
    }

    #[test]
    fn does_not_release_dependents_after_a_predecessor_fails() {
        let workflow =
            Workflow::try_from(vec![task("prepare", &[]), task("test", &["prepare"])]).unwrap();
        let mut sut = workflow.execution();
        assert_eq!(start(&mut sut), [TaskIndex(0)]);

        assert_eq!(sut.complete(TaskIndex(0), Completion::Failed), Ok(()));

        assert_eq!(start(&mut sut), Vec::<TaskIndex>::new());
        assert!(sut.has_pending());
    }

    #[test]
    fn resolves_dependencies_to_task_indexes() {
        let workflow = Workflow::try_from(vec![
            task("build", &["prepare", "lint"]),
            task("prepare", &[]),
            task("lint", &[]),
        ])
        .unwrap();

        let build = workflow.tasks().first().unwrap();
        let prepare = workflow.tasks().get(1).unwrap();
        let lint = workflow.tasks().last().unwrap();
        assert_eq!(build.id().as_str(), "build");
        assert_eq!(
            build
                .dependencies()
                .iter()
                .map(|dependency| dependency.position())
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(prepare.id().as_str(), "prepare");
        assert_eq!(prepare.dependencies(), Vec::<TaskIndex>::new());
        assert_eq!(lint.id().as_str(), "lint");
        assert_eq!(lint.dependencies(), Vec::<TaskIndex>::new());
    }

    #[test]
    fn rejects_duplicate_task_ids() {
        let error = Workflow::try_from(vec![task("build", &[]), task("build", &[])]).unwrap_err();

        assert_eq!(
            error,
            WorkflowError::DuplicateTask("build".parse().unwrap())
        );
    }

    #[test]
    fn rejects_self_dependencies() {
        let error = Workflow::try_from(vec![task("build", &["build"])]).unwrap_err();

        assert_eq!(
            error,
            WorkflowError::SelfDependency("build".parse().unwrap())
        );
    }

    #[test]
    fn accepts_output_references_to_direct_dependencies() {
        let workflow = Workflow::try_from(vec![
            task("inspect", &[]),
            prompt_task(
                "review",
                &["inspect"],
                "review {{ tasks.inspect.stdout }} and {{ tasks.inspect.stderr }}",
            ),
            definition(
                "test",
                &["review"],
                TaskRequest::command("echo".into(), vec!["{{ tasks.review.stdout }}".into()]),
            ),
        ])
        .unwrap();

        let reads = workflow
            .tasks()
            .iter()
            .map(|task| task.reads().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(reads, [vec![], vec![TaskIndex(0)], vec![TaskIndex(1)]]);
    }

    #[test]
    fn renders_output_references_by_task_index() {
        let workflow = Workflow::try_from(vec![
            task("inspect", &[]),
            definition(
                "test",
                &["inspect"],
                TaskRequest::command(
                    "echo".into(),
                    vec!["{{ tasks.inspect.stderr }}!".into(), "{{ x }}".into()],
                ),
            ),
        ])
        .unwrap();
        let sut = workflow.tasks().last().unwrap();

        let rendered = sut.render_request(|source, stream| {
            (source == TaskIndex(0)).then(|| format!("<{stream}>"))
        });
        let missing = sut.render_request(|_, _| None).unwrap_err();

        assert_eq!(
            rendered,
            Ok(TaskRequest::command(
                "echo".into(),
                vec!["<stderr>!".into(), "{{ x }}".into()]
            ))
        );
        assert_eq!(missing.to_string(), "{{ tasks.inspect.stderr }}");
    }

    #[test]
    fn reports_whether_another_task_reads_an_output() {
        let workflow = Workflow::try_from(vec![
            task("inspect", &[]),
            prompt_task("review", &["inspect"], "{{ tasks.inspect.stdout }}"),
        ])
        .unwrap();

        assert!(workflow.is_read(TaskIndex(0)));
        assert!(!workflow.is_read(TaskIndex(1)));
    }

    #[test]
    fn reports_whether_every_task_runs_in_a_sandbox() {
        let sandboxed = || task("build", &[]).sandboxed(Some(SandboxPolicy::default()));
        let all = Workflow::try_from(vec![sandboxed()]).unwrap();
        let some = Workflow::try_from(vec![sandboxed(), task("test", &[])]).unwrap();

        assert!(all.all_tasks_sandboxed());
        assert!(!some.all_tasks_sandboxed());
    }

    #[test]
    fn rejects_a_placeholder_that_names_no_task_output() {
        let error = Workflow::try_from(vec![
            task("inspect", &[]),
            prompt_task("review", &["inspect"], "{{ tasks.inspect.status }}"),
        ])
        .unwrap_err();

        assert!(matches!(error, WorkflowError::InvalidTemplate { .. }));
        assert_eq!(
            error.to_string(),
            "task review: {{ tasks.inspect.status }} is not a task output; write {{ tasks.<id>.stdout }} or {{ tasks.<id>.stderr }}"
        );
    }

    #[test]
    fn rejects_a_task_that_reads_its_own_output() {
        let error = Workflow::try_from(vec![prompt_task(
            "review",
            &[],
            "{{ tasks.review.stdout }}",
        )])
        .unwrap_err();

        assert_eq!(error, WorkflowError::SelfOutput("review".parse().unwrap()));
        assert_eq!(error.to_string(), "task review reads its own output");
    }

    #[test]
    fn rejects_reading_the_output_of_an_unknown_task() {
        let error = Workflow::try_from(vec![prompt_task(
            "review",
            &[],
            "{{ tasks.inspect.stdout }}",
        )])
        .unwrap_err();

        assert_eq!(
            error,
            WorkflowError::OutputOfUnknownTask {
                task: "review".parse().unwrap(),
                producer: "inspect".parse().unwrap(),
            }
        );
        assert_eq!(
            error.to_string(),
            "task review reads the output of unknown task inspect"
        );
    }

    #[test]
    fn rejects_reading_the_output_of_a_task_that_is_not_a_dependency() {
        let error = Workflow::try_from(vec![
            task("inspect", &[]),
            task("build", &["inspect"]),
            prompt_task("review", &["build"], "{{ tasks.inspect.stdout }}"),
        ])
        .unwrap_err();

        assert_eq!(
            error,
            WorkflowError::OutputWithoutDependency {
                task: "review".parse().unwrap(),
                producer: "inspect".parse().unwrap(),
            }
        );
        assert_eq!(
            error.to_string(),
            "task review reads the output of task inspect, so it must depend on it"
        );
    }

    #[test]
    fn checks_the_dependency_of_a_reference_after_stray_braces() {
        for prompt in [
            "x {{ y {{ tasks.inspect.stdout }}",
            "{{{ tasks.inspect.stdout }}",
        ] {
            let error = Workflow::try_from(vec![
                task("inspect", &[]),
                prompt_task("review", &[], prompt),
            ])
            .unwrap_err();

            assert_eq!(
                error,
                WorkflowError::OutputWithoutDependency {
                    task: "review".parse().unwrap(),
                    producer: "inspect".parse().unwrap(),
                },
                "{prompt}"
            );
        }
    }

    #[test]
    fn rejects_unknown_dependencies() {
        let error = Workflow::try_from(vec![task("build", &["prepare"])]).unwrap_err();

        assert_eq!(
            error,
            WorkflowError::UnknownDependency {
                task: "build".parse().unwrap(),
                dependency: "prepare".parse().unwrap(),
            }
        );
    }

    /// Dependencies resolve before the cycle check, and the cycle check runs
    /// before any template is parsed.
    #[test]
    fn reports_a_dependency_error_before_a_cycle_or_a_template_error() {
        let unknown = Workflow::try_from(vec![
            prompt_task("review", &["inspect"], "{{ tasks.review.status }}"),
            task("build", &["test"]),
            task("test", &["build"]),
        ])
        .unwrap_err();
        let cycle = Workflow::try_from(vec![
            prompt_task("review", &[], "{{ tasks.review.status }}"),
            task("build", &["test"]),
            task("test", &["build"]),
        ])
        .unwrap_err();

        assert_eq!(
            unknown,
            WorkflowError::UnknownDependency {
                task: "review".parse().unwrap(),
                dependency: "inspect".parse().unwrap(),
            }
        );
        assert!(matches!(cycle, WorkflowError::Cycle(_)), "{cycle:?}");
    }

    #[test]
    fn rejects_dependency_cycles() {
        let error = Workflow::try_from(vec![
            task("prepare", &["build"]),
            task("build", &["prepare"]),
        ])
        .unwrap_err();

        assert_eq!(
            error,
            WorkflowError::Cycle(vec![
                "prepare".parse().unwrap(),
                "build".parse().unwrap(),
                "prepare".parse().unwrap(),
            ])
        );
    }

    #[test]
    fn rejects_three_task_dependency_cycles_with_closed_path() {
        let error = Workflow::try_from(vec![
            task("prepare", &["build"]),
            task("build", &["test"]),
            task("test", &["prepare"]),
        ])
        .unwrap_err();

        assert_eq!(
            error,
            WorkflowError::Cycle(vec![
                "prepare".parse().unwrap(),
                "build".parse().unwrap(),
                "test".parse().unwrap(),
                "prepare".parse().unwrap(),
            ])
        );
    }
}
