//! Manifest loading integration tests.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use loom_core::TaskRequest;
use loom_manifest::{ManifestError, load};

#[test]
fn loads_yaml_tasks_and_resolves_dependencies() {
    let manifest = temporary_manifest(
        "yaml-workflow",
        "yaml",
        "tasks:\n  - id: inspect\n    harness: pi\n    prompt: inspect the repository\n  - id: summarize\n    depends_on: [inspect]\n    harness: omp\n    prompt: summarize the findings\n  - id: test\n    depends_on: [inspect, summarize]\n    command: [cargo, test, --workspace]\n",
    )
    .unwrap();

    let workflow = load(manifest.path()).unwrap();
    let tasks = workflow.tasks();
    let inspect = tasks.first().unwrap();
    let summarize = tasks.get(1).unwrap();
    let test = tasks.get(2).unwrap();

    assert_eq!(inspect.id().as_str(), "inspect");
    assert_eq!(inspect.dependencies(), []);
    assert_eq!(
        inspect.request(),
        &TaskRequest::pi("inspect the repository".into())
    );
    assert_eq!(summarize.id().as_str(), "summarize");
    assert_eq!(
        summarize
            .dependencies()
            .iter()
            .map(|dependency| dependency.position())
            .collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(
        summarize.request(),
        &TaskRequest::omp("summarize the findings".into())
    );
    assert_eq!(test.id().as_str(), "test");
    assert_eq!(
        test.dependencies()
            .iter()
            .map(|dependency| dependency.position())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        test.request(),
        &TaskRequest::command("cargo".into(), vec!["test".into(), "--workspace".into()])
    );
}

#[test]
fn loads_json_command_task() {
    let manifest = temporary_manifest(
        "json-workflow",
        "json",
        r#"{"tasks":[{"id":"lint","command":["cargo","clippy","--workspace"]}]}"#,
    )
    .unwrap();

    let workflow = load(manifest.path()).unwrap();
    let task = workflow.tasks().first().unwrap();

    assert_eq!(task.id().as_str(), "lint");
    assert_eq!(task.dependencies(), []);
    assert_eq!(
        task.request(),
        &TaskRequest::command("cargo".into(), vec!["clippy".into(), "--workspace".into()])
    );
}

#[test]
fn rejects_malformed_yaml() {
    let manifest = temporary_manifest("malformed-yaml", "yaml", "tasks: [").unwrap();

    assert!(matches!(
        load(manifest.path()),
        Err(ManifestError::Invalid(_))
    ));
}

#[test]
fn rejects_malformed_json() {
    let manifest = temporary_manifest("malformed-json", "json", r#"{"tasks":[}"#).unwrap();

    assert!(matches!(
        load(manifest.path()),
        Err(ManifestError::Invalid(_))
    ));
}

#[test]
fn rejects_unknown_task_fields() {
    let manifest = temporary_manifest(
        "unknown-field",
        "yaml",
        "tasks:\n  - id: inspect\n    command: [echo, inspect]\n    unexpected: value\n",
    )
    .unwrap();

    assert!(matches!(
        load(manifest.path()),
        Err(ManifestError::Invalid(_))
    ));
}

#[test]
fn rejects_tasks_without_a_request() {
    let manifest =
        temporary_manifest("missing-request", "yaml", "tasks:\n  - id: inspect\n").unwrap();

    assert!(matches!(
        load(manifest.path()),
        Err(ManifestError::Invalid(_))
    ));
}

#[test]
fn rejects_tasks_with_conflicting_requests() {
    let manifest = temporary_manifest(
        "conflicting-request",
        "yaml",
        "tasks:\n  - id: inspect\n    harness: pi\n    prompt: inspect the repository\n    command: [echo, inspect]\n",
    )
    .unwrap();

    assert!(matches!(
        load(manifest.path()),
        Err(ManifestError::Invalid(_))
    ));
}

#[test]
fn rejects_commands_without_a_program() {
    let manifest = temporary_manifest(
        "empty-command",
        "yaml",
        "tasks:\n  - id: inspect\n    command: []\n",
    )
    .unwrap();

    assert!(matches!(
        load(manifest.path()),
        Err(ManifestError::Invalid(_))
    ));
}

#[test]
fn rejects_invalid_task_ids() {
    let manifest = temporary_manifest(
        "invalid-id",
        "yaml",
        "tasks:\n  - id: inspect-repository\n    command: [echo, inspect]\n",
    )
    .unwrap();

    assert!(matches!(
        load(manifest.path()),
        Err(ManifestError::Invalid(_))
    ));
}

#[test]
fn rejects_unknown_dependencies() {
    let manifest = temporary_manifest(
        "unknown-dependency",
        "yaml",
        "tasks:\n  - id: test\n    depends_on: [prepare]\n    command: [cargo, test]\n",
    )
    .unwrap();

    assert!(matches!(
        load(manifest.path()),
        Err(ManifestError::Invalid(_))
    ));
}

#[test]
fn reports_duplicate_task_ids_from_the_workflow() {
    let manifest = temporary_manifest(
        "duplicate-task",
        "yaml",
        "tasks:\n  - id: build\n    command: [echo, first]\n  - id: build\n    command: [echo, second]\n",
    )
    .unwrap();

    assert!(matches!(
        load(manifest.path()),
        Err(ManifestError::Invalid(error)) if error == "duplicate task ID build"
    ));
}

#[test]
fn reports_three_task_dependency_cycles_from_the_workflow() {
    let manifest = temporary_manifest(
        "dependency-cycle",
        "yaml",
        "tasks:\n  - id: prepare\n    depends_on: [build]\n    command: [echo, prepare]\n  - id: build\n    depends_on: [test]\n    command: [echo, build]\n  - id: test\n    depends_on: [prepare]\n    command: [echo, test]\n",
    )
    .unwrap();

    assert!(matches!(
        load(manifest.path()),
        Err(ManifestError::Invalid(error))
            if error == "workflow dependency cycle: prepare -> build -> test -> prepare"
    ));
}

#[test]
fn rejects_unsupported_file_extensions() {
    let manifest = temporary_manifest(
        "unsupported-extension",
        "toml",
        "tasks = [{ id = \"inspect\", command = [\"echo\", \"inspect\"] }]",
    )
    .unwrap();

    assert!(matches!(
        load(manifest.path()),
        Err(ManifestError::UnsupportedFormat(_))
    ));
}

#[test]
fn reports_unreadable_manifest_files() {
    let manifest = temporary_manifest(
        "missing-file",
        "yaml",
        "tasks:\n  - id: inspect\n    command: [echo, inspect]\n",
    )
    .unwrap();
    fs::remove_file(manifest.path()).unwrap();

    assert!(matches!(load(manifest.path()), Err(ManifestError::Io(_))));
}

struct TemporaryManifest {
    directory: PathBuf,
    path: PathBuf,
}

impl TemporaryManifest {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryManifest {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn temporary_manifest(
    name: &str,
    extension: &str,
    source: &str,
) -> Result<TemporaryManifest, Box<dyn Error>> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "loom-manifest-{name}-{}-{timestamp}",
        process::id()
    ));
    let path = directory.join(format!("workflow.{extension}"));

    fs::create_dir(&directory)?;
    fs::write(&path, source)?;

    Ok(TemporaryManifest { directory, path })
}
