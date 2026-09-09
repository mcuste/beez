use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use loom_core::{
    HarnessOptions, HeadlessHarness, SandboxPolicy, TaskDefinition, TaskId, TaskRequest, Workflow,
};
use serde::Deserialize;

use crate::sandbox::{ManifestSandbox, SandboxSetting, resolve};

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
    #[serde(default)]
    sandbox: Option<ManifestSandbox>,
    tasks: Vec<ManifestTask>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestTask {
    id: String,
    #[serde(default)]
    depends_on: Vec<String>,
    harness: Option<String>,
    prompt: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    command: Option<Vec<String>>,
    #[serde(default)]
    sandbox: Option<SandboxSetting>,
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

impl ManifestTask {
    fn into_definition(
        mut self,
        sandbox: Option<&SandboxPolicy>,
    ) -> Result<TaskDefinition, String> {
        let task_sandbox = resolve(sandbox, self.sandbox.take())?;
        let definition = TaskDefinition::try_from(self)?;
        Ok(match task_sandbox {
            Some(policy) => definition.sandboxed(policy),
            None => definition,
        })
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
        let options = HarnessOptions::new(task.model, task.effort);
        let request = match (task.harness, task.prompt, task.command) {
            (Some(harness), Some(prompt), None) => {
                let harness = harness
                    .parse::<HeadlessHarness>()
                    .map_err(|error| error.to_string())?;
                TaskRequest::harness(harness, prompt, options)
            }
            (None, None, Some(mut command)) => {
                if !options.is_empty() {
                    return Err("model and effort require a harness task".into());
                }
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
    let sandbox = manifest
        .sandbox
        .map(SandboxPolicy::try_from)
        .transpose()
        .map_err(ManifestError::Invalid)?;
    let definitions = manifest
        .tasks
        .into_iter()
        .map(|task| task.into_definition(sandbox.as_ref()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(ManifestError::Invalid)?;

    Workflow::try_from(definitions).map_err(|error| ManifestError::Invalid(error.to_string()))
}
