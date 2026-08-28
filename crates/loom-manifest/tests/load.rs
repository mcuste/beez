//! Manifest loading integration tests.

use std::fs;
use std::io;
use std::path::PathBuf;

use loom_core::{HarnessOptions, TaskRequest};
use loom_manifest::{ManifestError, load};
use loom_test_support::TemporaryDirectory;

#[test]
fn loads_yaml_tasks_and_resolves_dependencies() {
    let directory = TemporaryDirectory::new("manifest-yaml-workflow").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    harness: pi\n    prompt: inspect the repository\n  - id: summarize\n    depends_on: [inspect]\n    harness: omp\n    prompt: summarize the findings\n  - id: test\n    depends_on: [inspect, summarize]\n    command: [cargo, test, --workspace]\n",
    )
    .unwrap();

    let workflow = load(&manifest).unwrap();
    let tasks = workflow.tasks();
    let inspect = tasks.first().unwrap();
    let summarize = tasks.get(1).unwrap();
    let test = tasks.get(2).unwrap();

    assert_eq!(inspect.id().as_str(), "inspect");
    assert_eq!(inspect.dependencies(), []);
    assert_eq!(
        inspect.request(),
        &TaskRequest::pi("inspect the repository".into(), HarnessOptions::default())
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
        &TaskRequest::omp("summarize the findings".into(), HarnessOptions::default())
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
    let directory = TemporaryDirectory::new("manifest-json-workflow").unwrap();
    let manifest = write_manifest(
        &directory,
        "json",
        r#"{"tasks":[{"id":"lint","command":["cargo","clippy","--workspace"]}]}"#,
    )
    .unwrap();

    let workflow = load(&manifest).unwrap();
    let task = workflow.tasks().first().unwrap();

    assert_eq!(task.id().as_str(), "lint");
    assert_eq!(task.dependencies(), []);
    assert_eq!(
        task.request(),
        &TaskRequest::command("cargo".into(), vec!["clippy".into(), "--workspace".into()])
    );
}

#[test]
fn loads_harness_model_and_effort() {
    let directory = TemporaryDirectory::new("manifest-harness-options").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    harness: pi\n    prompt: inspect the repository\n    model: opus\n    effort: high\n",
    )
    .unwrap();

    let workflow = load(&manifest).unwrap();
    let task = workflow.tasks().first().unwrap();

    assert_eq!(
        task.request(),
        &TaskRequest::pi(
            "inspect the repository".into(),
            HarnessOptions::new(Some("opus".into()), Some("high".into())),
        )
    );
}

#[test]
fn loads_a_harness_model_without_an_effort() {
    let directory = TemporaryDirectory::new("manifest-harness-model-only").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    harness: omp\n    prompt: inspect the repository\n    model: opus\n",
    )
    .unwrap();

    let workflow = load(&manifest).unwrap();
    let task = workflow.tasks().first().unwrap();

    assert_eq!(
        task.request(),
        &TaskRequest::omp(
            "inspect the repository".into(),
            HarnessOptions::new(Some("opus".into()), None),
        )
    );
}

#[test]
fn loads_a_harness_effort_without_a_model() {
    let directory = TemporaryDirectory::new("manifest-harness-effort-only").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    harness: omp\n    prompt: inspect the repository\n    effort: high\n",
    )
    .unwrap();

    let workflow = load(&manifest).unwrap();
    let task = workflow.tasks().first().unwrap();

    assert_eq!(
        task.request(),
        &TaskRequest::omp(
            "inspect the repository".into(),
            HarnessOptions::new(None, Some("high".into())),
        )
    );
}

#[test]
fn rejects_a_model_on_command_tasks() {
    let directory = TemporaryDirectory::new("manifest-command-model").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: test\n    command: [cargo, test]\n    model: opus\n",
    )
    .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error)) if error == "model and effort require a harness task"
    ));
}

#[test]
fn rejects_an_effort_on_command_tasks() {
    let directory = TemporaryDirectory::new("manifest-command-effort").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: test\n    command: [cargo, test]\n    effort: high\n",
    )
    .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error)) if error == "model and effort require a harness task"
    ));
}

