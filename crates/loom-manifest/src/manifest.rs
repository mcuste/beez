use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use loom_core::{TaskDefinition, TaskId, TaskRequest, Workflow};
use serde::Deserialize;

/// Reports an unreadable or invalid workflow manifest.
#[derive(Debug)]
pub enum ManifestError {
    /// The manifest file could not be read.
    Io(std::io::Error),
    /// The manifest extension is unsupported.
    UnsupportedFormat(PathBuf),
    /// The manifest does not match Loom's workflow schema.
    Invalid(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    tasks: Vec<ManifestTask>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestTask {
    id: String,
    #[serde(default)]
    depends_on: Vec<String>,
    harness: Option<Harness>,
    prompt: Option<String>,
    command: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Harness {
    Pi,
    Omp,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::UnsupportedFormat(path) => write!(
                formatter,
                "workflow file {} must use a .yaml, .yml, or .json extension",
                path.display()
            ),
            Self::Invalid(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::UnsupportedFormat(_) | Self::Invalid(_) => None,
        }
    }
}

impl TryFrom<ManifestTask> for TaskDefinition {
    type Error = String;

    fn try_from(task: ManifestTask) -> Result<Self, Self::Error> {
        let id = task
            .id
            .parse::<TaskId>()
            .map_err(|error| error.to_string())?;
        let depends_on = task
            .depends_on
            .into_iter()
            .map(|dependency| {
                dependency
                    .parse::<TaskId>()
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let request = match (task.harness, task.prompt, task.command) {
            (Some(Harness::Pi), Some(prompt), None) => TaskRequest::pi(prompt),
            (Some(Harness::Omp), Some(prompt), None) => TaskRequest::omp(prompt),
            (None, None, Some(mut command)) => {
                let Some(program) = command.first().cloned() else {
                    return Err("command must contain a program".into());
                };
                command.remove(0);
                TaskRequest::command(program, command)
            }
            _ => return Err("task must define either harness and prompt, or command".into()),
        };

        Ok(TaskDefinition::new(id, depends_on, request))
    }
}

/// Loads and validates a workflow manifest.
pub fn load(path: &Path) -> Result<Workflow, ManifestError> {
    let source = fs::read_to_string(path).map_err(ManifestError::Io)?;
    let manifest: Manifest = match path.extension().and_then(|extension| extension.to_str()) {
        Some("yaml" | "yml") => {
            noyalib::from_str(&source).map_err(|error| ManifestError::Invalid(error.to_string()))?
        }
        Some("json") => serde_json::from_str(&source)
            .map_err(|error| ManifestError::Invalid(error.to_string()))?,
        _ => return Err(ManifestError::UnsupportedFormat(path.into())),
    };
    let definitions = manifest
        .tasks
        .into_iter()
        .map(TaskDefinition::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(ManifestError::Invalid)?;

    Workflow::try_from(definitions).map_err(|error| ManifestError::Invalid(error.to_string()))
}
