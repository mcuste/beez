//! Manifest loading integration tests.

use std::fs;
use std::io;
use std::path::PathBuf;

use loom_core::{
    DomainGroup, ExecutableGroup, ExecutablePolicy, FilesystemPolicy, HarnessOptions,
    HeadlessHarness, NetworkPolicy, SandboxPolicy, TaskRequest,
};
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
        &TaskRequest::harness(
            HeadlessHarness::Pi,
            "inspect the repository".into(),
            HarnessOptions::default()
        )
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
        &TaskRequest::harness(
            HeadlessHarness::Omp,
            "summarize the findings".into(),
            HarnessOptions::default()
        )
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
        &TaskRequest::harness(
            HeadlessHarness::Pi,
            "inspect the repository".into(),
            HarnessOptions::new(Some("opus".into()), Some("high".into())),
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
fn loads_claude_and_codex_harness_tasks() {
    let directory = TemporaryDirectory::new("manifest-claude-codex").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    harness: claude\n    prompt: inspect the repository\n  - id: review\n    depends_on: [inspect]\n    harness: codex\n    prompt: review the findings\n    effort: high\n",
    )
    .unwrap();

    let workflow = load(&manifest).unwrap();
    let inspect = workflow.tasks().first().unwrap();
    let review = workflow.tasks().get(1).unwrap();

    assert_eq!(
        inspect.request(),
        &TaskRequest::harness(
            HeadlessHarness::Claude,
            "inspect the repository".into(),
            HarnessOptions::default(),
        )
    );
    assert_eq!(
        review.request(),
        &TaskRequest::harness(
            HeadlessHarness::Codex,
            "review the findings".into(),
            HarnessOptions::new(None, Some("high".into())),
        )
    );
}

#[test]
fn reports_the_rejected_harness_name() {
    let directory = TemporaryDirectory::new("manifest-unsupported-harness").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    harness: cursor\n    prompt: inspect the repository\n",
    )
    .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error)) if error.contains("cursor")
    ));
}

#[test]
fn rejects_a_harness_without_a_prompt() {
    let directory = TemporaryDirectory::new("manifest-harness-without-prompt").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    harness: claude\n",
    )
    .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error))
            if error == "task must define either harness and prompt, or command"
    ));
}

#[test]
fn rejects_a_prompt_without_a_harness() {
    let directory = TemporaryDirectory::new("manifest-prompt-without-harness").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: inspect\n    prompt: inspect the repository\n",
    )
    .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error))
            if error == "task must define either harness and prompt, or command"
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
        &TaskRequest::harness(
            HeadlessHarness::Pi,
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
fn rejects_a_workflow_without_tasks() {
    let directory = TemporaryDirectory::new("manifest-without-tasks").unwrap();
    let manifest = write_manifest(&directory, "yaml", "tasks: []\n").unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error)) if error == "workflow must define at least one task"
    ));
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

#[test]
fn applies_a_workflow_sandbox_to_every_task_unless_a_task_opts_out() {
    let directory = TemporaryDirectory::new("manifest-workflow-sandbox").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "sandbox:\n  network:\n    groups: [github]\n    allow: [\"registry.internal:443\"]\ntasks:\n  - id: inspect\n    harness: claude\n    prompt: inspect the repository\n  - id: test\n    command: [cargo, test]\n    sandbox: false\n",
    )
    .unwrap();

    let workflow = load(&manifest).unwrap();
    let inspect = workflow.tasks().first().unwrap();
    let test = workflow.tasks().get(1).unwrap();

    let expected = SandboxPolicy::new(
        NetworkPolicy::new(
            true,
            vec![DomainGroup::Github],
            Vec::new(),
            vec!["registry.internal:443".parse().unwrap()],
            false,
        ),
        FilesystemPolicy::default(),
        None,
    );
    assert_eq!(inspect.sandbox(), Some(&expected));
    assert_eq!(test.sandbox(), None);
}

