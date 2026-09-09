use std::collections::HashMap;
use std::fmt;

use crate::task::{Task, TaskDefinition, TaskId, TaskIndex};

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
        }
    }
}

impl std::error::Error for WorkflowError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
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
}

impl TryFrom<Vec<TaskDefinition>> for Workflow {
    type Error = WorkflowError;

    fn try_from(definitions: Vec<TaskDefinition>) -> Result<Self, Self::Error> {
        if definitions.is_empty() {
            return Err(WorkflowError::Empty);
        }

        let indexes = task_indexes(&definitions)?;

        let (dependencies, dependency_ids) = resolve_all_dependencies(&definitions, &indexes)?;

        validate_acyclic(&definitions, &dependency_ids)?;

        // End references into definitions before moving them into tasks.
        drop(dependency_ids);
        drop(indexes);
        let tasks = definitions
            .into_iter()
            .zip(dependencies)
            .map(|(definition, dependencies)| Task {
                id: definition.id,
                dependencies,
                request: definition.request,
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

impl WorkflowExecution<'_> {
    /// Returns all pending tasks whose dependencies succeeded.
    #[must_use]
    pub fn ready(&self) -> Vec<TaskIndex> {
        self.workflow
            .tasks
            .iter()
            .zip(&self.statuses)
            .enumerate()
            .filter(|(_, (task, status))| {
                **status == TaskStatus::Pending
                    && task.dependencies().iter().all(|dependency| {
                        self.statuses.get(dependency.position()) == Some(&TaskStatus::Succeeded)
                    })
            })
            .map(|(index, _)| TaskIndex(index))
            .collect()
    }

    /// Marks a ready task as running and returns it.
    pub fn start(&mut self, index: TaskIndex) -> Option<&Task> {
        if !self.ready().contains(&index) {
            return None;
        }

        *self.statuses.get_mut(index.position())? = TaskStatus::Running;
        self.workflow.task(index)
    }

    /// Records a running task's outcome.
    pub fn complete(&mut self, index: TaskIndex, succeeded: bool) -> bool {
        let Some(status) = self.statuses.get_mut(index.position()) else {
            return false;
        };
        if *status != TaskStatus::Running {
            return false;
        }

        *status = if succeeded {
            TaskStatus::Succeeded
        } else {
            TaskStatus::Failed
        };
        true
    }

    /// True while tasks remain pending.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.statuses.contains(&TaskStatus::Pending)
    }
}

fn task_indexes(
    definitions: &[TaskDefinition],
) -> Result<HashMap<&TaskId, TaskIndex>, WorkflowError> {
    let mut indexes = HashMap::with_capacity(definitions.len());

    for (position, definition) in definitions.iter().enumerate() {
        if indexes
            .insert(&definition.id, TaskIndex(position))
            .is_some()
        {
            return Err(WorkflowError::DuplicateTask(definition.id.clone()));
        }
    }

    Ok(indexes)
}

fn resolve_dependencies<'a>(
    definition: &'a TaskDefinition,
    task_index: TaskIndex,
    indexes: &HashMap<&'a TaskId, TaskIndex>,
) -> Result<(Vec<TaskIndex>, Vec<&'a TaskId>), WorkflowError> {
    let mut dependencies = Vec::with_capacity(definition.depends_on.len());
    let mut dependency_ids = Vec::with_capacity(definition.depends_on.len());

    for dependency_id in &definition.depends_on {
        let Some((resolved_id, &dependency)) = indexes.get_key_value(dependency_id) else {
            return Err(WorkflowError::UnknownDependency {
                task: definition.id.clone(),
                dependency: dependency_id.clone(),
            });
        };
        if dependency == task_index {
            return Err(WorkflowError::SelfDependency(definition.id.clone()));
        }
        dependencies.push(dependency);
        dependency_ids.push(*resolved_id);
    }

    Ok((dependencies, dependency_ids))
}

fn resolve_all_dependencies<'a>(
    definitions: &'a [TaskDefinition],
    indexes: &HashMap<&'a TaskId, TaskIndex>,
) -> Result<(Vec<Vec<TaskIndex>>, HashMap<&'a TaskId, Vec<&'a TaskId>>), WorkflowError> {
    let mut dependencies = Vec::with_capacity(definitions.len());
    let mut dependency_ids = HashMap::with_capacity(definitions.len());

    for (position, definition) in definitions.iter().enumerate() {
        let task_index = TaskIndex(position);
        let (task_dependencies, task_dependency_ids) =
            resolve_dependencies(definition, task_index, indexes)?;

        dependencies.push(task_dependencies);
        dependency_ids.insert(&definition.id, task_dependency_ids);
    }

    Ok((dependencies, dependency_ids))
}

