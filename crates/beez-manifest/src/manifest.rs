use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use beez_core::{TaskDefinition, TaskId, TaskRequest, Workflow};
use beez_policy::{HarnessOptions, HeadlessHarness, SandboxPolicy, parse_all};
use serde::Deserialize;

use crate::sandbox::{ManifestSandbox, SandboxSetting};
use crate::schedule::{JobSchedule, ScheduleSetting};

/// Reports an unreadable or invalid workflow manifest.
#[derive(Debug)]
pub enum ManifestError {
    /// The manifest file could not be read.
    Io(std::io::Error),
    /// The manifest extension is unsupported.
    UnsupportedFormat(PathBuf),
    /// The manifest does not match Beez's workflow schema.
    Invalid(String),
    /// A prompt file a task names could not be read.
    PromptFile {
        /// The path as the manifest resolves it.
        path: PathBuf,
        /// Why the read failed.
        error: std::io::Error,
    },
}

/// A loaded workflow manifest: its tasks, and when it runs.
#[derive(Debug)]
pub struct Manifest {
    workflow: Workflow,
    schedules: Vec<JobSchedule>,
}

impl Manifest {
    /// The tasks and their dependencies.
    #[must_use]
    pub fn workflow(&self) -> &Workflow {
        &self.workflow
    }

    /// Takes the workflow out of the manifest.
    #[must_use]
    pub fn into_workflow(self) -> Workflow {
        self.workflow
    }

    /// Every schedule the manifest declares, in the order it wrote them.
    ///
    /// A manifest without a `schedule` section has none, so only `beez run`
    /// starts it.
    #[must_use]
    pub fn schedules(&self) -> &[JobSchedule] {
        &self.schedules
    }
}

/// A manifest as it is written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(default)]
    schedule: Option<ScheduleSetting>,
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
    prompt_file: Option<PathBuf>,
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
            Self::PromptFile { path, error } => {
                write!(
                    formatter,
                    "cannot read prompt file {}: {error}",
                    path.display()
                )
            }
        }
    }
}

impl ManifestError {
    fn invalid(error: impl fmt::Display) -> Self {
        Self::Invalid(error.to_string())
    }
}

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) | Self::PromptFile { error, .. } => Some(error),
            Self::UnsupportedFormat(_) | Self::Invalid(_) => None,
        }
    }
}

impl ManifestTask {
    /// Builds the task, with its prompt file read relative to `directory` and
    /// the sandbox resolved from the workflow and the task.
    fn into_definition(
        self,
        directory: &Path,
        workflow: Option<&SandboxPolicy>,
    ) -> Result<TaskDefinition, ManifestError> {
        let prompt = prompt(self.prompt, self.prompt_file, directory)?;
        let sandbox =
            crate::sandbox::resolve(workflow, self.sandbox).map_err(ManifestError::Invalid)?;
        let id = self.id.parse::<TaskId>().map_err(ManifestError::invalid)?;
        let depends_on = parse_all::<TaskId>(&self.depends_on).map_err(ManifestError::Invalid)?;
        let options = HarnessOptions::new(self.model, self.effort);
        let request = match (self.harness, prompt, self.command) {
            (Some(harness), Some(prompt), None) => {
                let harness = harness
                    .parse::<HeadlessHarness>()
                    .map_err(ManifestError::invalid)?;
                TaskRequest::harness(harness, prompt, options)
            }
            (None, None, Some(command)) => {
                if !options.is_empty() {
                    return Err(ManifestError::Invalid(
                        "model and effort require a harness task".into(),
                    ));
                }
                let mut command = command.into_iter();
                let Some(program) = command.next() else {
                    return Err(ManifestError::Invalid(
                        "command must contain a program".into(),
                    ));
                };
                TaskRequest::command(program, command.collect())
            }
            _ => {
                return Err(ManifestError::Invalid(
                    "task must define either harness with prompt or prompt_file, or command".into(),
                ));
            }
        };
        Ok(TaskDefinition::new(id, depends_on, request).sandboxed(sandbox))
    }
}

/// The prompt a task writes inline, or the contents of its `file`, relative to
/// `directory`.
fn prompt(
    prompt: Option<String>,
    file: Option<PathBuf>,
    directory: &Path,
) -> Result<Option<String>, ManifestError> {
    let Some(file) = file else {
        return Ok(prompt);
    };
    if prompt.is_some() {
        return Err(ManifestError::Invalid(
            "task must define either prompt or prompt_file, not both".into(),
        ));
    }
    let path = directory.join(file);
    fs::read_to_string(&path)
        .map(Some)
        .map_err(|error| ManifestError::PromptFile { path, error })
}

/// Loads and validates a workflow manifest.
pub fn load(path: &Path) -> Result<Manifest, ManifestError> {
    let source = fs::read_to_string(path).map_err(ManifestError::Io)?;
    let document: Document = match path.extension().and_then(|extension| extension.to_str()) {
        Some("yaml" | "yml") => noyalib::from_str(&source).map_err(ManifestError::invalid)?,
        Some("json") => serde_json::from_str(&source).map_err(ManifestError::invalid)?,
        _ => return Err(ManifestError::UnsupportedFormat(path.into())),
    };
    let schedules = crate::schedule::resolve(document.schedule).map_err(ManifestError::Invalid)?;
    let sandbox = document
        .sandbox
        .map(SandboxPolicy::try_from)
        .transpose()
        .map_err(ManifestError::Invalid)?;
    let directory = path.parent().unwrap_or_else(|| Path::new(""));
    let definitions = document
        .tasks
        .into_iter()
        .map(|task| task.into_definition(directory, sandbox.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let workflow = Workflow::try_from(definitions).map_err(ManifestError::invalid)?;

    Ok(Manifest {
        workflow,
        schedules,
    })
}
