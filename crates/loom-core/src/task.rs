use std::fmt;
use std::str::FromStr;

/// Reports an invalid task ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskIdError {
    /// The ID is empty.
    Empty,
    /// The ID contains a character outside the allowed alphabet.
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

/// A task ID of ASCII letters, digits, or underscores.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TaskId(String);

impl TryFrom<String> for TaskId {
    type Error = TaskIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
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
}

impl FromStr for TaskId {
    type Err = TaskIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}

impl TaskId {
    /// The validated ID text.
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

/// A task declaration with dependencies identified by task ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskDefinition {
    pub(crate) id: TaskId,
    pub(crate) depends_on: Vec<TaskId>,
}

impl TaskDefinition {
    /// Builds an unresolved task declaration.
    #[must_use]
    pub fn new(id: TaskId, depends_on: Vec<TaskId>) -> Self {
        Self { id, depends_on }
    }
}

/// A resolved dependency reference into a workflow's task list.
///
/// An executor can use it to access per-task state without a task-ID lookup.
/// It does not determine execution order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TaskIndex(pub(crate) usize);

impl TaskIndex {
    /// Returns the position in the containing workflow's task list.
    #[must_use]
    pub fn position(self) -> usize {
        self.0
    }
}

/// A task with resolved predecessor references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Task {
    pub(crate) id: TaskId,
    pub(crate) dependencies: Vec<TaskIndex>,
}

impl Task {
    /// The task ID.
    #[must_use]
    pub fn id(&self) -> &TaskId {
        &self.id
    }

    /// Required predecessor positions.
    #[must_use]
    pub fn dependencies(&self) -> &[TaskIndex] {
        &self.dependencies
    }
}

#[cfg(test)]
mod tests {
    use super::{TaskId, TaskIdError};

    #[test]
    fn rejects_invalid_task_ids() {
        assert_eq!("".parse::<TaskId>(), Err(TaskIdError::Empty));
        assert_eq!(
            "build-app".parse::<TaskId>(),
            Err(TaskIdError::InvalidCharacter('-'))
        );
    }
}