fn validate_acyclic<'a>(
    definitions: &'a [TaskDefinition],
    dependencies: &HashMap<&'a TaskId, Vec<&'a TaskId>>,
) -> Result<(), WorkflowError> {
    let mut states = HashMap::with_capacity(definitions.len());
    let mut trail = Vec::new();

    for definition in definitions {
        visit(&definition.id, dependencies, &mut states, &mut trail)?;
    }

    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Visit {
    Unseen,
    Visiting,
    Complete,
}

fn visit<'a>(
    id: &'a TaskId,
    dependencies: &HashMap<&'a TaskId, Vec<&'a TaskId>>,
    states: &mut HashMap<&'a TaskId, Visit>,
    trail: &mut Vec<&'a TaskId>,
) -> Result<(), WorkflowError> {
    match states.get(id).copied().unwrap_or(Visit::Unseen) {
        Visit::Complete => return Ok(()),
        Visit::Visiting => {
            let start = trail
                .iter()
                .position(|task| *task == id)
                .unwrap_or_default();
            let cycle = trail
                .iter()
                .skip(start)
                .map(|task| (*task).clone())
                .chain(std::iter::once(id.clone()))
                .collect();
            return Err(WorkflowError::Cycle(cycle));
        }
        Visit::Unseen => {}
    }

    states.insert(id, Visit::Visiting);
    trail.push(id);

    if let Some(task_dependencies) = dependencies.get(id) {
        for dependency in task_dependencies {
            visit(dependency, dependencies, states, trail)?;
        }
    }

    let _ = trail.pop();
    states.insert(id, Visit::Complete);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::task::{TaskDefinition, TaskIndex, TaskRequest};

    use super::{Workflow, WorkflowError, WorkflowExecution};

    fn task(id: &str, depends_on: &[&str]) -> TaskDefinition {
        TaskDefinition::new(
            id.parse().unwrap(),
            depends_on
                .iter()
                .map(|dependency| dependency.parse().unwrap())
                .collect(),
            TaskRequest::command("true".into(), Vec::new()),
        )
    }

    fn succeeded<'workflow>(
        mut execution: WorkflowExecution<'workflow>,
        tasks: &[TaskIndex],
    ) -> WorkflowExecution<'workflow> {
        for index in tasks {
            assert!(execution.start(*index).is_some());
            assert!(execution.complete(*index, true));
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

        assert!(sut.start(TaskIndex(1)).is_none());
        assert_eq!(sut.ready(), [TaskIndex(0)]);
    }

    #[test]
    fn does_not_start_a_task_twice() {
        let workflow = Workflow::try_from(vec![task("prepare", &[])]).unwrap();
        let mut sut = workflow.execution();
        assert!(sut.start(TaskIndex(0)).is_some());

        assert!(sut.start(TaskIndex(0)).is_none());
        assert!(sut.ready().is_empty());
    }

    #[test]
    fn does_not_complete_a_task_that_has_not_started() {
        let workflow = Workflow::try_from(vec![task("prepare", &[])]).unwrap();
        let mut sut = workflow.execution();

        assert!(!sut.complete(TaskIndex(0), true));
        assert_eq!(sut.ready(), [TaskIndex(0)]);
    }

    #[test]
    fn does_not_change_a_completed_task_outcome() {
        let workflow =
            Workflow::try_from(vec![task("prepare", &[]), task("test", &["prepare"])]).unwrap();
        let mut sut = workflow.execution();
        assert!(sut.start(TaskIndex(0)).is_some());
        assert!(sut.complete(TaskIndex(0), true));

        assert!(!sut.complete(TaskIndex(0), false));
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
        assert!(sut.start(TaskIndex(0)).is_some());

        assert!(sut.complete(TaskIndex(0), false));

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
        assert!(sut.start(TaskIndex(1)).is_some());

        assert!(sut.complete(TaskIndex(1), false));

        assert!(sut.ready().is_empty());
        assert!(sut.has_pending());
    }

    #[test]
    fn does_not_release_dependents_after_a_predecessor_fails() {
        let workflow =
            Workflow::try_from(vec![task("prepare", &[]), task("test", &["prepare"])]).unwrap();
        let mut sut = workflow.execution();
        assert!(sut.start(TaskIndex(0)).is_some());

        assert!(sut.complete(TaskIndex(0), false));

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
