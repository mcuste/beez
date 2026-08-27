use std::collections::HashMap;
use std::fmt;

use crate::task::{Task, TaskDefinition, TaskId, TaskIndex};

/// Reports invalid workflow dependencies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowError {
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
}

impl TryFrom<Vec<TaskDefinition>> for Workflow {
    type Error = WorkflowError;

    fn try_from(definitions: Vec<TaskDefinition>) -> Result<Self, Self::Error> {
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
            })
            .collect();

        Ok(Self { tasks })
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
    use std::collections::HashMap;

    use crate::task::{TaskDefinition, TaskIndex};

    use super::{
        Workflow, WorkflowError, resolve_all_dependencies, resolve_dependencies, task_indexes,
        validate_acyclic, visit,
    };

    fn task(id: &str, depends_on: &[&str]) -> TaskDefinition {
        TaskDefinition::new(
            id.parse().unwrap(),
            depends_on
                .iter()
                .map(|dependency| dependency.parse().unwrap())
                .collect(),
        )
    }

    #[test]
    fn indexes_tasks_in_declaration_order() {
        let definitions = vec![task("build", &[]), task("prepare", &[])];

        let sut = task_indexes(&definitions).unwrap();

        assert_eq!(
            sut.get(&definitions.first().unwrap().id),
            Some(&TaskIndex(0))
        );
        assert_eq!(
            sut.get(&definitions.last().unwrap().id),
            Some(&TaskIndex(1))
        );
    }

    #[test]
    fn resolves_a_task_dependency_to_its_index() {
        let definitions = vec![task("build", &["prepare"]), task("prepare", &[])];
        let indexes = task_indexes(&definitions).unwrap();

        let (dependencies, dependency_ids) =
            resolve_dependencies(definitions.first().unwrap(), TaskIndex(0), &indexes).unwrap();

        assert_eq!(dependencies, [TaskIndex(1)]);
        assert_eq!(dependency_ids, [&definitions.last().unwrap().id]);
    }

    #[test]
    fn resolves_all_task_dependencies() {
        let definitions = vec![task("build", &["prepare"]), task("prepare", &[])];
        let indexes = task_indexes(&definitions).unwrap();

        let (dependencies, dependency_ids) =
            resolve_all_dependencies(&definitions, &indexes).unwrap();

        assert_eq!(dependencies, [vec![TaskIndex(1)], vec![]]);
        assert_eq!(
            dependency_ids
                .get(&definitions.first().unwrap().id)
                .unwrap()
                .as_slice(),
            [&definitions.last().unwrap().id]
        );
    }

    #[test]
    fn detects_a_cycle_in_resolved_dependencies() {
        let definitions = vec![task("prepare", &["build"]), task("build", &["prepare"])];
        let indexes = task_indexes(&definitions).unwrap();
        let (_, dependencies) = resolve_all_dependencies(&definitions, &indexes).unwrap();

        let error = validate_acyclic(&definitions, &dependencies).unwrap_err();

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
    fn visit_reports_the_cycle_path() {
        let definitions = vec![task("prepare", &["build"]), task("build", &["prepare"])];
        let indexes = task_indexes(&definitions).unwrap();
        let (_, dependencies) = resolve_all_dependencies(&definitions, &indexes).unwrap();
        let mut states = HashMap::new();
        let mut trail = Vec::new();

        let error = visit(
            &definitions.first().unwrap().id,
            &dependencies,
            &mut states,
            &mut trail,
        )
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
    fn resolves_dependencies_to_task_indexes() {
        let workflow =
            Workflow::try_from(vec![task("build", &["prepare"]), task("prepare", &[])]).unwrap();

        let build = workflow.tasks().first().unwrap();
        let prepare = workflow.tasks().last().unwrap();
        assert_eq!(build.id().as_str(), "build");
        assert_eq!(build.dependencies().first().unwrap().position(), 1);
        assert_eq!(prepare.id().as_str(), "prepare");
        assert!(prepare.dependencies().is_empty());
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
}
