use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TaskId(String);

impl TaskId {
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

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskIdError {
    Empty,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Task {
    pub id: TaskId,
    pub depends_on: Vec<TaskId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workflow {
    tasks: Vec<Task>,
}

impl Workflow {
    pub fn new(tasks: Vec<Task>) -> Result<Self, WorkflowError> {
        let mut indexes = HashMap::with_capacity(tasks.len());

        for (index, task) in tasks.iter().enumerate() {
            if indexes.insert(task.id.clone(), index).is_some() {
                return Err(WorkflowError::DuplicateTask(task.id.clone()));
            }
        }

        for task in &tasks {
            for dependency in &task.depends_on {
                if dependency == &task.id {
                    return Err(WorkflowError::SelfDependency(task.id.clone()));
                }
                if !indexes.contains_key(dependency) {
                    return Err(WorkflowError::UnknownDependency {
                        task: task.id.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }

        let mut states = vec![Visit::Unseen; tasks.len()];
        let mut trail = Vec::new();
        for index in 0..tasks.len() {
            visit(index, &tasks, &indexes, &mut states, &mut trail)?;
        }

        Ok(Self { tasks })
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowError {
    DuplicateTask(TaskId),
    UnknownDependency { task: TaskId, dependency: TaskId },
    SelfDependency(TaskId),
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
    index: usize,
    tasks: &[Task],
    indexes: &HashMap<TaskId, usize>,
    states: &mut [Visit],
    trail: &mut Vec<TaskId>,
) -> Result<(), WorkflowError> {
    match states[index] {
        Visit::Complete => return Ok(()),
        Visit::Visiting => {
            let start = trail
                .iter()
                .position(|task| task == &tasks[index].id)
                .expect("visiting task is in the traversal trail");
            let mut cycle = trail[start..].to_vec();
            cycle.push(tasks[index].id.clone());
            return Err(WorkflowError::Cycle(cycle));
        }
        Visit::Unseen => {}
    }

    states[index] = Visit::Visiting;
    trail.push(tasks[index].id.clone());

    for dependency in &tasks[index].depends_on {
        let dependency_index = indexes[dependency];
        visit(dependency_index, tasks, indexes, states, trail)?;
    }

    trail.pop();
    states[index] = Visit::Complete;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Task, TaskId, TaskIdError, Workflow, WorkflowError};

    fn task(id: &str, depends_on: &[&str]) -> Task {
        Task {
            id: TaskId::new(id).unwrap(),
            depends_on: depends_on
                .iter()
                .map(|dependency| TaskId::new(*dependency).unwrap())
                .collect(),
        }
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