#[test]
fn enables_the_default_sandbox_with_a_boolean() {
    let directory = TemporaryDirectory::new("manifest-sandbox-true").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: test\n    command: [cargo, test]\n    sandbox: true\n",
    )
    .unwrap();

    let workflow = load(&manifest).unwrap();
    let task = workflow.tasks().first().unwrap();

    assert_eq!(task.sandbox(), Some(&SandboxPolicy::default()));
}

#[test]
fn replaces_workflow_sandbox_sections_with_task_sections() {
    let directory = TemporaryDirectory::new("manifest-task-sandbox-override").unwrap();
    let manifest = write_manifest(
        &directory,
        "yaml",
        concat!(
            "sandbox:\n",
            "  network:\n    groups: [github]\n",
            "  filesystem:\n    write_allow: [/data]\n",
            "tasks:\n",
            "  - id: build\n    command: [cargo, build]\n",
            "    sandbox:\n",
            "      network:\n        defaults: false\n        localhost: true\n",
            "      executables:\n        groups: [rust]\n        disable: [net]\n        allow: [~/.local/share/mise]\n",
        ),
    )
    .unwrap();

    let workflow = load(&manifest).unwrap();
    let task = workflow.tasks().first().unwrap();

    let expected = SandboxPolicy::new(
        NetworkPolicy::new(false, Vec::new(), Vec::new(), Vec::new(), true),
        FilesystemPolicy::new(true, Vec::new(), vec!["/data".parse().unwrap()], Vec::new()),
        Some(ExecutablePolicy::new(
            true,
            vec![ExecutableGroup::Rust],
            vec![ExecutableGroup::Net],
            vec!["~/.local/share/mise".parse().unwrap()],
        )),
    );
    assert_eq!(task.sandbox(), Some(&expected));
}

#[test]
fn loads_a_json_sandbox() {
    let directory = TemporaryDirectory::new("manifest-json-sandbox").unwrap();
    let manifest = write_manifest(
        &directory,
        "json",
        r#"{"tasks":[{"id":"lint","command":["cargo","clippy"],"sandbox":{"filesystem":{"defaults":false,"read_deny":["~/.secrets"],"write_allow":["."]}}}]}"#,
    )
    .unwrap();

    let workflow = load(&manifest).unwrap();
    let task = workflow.tasks().first().unwrap();

    let expected = SandboxPolicy::new(
        NetworkPolicy::default(),
        FilesystemPolicy::new(
            false,
            vec!["~/.secrets".parse().unwrap()],
            vec![".".parse().unwrap()],
            Vec::new(),
        ),
        None,
    );
    assert_eq!(task.sandbox(), Some(&expected));
}

#[test]
fn rejects_unknown_sandbox_groups_and_fields() {
    let directory = TemporaryDirectory::new("manifest-sandbox-invalid").unwrap();
    let unknown_group = write_manifest(
        &directory,
        "yaml",
        "tasks:\n  - id: test\n    command: [cargo, test]\n    sandbox:\n      network:\n        groups: [gitlab]\n",
    )
    .unwrap();
    let unknown_field = write_manifest(
        &directory,
        "json",
        r#"{"tasks":[{"id":"test","command":["cargo","test"],"sandbox":{"network":{"domains":[]}}}]}"#,
    )
    .unwrap();
    let bad_rule = write_manifest(
        &directory,
        "json",
        r#"{"tasks":[{"id":"test","command":["cargo","test"],"sandbox":{"network":{"allow":["a.*.com"]}}}]}"#,
    )
    .unwrap();

    assert!(matches!(
        load(&unknown_group),
        Err(ManifestError::Invalid(error)) if error.contains("gitlab")
    ));
    assert!(matches!(
        load(&unknown_field),
        Err(ManifestError::Invalid(_))
    ));
    assert!(matches!(
        load(&bad_rule),
        Err(ManifestError::Invalid(error)) if error.contains("`*`")
    ));
}
