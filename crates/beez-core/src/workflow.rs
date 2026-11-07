use std::collections::HashMap;
use std::fmt;

use crate::task::{ResolvedReference, Task, TaskDefinition, TaskId, TaskIndex};
use crate::template::{Template, TemplateError};

/// Reports invalid workflow dependencies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowError {
    /// The workflow declares no tasks.
    Empty,
    /// An ID occurs more than once.
    DuplicateTask(TaskId),
    /// A required predecessor is absent.
    UnknownDependency {
        /// The task that declares the dependency.
        task: TaskId,
        /// The missing predecessor.
        dependency: TaskId,
    },
    /// A task depends directly on itself.
    SelfDependency(TaskId),
    /// A dependency cycle, with the first ID repeated last.
    Cycle(Vec<TaskId>),
    /// A placeholder in a prompt or an argument names no task output.
    InvalidTemplate {
        /// The task whose text holds the placeholder.
        task: TaskId,
        /// What is wrong with the placeholder.
        error: TemplateError,
    },
    /// A task reads its own output.
    SelfOutput(TaskId),
    /// A task reads the output of a task that does not exist.
    OutputOfUnknownTask {
        /// The task that reads the output.
        task: TaskId,
        /// The task the placeholder names.
        source: TaskId,
    },
    /// A task reads the output of a task it does not depend on.
    OutputWithoutDependency {
        /// The task that reads the output.
        task: TaskId,
        /// The task the placeholder names.
        source: TaskId,
    },
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("workflow must define at least one task"),
            Self::DuplicateTask(task) => write!(formatter, "duplicate task ID {task}"),
            Self::UnknownDependency { task, dependency } => {
                write!(
                    formatter,
                    "task {task} depends on unknown task {dependency}"
                )
            }
            Self::SelfDependency(task) => write!(formatter, "task {task} depends on itself"),
            Self::Cycle(tasks) => {
                let cycle = tasks
                    .iter()
                    .map(TaskId::as_str)
                    .collect::<Vec<_>>()
                    .join(" -> ");
                write!(formatter, "workflow dependency cycle: {cycle}")
            }
            Self::InvalidTemplate { task, error } => write!(formatter, "task {task}: {error}"),
            Self::SelfOutput(task) => write!(formatter, "task {task} reads its own output"),
            Self::OutputOfUnknownTask { task, source } => {
                write!(
                    formatter,
                    "task {task} reads the output of unknown task {source}"
                )
            }
            Self::OutputWithoutDependency { task, source } => {
                write!(
                    formatter,
                    "task {task} reads the output of task {source}, so it must depend on it"
                )
            }
        }
    }
}

impl std::error::Error for WorkflowError {}

/// How a running task ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    /// The task succeeded, so its dependents may start.
    Succeeded,
    /// The task failed, so its dependents stay blocked.
    Failed,
}

/// Reports a task that is not in the state an execution step needs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionError {
    /// The task has started already, or a dependency has not succeeded.
    NotReady(TaskIndex),
    /// The task is not running.
    NotRunning(TaskIndex),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotReady(_) => "workflow task is not ready",
            Self::NotRunning(_) => "workflow task is not running",
        })
    }
}

impl std::error::Error for ExecutionError {}

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
        self.tasks.iter().any(|task| task.reads.contains(&index))
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
            return Err(WorkflowError::Cycle(task_ids(&definitions, &cycle)));
        }
        let templates = definitions
            .iter()
            .zip(&graph.dependencies)
            .map(|(definition, dependencies)| graph.resolve_templates(definition, dependencies))
            .collect::<Result<Vec<_>, _>>()?;
        let dependencies = graph.into_dependencies();

        let tasks = definitions
            .into_iter()
            .zip(dependencies)
            .zip(templates)
            .map(|((definition, dependencies), templates)| Task {
                id: definition.id,
                dependencies,
                reads: distinct_sources(&templates),
                request: definition.request,
                templates,
                sandbox: definition.sandbox,
            })
            .collect();

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
    /// Returns all pending tasks whose dependencies succeeded.
    #[must_use]
    pub fn ready(&self) -> Vec<TaskIndex> {
        (0..self.workflow.tasks.len())
            .map(TaskIndex)
            .filter(|index| self.is_ready(*index))
            .collect()
    }

    /// Marks a ready task as running and returns it.
    pub fn start(&mut self, index: TaskIndex) -> Result<&'workflow Task, ExecutionError> {
        let not_ready = ExecutionError::NotReady(index);
        if !self.is_ready(index) {
            return Err(not_ready);
        }

        *self.statuses.get_mut(index.position()).ok_or(not_ready)? = TaskStatus::Running;
        self.workflow.task(index).ok_or(not_ready)
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

    /// True when the task has not started and every dependency succeeded.
    fn is_ready(&self, index: TaskIndex) -> bool {
        let Some(task) = self.workflow.task(index) else {
            return false;
        };
        self.statuses.get(index.position()) == Some(&TaskStatus::Pending)
            && task.dependencies().iter().all(|dependency| {
                self.statuses.get(dependency.position()) == Some(&TaskStatus::Succeeded)
            })
    }
}

