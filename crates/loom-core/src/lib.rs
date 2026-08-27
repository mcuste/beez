//! Validated workflow identifiers and dependency DAGs.

use std::collections::HashMap;
use std::fmt;

/// A workflow task identifier containing only ASCII letters, digits, and underscores.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TaskId(String);

impl TaskId {
    /// Validates and creates a task identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, TaskIdError> {
        let value = value.into();

        if value.is_empty() {
            return Err(TaskIdError::Empty);
        }

        if let Some(character) = value
            .chars()
            .find(|character| !character.is_ascii_alphanumeric() && *character != '_')
        {
            return Err(TaskIdError::InvalidCharacter(character));
        }

        Ok(Self(value))
    }

    /// Returns the validated identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Reports a task identifier validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskIdError {
    /// The identifier has no characters.
    Empty,
    /// The identifier includes a character outside the allowed alphabet.
    InvalidCharacter(char),
}

impl fmt::Display for TaskIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("task ID must not be empty"),
            Self::InvalidCharacter(character) => {
                write!(
                    formatter,
                    "task ID contains invalid character {character:?}"
                )
            }
        }
    }
}

impl std::error::Error for TaskIdError {}

/// A task and the tasks that must complete before it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Task {
    id: TaskId,
    depends_on: Vec<TaskId>,
}

impl Task {
    /// Creates a task from a valid identifier and its declared dependencies.
    #[must_use]
    pub fn new(id: TaskId, depends_on: Vec<TaskId>) -> Self {
        Self { id, depends_on }
    }

    /// Returns the task identifier.
    #[must_use]
    pub fn id(&self) -> &TaskId {
        &self.id
    }

    /// Returns the declared dependency identifiers.
    #[must_use]
    pub fn dependencies(&self) -> &[TaskId] {
        &self.depends_on
    }
}

/// A validated, acyclic collection of workflow tasks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workflow {
    tasks: Vec<Task>,
}

impl Workflow {
    /// Validates task identifiers, dependencies, and dependency cycles.
    pub fn new(tasks: Vec<Task>) -> Result<Self, WorkflowError> {
        let mut dependencies = HashMap::with_capacity(tasks.len());

        for task in &tasks {
            if dependencies
                .insert(task.id.clone(), task.depends_on.clone())
                .is_some()
            {
                return Err(WorkflowError::DuplicateTask(task.id.clone()));
            }
        }

        for (task, task_dependencies) in &dependencies {
            for dependency in task_dependencies {
                if dependency == task {
                    return Err(WorkflowError::SelfDependency(task.clone()));
                }
                if !dependencies.contains_key(dependency) {
                    return Err(WorkflowError::UnknownDependency {
                        task: task.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }

        let mut states = HashMap::with_capacity(tasks.len());
        let mut trail = Vec::new();
        for task in &tasks {
            visit(&task.id, &dependencies, &mut states, &mut trail)?;
        }

        Ok(Self { tasks })
    }

    /// Returns tasks in declaration order.
    #[must_use]
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }
}

/// Reports workflow DAG validation failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowError {
    /// More than one task uses this identifier.
    DuplicateTask(TaskId),
    /// A task names a dependency that is not part of the workflow.
    UnknownDependency {
        /// The task with the invalid dependency.
        task: TaskId,
        /// The missing dependency identifier.
        dependency: TaskId,
    },
    /// A task directly depends on itself.
    SelfDependency(TaskId),
    /// A sequence of task identifiers closes a dependency cycle.
    Cycle(Vec<TaskId>),
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum Visit {
    Unseen,
    Visiting,
    Complete,
}

fn visit(
    id: &TaskId,
    dependencies: &HashMap<TaskId, Vec<TaskId>>,
    states: &mut HashMap<TaskId, Visit>,
    trail: &mut Vec<TaskId>,
) -> Result<(), WorkflowError> {
    match states.get(id).copied().unwrap_or(Visit::Unseen) {
        Visit::Complete => return Ok(()),
        Visit::Visiting => {
            let start = trail.iter().position(|task| task == id).unwrap_or_default();
            let cycle = trail
                .iter()
                .skip(start)
                .cloned()
                .chain(std::iter::once(id.clone()))
                .collect();
            return Err(WorkflowError::Cycle(cycle));
        }
        Visit::Unseen => {}
    }

    states.insert(id.clone(), Visit::Visiting);
    trail.push(id.clone());

    if let Some(task_dependencies) = dependencies.get(id) {
        for dependency in task_dependencies {
            visit(dependency, dependencies, states, trail)?;
        }
    }

    let _ = trail.pop();
    states.insert(id.clone(), Visit::Complete);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Task, TaskId, TaskIdError, Workflow, WorkflowError};

    fn task(id: &str, depends_on: &[&str]) -> Task {
        Task::new(
            TaskId::new(id).unwrap(),
            depends_on
                .iter()
                .map(|dependency| TaskId::new(*dependency).unwrap())
                .collect(),
        )
    }

    #[test]
    fn rejects_invalid_task_ids() {
        assert_eq!(TaskId::new(""), Err(TaskIdError::Empty));
        assert_eq!(
            TaskId::new("build-app"),
            Err(TaskIdError::InvalidCharacter('-'))
        );
    }

    #[test]
    fn accepts_acyclic_dependencies() {
        let workflow =
            Workflow::new(vec![task("prepare", &[]), task("build", &["prepare"])]).unwrap();

        assert_eq!(workflow.tasks().len(), 2);
    }

    #[test]
    fn rejects_unknown_dependencies() {
        let error = Workflow::new(vec![task("build", &["prepare"])]).unwrap_err();

        assert_eq!(
            error,
            WorkflowError::UnknownDependency {
                task: TaskId::new("build").unwrap(),
                dependency: TaskId::new("prepare").unwrap(),
            }
        );
    }

    #[test]
    fn rejects_dependency_cycles() {
        let error = Workflow::new(vec![
            task("prepare", &["build"]),
            task("build", &["prepare"]),
        ])
        .unwrap_err();

        assert_eq!(
            error,
            WorkflowError::Cycle(vec![
                TaskId::new("prepare").unwrap(),
                TaskId::new("build").unwrap(),
                TaskId::new("prepare").unwrap(),
            ])
        );
    }
}