#[test]
fn loads_json_harness_model_and_effort() {
    let directory = TemporaryDirectory::new("manifest-json-harness-options").unwrap();
    let manifest = write_manifest(
        &directory,
        "json",
        r#"{"tasks":[{"id":"inspect","harness":"pi","prompt":"inspect","model":"opus","effort":"high"}]}"#,
    )
    .unwrap();

    let workflow = load(&manifest).unwrap();
    let task = workflow.tasks().first().unwrap();

    assert_eq!(
        task.request(),
        &TaskRequest::pi(
            "inspect".into(),
            HarnessOptions::new(Some("opus".into()), Some("high".into())),
        )
    );
}

#[test]
fn rejects_malformed_yaml() {
    let directory = TemporaryDirectory::new("manifest-malformed-yaml").unwrap();
    let manifest = write_manifest(&directory, "yaml", "tasks: [").unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_malformed_json() {
    let directory = TemporaryDirectory::new("manifest-malformed-json").unwrap();
    let manifest = write_manifest(&directory, "json", r#"{"tasks":[}"#).unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_unknown_task_fields() {
    let directory = TemporaryDirectory::new("manifest-unknown-field").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    command: [echo, inspect]\n    unexpected: value\n",
    )
    .unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_tasks_without_a_request() {
    let directory = TemporaryDirectory::new("manifest-missing-request").unwrap();
    let manifest = write_manifest(&directory, "yaml", "tasks:\n  - id: inspect\n").unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_tasks_with_conflicting_requests() {
    let directory = TemporaryDirectory::new("manifest-conflicting-request").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    harness: pi\n    prompt: inspect the repository\n    command: [echo, inspect]\n",
    )
    .unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_commands_without_a_program() {
    let directory = TemporaryDirectory::new("manifest-empty-command").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    command: []\n",
    )
    .unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_invalid_task_ids() {
    let directory = TemporaryDirectory::new("manifest-invalid-id").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect-repository\n    command: [echo, inspect]\n",
    )
    .unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_unknown_dependencies() {
    let directory = TemporaryDirectory::new("manifest-unknown-dependency").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: test\n    depends_on: [prepare]\n    command: [cargo, test]\n",
    )
    .unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn reports_duplicate_task_ids_from_the_workflow() {
    let directory = TemporaryDirectory::new("manifest-duplicate-task").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: build\n    command: [echo, first]\n  - id: build\n    command: [echo, second]\n",
    )
    .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error)) if error == "duplicate task ID build"
    ));
}

#[test]
fn reports_three_task_dependency_cycles_from_the_workflow() {
    let directory = TemporaryDirectory::new("manifest-dependency-cycle").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: prepare\n    depends_on: [build]\n    command: [echo, prepare]\n  - id: build\n    depends_on: [test]\n    command: [echo, build]\n  - id: test\n    depends_on: [prepare]\n    command: [echo, test]\n",
    )
    .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error))
            if error == "workflow dependency cycle: prepare -> build -> test -> prepare"
    ));
}

#[test]
fn rejects_unsupported_file_extensions() {
    let directory = TemporaryDirectory::new("manifest-unsupported-extension").unwrap();
    let manifest = write_manifest(
        &directory,
        "toml",
        "tasks = [{ id = \"inspect\", command = [\"echo\", \"inspect\"] }]",
    )
    .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::UnsupportedFormat(_))
    ));
}

#[test]
fn reports_unreadable_manifest_files() {
    let directory = TemporaryDirectory::new("manifest-missing-file").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    command: [echo, inspect]\n",
    )
    .unwrap();
    fs::remove_file(&manifest).unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Io(_))));
}

fn write_manifest(
    directory: &TemporaryDirectory,
    extension: &str,
    source: &str,
) -> io::Result<PathBuf> {
    let path = directory.join(&format!("workflow.{extension}"));
    fs::write(&path, source)?;

    Ok(path)
}