/// The task positions and predecessors of a workflow under validation.
struct Graph<'definitions> {
    indexes: HashMap<&'definitions TaskId, TaskIndex>,
    /// The predecessors of every task, in declaration order.
    dependencies: Vec<Vec<TaskIndex>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Visit {
    Unseen,
    Visiting,
    Complete,
}

impl<'definitions> Graph<'definitions> {
    /// Indexes every task, then resolves every dependency ID.
    fn resolve(definitions: &'definitions [TaskDefinition]) -> Result<Self, WorkflowError> {
        let mut indexes = HashMap::with_capacity(definitions.len());
        for (position, definition) in definitions.iter().enumerate() {
            if indexes
                .insert(&definition.id, TaskIndex(position))
                .is_some()
            {
                return Err(WorkflowError::DuplicateTask(definition.id.clone()));
            }
        }

        let mut graph = Self {
            indexes,
            dependencies: Vec::with_capacity(definitions.len()),
        };
        for (position, definition) in definitions.iter().enumerate() {
            let dependencies = graph.resolve_dependencies(definition, TaskIndex(position))?;
            graph.dependencies.push(dependencies);
        }
        Ok(graph)
    }

    fn resolve_dependencies(
        &self,
        definition: &TaskDefinition,
        task_index: TaskIndex,
    ) -> Result<Vec<TaskIndex>, WorkflowError> {
        definition
            .depends_on
            .iter()
            .map(|dependency_id| {
                let Some(&dependency) = self.indexes.get(dependency_id) else {
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

    /// Parses the texts of the request and resolves the task every
    /// placeholder reads.
    ///
    /// A transitive dependency has also finished when the task starts, but
    /// requiring a direct one keeps the data flow visible in `depends_on`.
    fn resolve_templates(
        &self,
        definition: &TaskDefinition,
        dependencies: &[TaskIndex],
    ) -> Result<Vec<Template<ResolvedReference>>, WorkflowError> {
        let task = || definition.id.clone();
        definition
            .request
            .texts()
            .map(|text| {
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
                        let Some(&index) = self.indexes.get(source) else {
                            return Err(WorkflowError::OutputOfUnknownTask {
                                task: task(),
                                source: source.clone(),
                            });
                        };
                        if !dependencies.contains(&index) {
                            return Err(WorkflowError::OutputWithoutDependency {
                                task: task(),
                                source: source.clone(),
                            });
                        }
                        Ok(ResolvedReference {
                            source: index,
                            reference,
                        })
                    })
            })
            .collect()
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

    /// The predecessors of every task, once the IDs are no longer needed.
    fn into_dependencies(self) -> Vec<Vec<TaskIndex>> {
        self.dependencies
    }
}

/// The distinct tasks the templates read, in the order they first read them.
fn distinct_sources(templates: &[Template<ResolvedReference>]) -> Vec<TaskIndex> {
    let mut sources = Vec::new();
    for resolved in templates.iter().flat_map(Template::references) {
        if !sources.contains(&resolved.source) {
            sources.push(resolved.source);
        }
    }
    sources
}

/// The IDs of the tasks at `positions`.
fn task_ids(definitions: &[TaskDefinition], positions: &[TaskIndex]) -> Vec<TaskId> {
    positions
        .iter()
        .filter_map(|index| definitions.get(index.position()))
        .map(|definition| definition.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use beez_policy::{HarnessOptions, HeadlessHarness};

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

    fn succeeded<'workflow>(
        mut execution: WorkflowExecution<'workflow>,
        tasks: &[TaskIndex],
    ) -> WorkflowExecution<'workflow> {
        for index in tasks {
            assert!(execution.start(*index).is_ok());
            assert_eq!(execution.complete(*index, Completion::Succeeded), Ok(()));
        }

        execution
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

        assert_eq!(
            sut.start(TaskIndex(1)).err(),
            Some(ExecutionError::NotReady(TaskIndex(1)))
        );
        assert_eq!(sut.ready(), [TaskIndex(0)]);
    }

    #[test]
    fn does_not_start_a_task_twice() {
        let workflow = Workflow::try_from(vec![task("prepare", &[])]).unwrap();
        let mut sut = workflow.execution();
        assert!(sut.start(TaskIndex(0)).is_ok());

        assert!(sut.start(TaskIndex(0)).is_err());
        assert!(sut.ready().is_empty());
    }

    #[test]
    fn does_not_complete_a_task_that_has_not_started() {
        let workflow = Workflow::try_from(vec![task("prepare", &[])]).unwrap();
        let mut sut = workflow.execution();

        assert_eq!(
            sut.complete(TaskIndex(0), Completion::Succeeded),
            Err(ExecutionError::NotRunning(TaskIndex(0)))
        );
        assert_eq!(sut.ready(), [TaskIndex(0)]);
    }

    #[test]
    fn does_not_change_a_completed_task_outcome() {
        let workflow =
            Workflow::try_from(vec![task("prepare", &[]), task("test", &["prepare"])]).unwrap();
        let mut sut = workflow.execution();
        assert!(sut.start(TaskIndex(0)).is_ok());
        assert_eq!(sut.complete(TaskIndex(0), Completion::Succeeded), Ok(()));

        assert_eq!(
            sut.complete(TaskIndex(0), Completion::Failed),
            Err(ExecutionError::NotRunning(TaskIndex(0)))
        );
        assert_eq!(sut.ready(), [TaskIndex(1)]);
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
        let sut = succeeded(
            workflow.execution(),
            &[TaskIndex(0), TaskIndex(1), TaskIndex(2)],
        );

        assert!(!sut.has_pending());
        assert!(sut.ready().is_empty());
    }

    #[test]
    fn reports_no_pending_task_after_the_only_task_fails() {
        let workflow = Workflow::try_from(vec![task("prepare", &[])]).unwrap();
        let mut sut = workflow.execution();
        assert!(sut.start(TaskIndex(0)).is_ok());

        assert_eq!(sut.complete(TaskIndex(0), Completion::Failed), Ok(()));

        assert!(!sut.has_pending());
    }

    #[test]
    fn reports_independent_tasks_as_ready_together() {
        let workflow = Workflow::try_from(vec![
            task("prepare", &[]),
            task("build", &[]),
            task("test", &["prepare", "build"]),
        ])
        .unwrap();

        let sut = workflow.execution();

        assert_eq!(sut.ready(), [TaskIndex(0), TaskIndex(1)]);
    }

    #[test]
    fn releases_a_task_after_every_dependency_succeeds() {
        let workflow = Workflow::try_from(vec![
            task("prepare", &[]),
            task("build", &[]),
            task("test", &["prepare", "build"]),
        ])
        .unwrap();
        let sut = succeeded(workflow.execution(), &[TaskIndex(0), TaskIndex(1)]);

        let ready = sut.ready();

        assert_eq!(ready, [TaskIndex(2)]);
    }

    #[test]
    fn does_not_release_a_task_while_one_dependency_is_pending() {
        let workflow = Workflow::try_from(vec![
            task("prepare", &[]),
            task("build", &[]),
            task("test", &["prepare", "build"]),
        ])
        .unwrap();
        let sut = succeeded(workflow.execution(), &[TaskIndex(0)]);

        let ready = sut.ready();

        assert_eq!(ready, [TaskIndex(1)]);
    }

    #[test]
    fn blocks_tasks_when_any_dependency_fails() {
        let workflow = Workflow::try_from(vec![
            task("prepare", &[]),
            task("build", &[]),
            task("test", &["prepare", "build"]),
        ])
        .unwrap();
        let mut sut = succeeded(workflow.execution(), &[TaskIndex(0)]);
        assert!(sut.start(TaskIndex(1)).is_ok());

        assert_eq!(sut.complete(TaskIndex(1), Completion::Failed), Ok(()));

        assert!(sut.ready().is_empty());
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
        let mut sut = succeeded(workflow.execution(), &[TaskIndex(0)]);
        assert!(sut.start(TaskIndex(1)).is_ok());

        assert_eq!(sut.pending(), [TaskIndex(2)]);
    }

    #[test]
    fn does_not_release_dependents_after_a_predecessor_fails() {
        let workflow =
            Workflow::try_from(vec![task("prepare", &[]), task("test", &["prepare"])]).unwrap();
        let mut sut = workflow.execution();
        assert!(sut.start(TaskIndex(0)).is_ok());

        assert_eq!(sut.complete(TaskIndex(0), Completion::Failed), Ok(()));

        assert!(sut.ready().is_empty());
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
        assert!(prepare.dependencies().is_empty());
        assert_eq!(lint.id().as_str(), "lint");
        assert!(lint.dependencies().is_empty());
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
                source: "inspect".parse().unwrap(),
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
                source: "inspect".parse().unwrap(),
            }
        );
        assert_eq!(
            error.to_string(),
            "task review reads the output of task inspect, so it must depend on it"
        );
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
