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

impl std::error::Error for TaskIdError {}

/// A task ID of ASCII letters, digits, or underscores.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TaskId(String);

impl TaskId {
    /// The validated ID text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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

impl fmt::Display for TaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Model and effort settings for a harness prompt.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HarnessOptions {
    model: Option<String>,
    effort: Option<String>,
}

impl HarnessOptions {
    /// Builds harness options; `None` leaves the harness default.
    #[must_use]
    pub fn new(model: Option<String>, effort: Option<String>) -> Self {
        Self { model, effort }
    }

    /// The requested model.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The requested reasoning effort.
    #[must_use]
    pub fn effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }

    /// True when neither model nor effort is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.model.is_none() && self.effort.is_none()
    }
}

/// A task action Loom can execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskRequest {
    /// Sends a prompt to Pi.
    Pi {
        /// Prompt for Pi.
        prompt: String,
        /// Model and effort for Pi.
        options: HarnessOptions,
    },
    /// Sends a prompt to Oh My Pi.
    Omp {
        /// Prompt for Oh My Pi.
        prompt: String,
        /// Model and effort for Oh My Pi.
        options: HarnessOptions,
    },
    /// Runs a program without invoking a shell.
    Command {
        /// Program path or name.
        program: String,
        /// Literal program arguments.
        arguments: Vec<String>,
    },
}

impl TaskRequest {
    /// Builds a Pi request.
    #[must_use]
    pub fn pi(prompt: String, options: HarnessOptions) -> Self {
        Self::Pi { prompt, options }
    }

    /// Builds an Oh My Pi request.
    #[must_use]
    pub fn omp(prompt: String, options: HarnessOptions) -> Self {
        Self::Omp { prompt, options }
    }

    /// Builds a direct process request.
    #[must_use]
    pub fn command(program: String, arguments: Vec<String>) -> Self {
        Self::Command { program, arguments }
    }
}

/// A task declaration with dependencies identified by task ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskDefinition {
    pub(crate) id: TaskId,
    pub(crate) depends_on: Vec<TaskId>,
    pub(crate) request: TaskRequest,
}

impl TaskDefinition {
    /// Builds an unresolved task declaration.
    #[must_use]
    pub fn new(id: TaskId, depends_on: Vec<TaskId>, request: TaskRequest) -> Self {
        Self {
            id,
            depends_on,
            request,
        }
    }
}

/// A resolved dependency reference into a workflow's task list.
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
    pub(crate) request: TaskRequest,
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

    /// The action to execute after dependencies succeed.
    #[must_use]
    pub fn request(&self) -> &TaskRequest {
        &self.request
    }
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

#[cfg(test)]
mod tests {
    use super::{TaskId, TaskIdError};

    #[test]
    fn accepts_ascii_letters_digits_and_underscores() {
        let sut = "Build_42".parse::<TaskId>().unwrap();

        assert_eq!(sut.as_str(), "Build_42");
    }

    #[test]
    fn rejects_empty_task_ids() {
        assert_eq!("".parse::<TaskId>(), Err(TaskIdError::Empty));
    }

    #[test]
    fn rejects_punctuation_in_task_ids() {
        assert_eq!(
            "build-app".parse::<TaskId>(),
            Err(TaskIdError::InvalidCharacter('-'))
        );
    }

    #[test]
    fn rejects_non_ascii_characters_in_task_ids() {
        assert_eq!(
            "buildé".parse::<TaskId>(),
            Err(TaskIdError::InvalidCharacter('é'))
        );
    }
}
