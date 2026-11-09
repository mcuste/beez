//! Manifest loading integration tests.

use std::fs;

use beez_core::TaskRequest;
use beez_manifest::{ManifestError, Overlap, load};
use beez_policy::{
    DomainGroup, ExecutableGroup, ExecutablePolicy, FilesystemPolicy, HarnessOptions,
    HeadlessHarness, NetworkPolicy, SandboxPolicy,
};
use beez_test_support::TemporaryDirectory;

#[test]
fn loads_yaml_tasks_and_resolves_dependencies() {
    let directory = TemporaryDirectory::new("manifest-yaml-workflow").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    harness: pi\n    prompt: inspect the repository\n  - id: summarize\n    depends_on: [inspect]\n    harness: omp\n    prompt: summarize the findings\n  - id: test\n    depends_on: [inspect, summarize]\n    command: [cargo, test, --workspace]\n",
        )
        .unwrap();

    let workflow = load(&manifest).unwrap().into_workflow();
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
    let manifest = directory
        .write(
            "workflow.json",
            r#"{"tasks":[{"id":"lint","command":["cargo","clippy","--workspace"]}]}"#,
        )
        .unwrap();

    let workflow = load(&manifest).unwrap().into_workflow();
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
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    harness: pi\n    prompt: inspect the repository\n    model: opus\n    effort: high\n",
        )
        .unwrap();

    let workflow = load(&manifest).unwrap().into_workflow();
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
    let manifest = directory
        .write(
            "workflow.yaml",
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
    let manifest = directory
        .write(
            "workflow.yaml",
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
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    harness: claude\n    prompt: inspect the repository\n  - id: review\n    depends_on: [inspect]\n    harness: codex\n    prompt: review the findings\n    effort: high\n",
        )
        .unwrap();

    let workflow = load(&manifest).unwrap().into_workflow();
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
    let manifest = directory
        .write(
            "workflow.yaml",
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
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    harness: claude\n",
        )
        .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error))
            if error == "task must define either harness with prompt or prompt_file, or command"
    ));
}

#[test]
fn rejects_a_prompt_without_a_harness() {
    let directory = TemporaryDirectory::new("manifest-prompt-without-harness").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    prompt: inspect the repository\n",
        )
        .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error))
            if error == "task must define either harness with prompt or prompt_file, or command"
    ));
}

#[test]
fn loads_a_prompt_file_relative_to_the_manifest() {
    let directory = TemporaryDirectory::new("manifest-prompt-file").unwrap();
    directory.subdirectory("prompts").unwrap();
    directory.subdirectory("nested").unwrap();
    directory
        .write(
            "prompts/review.md",
            "review the findings:\n{{ tasks.inspect.stdout }}\n",
        )
        .unwrap();
    let manifest = directory
        .write(
            "nested/workflow.yaml",
            "tasks:\n  - id: inspect\n    command: [git, status]\n  - id: review\n    depends_on: [inspect]\n    harness: claude\n    prompt_file: ../prompts/review.md\n",
        )
        .unwrap();

    let workflow = load(&manifest).unwrap().into_workflow();

    assert_eq!(
        workflow.tasks().get(1).unwrap().request(),
        &TaskRequest::harness(
            HeadlessHarness::Claude,
            "review the findings:\n{{ tasks.inspect.stdout }}\n".into(),
            HarnessOptions::default()
        )
    );
}

#[test]
fn rejects_a_prompt_together_with_a_prompt_file() {
    let directory = TemporaryDirectory::new("manifest-prompt-and-prompt-file").unwrap();
    directory.write("review.md", "review").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: review\n    harness: claude\n    prompt: review\n    prompt_file: review.md\n",
        )
        .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error))
            if error == "task must define either prompt or prompt_file, not both"
    ));
}

#[test]
fn reports_an_unreadable_prompt_file() {
    let directory = TemporaryDirectory::new("manifest-missing-prompt-file").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: review\n    harness: claude\n    prompt_file: missing.md\n",
        )
        .unwrap();

    let error = load(&manifest).unwrap_err();

    assert!(
        matches!(&error, ManifestError::PromptFile { path, .. } if path == &directory.join("missing.md"))
    );
    assert_eq!(
        error.to_string(),
        format!(
            "cannot read prompt file {}",
            directory.join("missing.md").display()
        )
    );
    assert!(std::error::Error::source(&error).is_some(), "{error:?}");
}

/// A task's id and shape are checked before its prompt file is read.
#[test]
fn reports_a_task_mistake_before_reading_its_prompt_file() {
    let command = invalid_manifest_error(
        "manifest-command-with-prompt-file",
        "workflow.yaml",
        "tasks:\n  - id: review\n    command: [echo]\n    prompt_file: missing.md\n",
    );
    let bad_id = invalid_manifest_error(
        "manifest-bad-id-with-prompt-file",
        "workflow.yaml",
        "tasks:\n  - id: bad-id\n    harness: claude\n    prompt_file: missing.md\n",
    );
    let bad_harness = invalid_manifest_error(
        "manifest-bad-harness-with-prompt-file",
        "workflow.yaml",
        "tasks:\n  - id: review\n    harness: nobody\n    prompt_file: missing.md\n",
    );

    assert_eq!(
        command.as_deref(),
        Some("task must define either harness with prompt or prompt_file, or command")
    );
    assert_eq!(
        bad_id.as_deref(),
        Some("task ID contains invalid character '-'")
    );
    let bad_harness = bad_harness.unwrap();
    assert!(bad_harness.contains("nobody"), "{bad_harness}");
}

#[test]
fn reports_an_output_reference_without_a_dependency() {
    let directory = TemporaryDirectory::new("manifest-output-without-dependency").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    command: [git, status]\n  - id: review\n    harness: claude\n    prompt: review {{ tasks.inspect.stdout }}\n",
        )
        .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error))
            if error == "task review reads the output of task inspect, so it must depend on it"
    ));
}

#[test]
fn loads_json_harness_model_and_effort() {
    let directory = TemporaryDirectory::new("manifest-json-harness-options").unwrap();
    let manifest = directory
        .write(
            "workflow.json",
            r#"{"tasks":[{"id":"inspect","harness":"pi","prompt":"inspect","model":"opus","effort":"high"}]}"#,
        )
        .unwrap();

    let workflow = load(&manifest).unwrap().into_workflow();
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
    let manifest = directory.write("workflow.yaml", "tasks: [").unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_malformed_json() {
    let directory = TemporaryDirectory::new("manifest-malformed-json").unwrap();
    let manifest = directory.write("workflow.json", r#"{"tasks":[}"#).unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_unknown_task_fields() {
    let directory = TemporaryDirectory::new("manifest-unknown-field").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    command: [echo, inspect]\n    unexpected: value\n",
        )
        .unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_tasks_without_a_request() {
    let directory = TemporaryDirectory::new("manifest-missing-request").unwrap();
    let manifest = directory
        .write("workflow.yaml", "tasks:\n  - id: inspect\n")
        .unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_tasks_with_conflicting_requests() {
    let directory = TemporaryDirectory::new("manifest-conflicting-request").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    harness: pi\n    prompt: inspect the repository\n    command: [echo, inspect]\n",
        )
        .unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_commands_without_a_program() {
    let directory = TemporaryDirectory::new("manifest-empty-command").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    command: []\n",
        )
        .unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_invalid_task_ids() {
    let directory = TemporaryDirectory::new("manifest-invalid-id").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect-repository\n    command: [echo, inspect]\n",
        )
        .unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn rejects_a_workflow_without_tasks() {
    let directory = TemporaryDirectory::new("manifest-without-tasks").unwrap();
    let manifest = directory.write("workflow.yaml", "tasks: []\n").unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error)) if error == "workflow must define at least one task"
    ));
}

#[test]
fn rejects_unknown_dependencies() {
    let directory = TemporaryDirectory::new("manifest-unknown-dependency").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: test\n    depends_on: [prepare]\n    command: [cargo, test]\n",
        )
        .unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Invalid(_))));
}

#[test]
fn reports_duplicate_task_ids_from_the_workflow() {
    let directory = TemporaryDirectory::new("manifest-duplicate-task").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
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
    let manifest = directory
        .write(
            "workflow.yaml",
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
    let manifest = directory
        .write(
            "workflow.toml",
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
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    command: [echo, inspect]\n",
        )
        .unwrap();
    fs::remove_file(&manifest).unwrap();

    assert!(matches!(load(&manifest), Err(ManifestError::Io(_))));
}

#[test]
fn applies_a_workflow_sandbox_to_every_task_unless_a_task_opts_out() {
    let directory = TemporaryDirectory::new("manifest-workflow-sandbox").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "sandbox:\n  network:\n    groups: [github]\n    allow: [\"registry.internal:443\"]\ntasks:\n  - id: inspect\n    harness: claude\n    prompt: inspect the repository\n  - id: test\n    command: [cargo, test]\n    sandbox: false\n",
        )
        .unwrap();

    let workflow = load(&manifest).unwrap().into_workflow();
    let inspect = workflow.tasks().first().unwrap();
    let test = workflow.tasks().get(1).unwrap();

    let expected = SandboxPolicy::new(
        NetworkPolicy::default()
            .with_groups(vec![DomainGroup::Github])
            .with_allow(vec!["registry.internal:443".parse().unwrap()]),
        FilesystemPolicy::default(),
        None,
    );
    assert_eq!(inspect.sandbox(), Some(&expected));
    assert_eq!(test.sandbox(), None);
}

#[test]
fn sandboxes_every_task_a_manifest_does_not_opt_out() {
    let directory = TemporaryDirectory::new("manifest-sandbox-default").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: build\n    command: [cargo, build]\n  - id: publish\n    command: [./publish.sh]\n    sandbox: false\n",
        )
        .unwrap();

    let workflow = load(&manifest).unwrap().into_workflow();

    assert_eq!(
        workflow.tasks().first().unwrap().sandbox(),
        Some(&SandboxPolicy::default())
    );
    assert_eq!(workflow.tasks().get(1).unwrap().sandbox(), None);
}

#[test]
fn enables_the_default_sandbox_with_a_boolean() {
    let directory = TemporaryDirectory::new("manifest-sandbox-true").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: test\n    command: [cargo, test]\n    sandbox: true\n",
        )
        .unwrap();

    let workflow = load(&manifest).unwrap().into_workflow();
    let task = workflow.tasks().first().unwrap();

    assert_eq!(task.sandbox(), Some(&SandboxPolicy::default()));
}

#[test]
fn adds_task_sandbox_sections_to_the_workflow_sections() {
    let directory = TemporaryDirectory::new("manifest-task-sandbox-adds").unwrap();
    let manifest = directory.write("workflow.yaml", concat!(
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

    let workflow = load(&manifest).unwrap().into_workflow();
    let task = workflow.tasks().first().unwrap();

    let expected = SandboxPolicy::new(
        NetworkPolicy::default()
            .with_defaults(Some(false))
            .with_groups(vec![DomainGroup::Github])
            .with_localhost(Some(true)),
        FilesystemPolicy::default().with_write_allow(vec!["/data".parse().unwrap()]),
        Some(
            ExecutablePolicy::default()
                .with_groups(vec![ExecutableGroup::Rust])
                .with_disable(vec![ExecutableGroup::Net])
                .with_allow(vec!["~/.local/share/mise".parse().unwrap()]),
        ),
    );
    assert_eq!(task.sandbox(), Some(&expected));
}

#[test]
fn loads_a_json_sandbox() {
    let directory = TemporaryDirectory::new("manifest-json-sandbox").unwrap();
    let manifest = directory
        .write(
            "workflow.json",
            r#"{"tasks":[{"id":"lint","command":["cargo","clippy"],"sandbox":{"filesystem":{"defaults":false,"read_deny":["~/.secrets"],"write_allow":["."]}}}]}"#,
        )
        .unwrap();

    let workflow = load(&manifest).unwrap().into_workflow();
    let task = workflow.tasks().first().unwrap();

    let expected = SandboxPolicy::new(
        NetworkPolicy::default(),
        FilesystemPolicy::default()
            .with_defaults(Some(false))
            .with_read_deny(vec!["~/.secrets".parse().unwrap()])
            .with_write_allow(vec![".".parse().unwrap()]),
        None,
    );
    assert_eq!(task.sandbox(), Some(&expected));
}

#[test]
fn rejects_unknown_sandbox_groups_and_fields() {
    let directory = TemporaryDirectory::new("manifest-sandbox-invalid").unwrap();
    let unknown_field_directory = TemporaryDirectory::new("manifest-sandbox-unknown").unwrap();
    let bad_rule_directory = TemporaryDirectory::new("manifest-sandbox-rule").unwrap();
    let unknown_group = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: test\n    command: [cargo, test]\n    sandbox:\n      network:\n        groups: [nowhere]\n",
        )
        .unwrap();
    let unknown_field = unknown_field_directory
        .write(
            "workflow.json",
            r#"{"tasks":[{"id":"test","command":["cargo","test"],"sandbox":{"network":{"domains":[]}}}]}"#,
        )
        .unwrap();
    let bad_rule = bad_rule_directory
        .write(
            "workflow.json",
            r#"{"tasks":[{"id":"test","command":["cargo","test"],"sandbox":{"network":{"allow":["a.*.com"]}}}]}"#,
        )
        .unwrap();

    assert!(matches!(
        load(&unknown_group),
        Err(ManifestError::Invalid(error)) if error.contains("nowhere")
    ));
    assert!(matches!(
        load(&unknown_field),
        Err(ManifestError::Invalid(error)) if error.contains("unknown field") && error.contains("domains")
    ));
    assert!(matches!(
        load(&bad_rule),
        Err(ManifestError::Invalid(error)) if error.contains("`*`")
    ));
}

#[test]
fn loads_one_cron_schedule_with_its_defaults() {
    let directory = TemporaryDirectory::new("manifest-one-schedule").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "schedule:\n  cron: \"0 3 * * *\"\ntasks:\n  - id: test\n    command: [cargo, test]\n",
        )
        .unwrap();

    let manifest = load(&manifest).unwrap();
    let schedule = manifest.schedules().first().unwrap();

    assert_eq!(manifest.schedules().len(), 1);
    assert_eq!(schedule.name(), None);
    assert_eq!(schedule.schedule().to_string(), "0 3 * * *");
    assert_eq!(schedule.on_overlap(), Overlap::Skip);
    assert!(!schedule.catch_up());
    assert!(schedule.enabled());
}

#[test]
fn loads_a_schedule_with_an_offset_and_policies() {
    let directory = TemporaryDirectory::new("manifest-schedule-policies").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "schedule:\n  cron: \"0 3 * * *\"\n  offset: \"+02:00\"\n  on_overlap: queue\n  catch_up: true\n  enabled: false\ntasks:\n  - id: test\n    command: [cargo, test]\n",
        )
        .unwrap();

    let manifest = load(&manifest).unwrap();
    let schedule = manifest.schedules().first().unwrap();

    assert_eq!(schedule.schedule().to_string(), "0 3 * * * +02:00");
    assert_eq!(schedule.on_overlap(), Overlap::Queue);
    assert!(schedule.catch_up());
    assert!(!schedule.enabled());
}

#[test]
fn loads_a_one_time_schedule() {
    let directory = TemporaryDirectory::new("manifest-once-schedule").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "schedule:\n  at: \"2026-09-10T03:00:00Z\"\ntasks:\n  - id: test\n    command: [cargo, test]\n",
        )
        .unwrap();

    let manifest = load(&manifest).unwrap();
    let schedule = manifest.schedules().first().unwrap();

    assert!(!schedule.schedule().is_recurring());
    assert_eq!(schedule.schedule().to_string(), "2026-09-10T03:00:00Z");
}

#[test]
fn loads_a_list_of_named_schedules() {
    let directory = TemporaryDirectory::new("manifest-many-schedules").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "schedule:\n  - name: weekday\n    cron: \"0 3 * * 1-5\"\n  - name: weekend\n    cron: \"0 5 * * 6,0\"\ntasks:\n  - id: test\n    command: [cargo, test]\n",
        )
        .unwrap();

    let manifest = load(&manifest).unwrap();
    let names: Vec<Option<&str>> = manifest
        .schedules()
        .iter()
        .map(beez_manifest::JobSchedule::name)
        .collect();

    assert_eq!(names, [Some("weekday"), Some("weekend")]);
}

#[test]
fn loads_a_manifest_without_a_schedule() {
    let directory = TemporaryDirectory::new("manifest-no-schedule").unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: test\n    command: [cargo, test]\n",
        )
        .unwrap();

    assert_eq!(load(&manifest).unwrap().schedules(), []);
}

#[test]
fn loads_a_null_or_empty_schedule_as_no_schedule() {
    for (label, file, source) in [
        (
            "manifest-null-schedule-yaml",
            "workflow.yaml",
            "schedule: null\ntasks:\n  - id: test\n    command: [cargo, test]\n",
        ),
        (
            "manifest-blank-schedule-yaml",
            "workflow.yaml",
            "schedule:\ntasks:\n  - id: test\n    command: [cargo, test]\n",
        ),
        (
            "manifest-empty-schedule-yaml",
            "workflow.yaml",
            "schedule: []\ntasks:\n  - id: test\n    command: [cargo, test]\n",
        ),
        (
            "manifest-null-schedule-json",
            "workflow.json",
            r#"{"schedule":null,"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
        ),
        (
            "manifest-empty-schedule-json",
            "workflow.json",
            r#"{"schedule":[],"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
        ),
    ] {
        let directory = TemporaryDirectory::new(label).unwrap();
        let manifest = directory.write(file, source).unwrap();

        assert_eq!(load(&manifest).unwrap().schedules(), [], "{label}");
    }
}

#[test]
fn rejects_a_schedule_that_names_neither_or_both_of_cron_and_at() {
    let neither_directory = TemporaryDirectory::new("manifest-schedule-neither").unwrap();
    let both_directory = TemporaryDirectory::new("manifest-schedule-both").unwrap();
    let neither = neither_directory
        .write(
            "workflow.json",
            r#"{"schedule":{"catch_up":true},"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
        )
        .unwrap();
    let both = both_directory
        .write(
            "workflow.json",
            r#"{"schedule":{"cron":"0 3 * * *","at":"2026-09-10T03:00:00Z"},"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
        )
        .unwrap();

    assert!(matches!(
        load(&neither),
        Err(ManifestError::Invalid(error)) if error == "schedule must define either cron or at"
    ));
    assert!(matches!(
        load(&both),
        Err(ManifestError::Invalid(error)) if error == "schedule must define either cron or at, not both"
    ));
}

/// The schedule's shape is checked before its offset is read.
#[test]
fn reports_the_schedule_shape_before_a_bad_offset() {
    let both = invalid_manifest_error(
        "manifest-schedule-both-bad-offset",
        "workflow.json",
        r#"{"schedule":{"cron":"0 3 * * *","at":"2026-09-10T03:00:00Z","offset":"later"},"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
    );
    let neither = invalid_manifest_error(
        "manifest-schedule-neither-bad-offset",
        "workflow.json",
        r#"{"schedule":{"offset":"later"},"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
    );
    let once = invalid_manifest_error(
        "manifest-schedule-once-bad-offset",
        "workflow.json",
        r#"{"schedule":{"at":"2026-09-10T03:00:00Z","offset":"later"},"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
    );
    let cron = invalid_manifest_error(
        "manifest-schedule-cron-bad-offset",
        "workflow.json",
        r#"{"schedule":{"cron":"0 3 * * *","offset":"later"},"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
    );

    assert_eq!(
        both.as_deref(),
        Some("schedule must define either cron or at, not both")
    );
    assert_eq!(
        neither.as_deref(),
        Some("schedule must define either cron or at")
    );
    assert_eq!(
        once.as_deref(),
        Some("at already names its own offset, so offset must be absent")
    );
    assert_eq!(
        cron.as_deref(),
        Some("later is not a fixed offset from UTC, such as Z, +02:00, or -05:30")
    );
}

#[test]
fn rejects_an_offset_on_a_one_time_schedule() {
    let directory = TemporaryDirectory::new("manifest-once-offset").unwrap();
    let manifest = directory
        .write(
            "workflow.json",
            r#"{"schedule":{"at":"2026-09-10T03:00:00Z","offset":"+02:00"},"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
        )
        .unwrap();

    assert!(matches!(
        load(&manifest),
        Err(ManifestError::Invalid(error)) if error.contains("at already names its own offset")
    ));
}

#[test]
fn rejects_a_cron_expression_the_scheduler_cannot_use() {
    let sub_minute_directory = TemporaryDirectory::new("manifest-sub-minute-cron").unwrap();
    let short_directory = TemporaryDirectory::new("manifest-short-cron").unwrap();
    let sub_minute = sub_minute_directory
        .write(
            "workflow.json",
            r#"{"schedule":{"cron":"* * * * * *"},"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
        )
        .unwrap();
    let short = short_directory
        .write(
            "workflow.json",
            r#"{"schedule":{"cron":"0 3 * *"},"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
        )
        .unwrap();

    assert!(matches!(
        load(&sub_minute),
        Err(ManifestError::Invalid(error)) if error.contains("at most once a minute")
    ));
    assert!(matches!(
        load(&short),
        Err(ManifestError::Invalid(error)) if error.contains("4 fields")
    ));
}

#[test]
fn rejects_a_list_of_schedules_that_does_not_name_each_one() {
    let unnamed_directory = TemporaryDirectory::new("manifest-unnamed-schedules").unwrap();
    let repeated_directory = TemporaryDirectory::new("manifest-repeated-schedules").unwrap();
    let unnamed = unnamed_directory
        .write(
            "workflow.json",
            r#"{"schedule":[{"cron":"0 3 * * *"},{"name":"late","cron":"0 5 * * *"}],"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
        )
        .unwrap();
    let repeated = repeated_directory
        .write(
            "workflow.json",
            r#"{"schedule":[{"name":"nightly","cron":"0 3 * * *"},{"name":"nightly","cron":"0 5 * * *"}],"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
        )
        .unwrap();

    assert!(matches!(
        load(&unnamed),
        Err(ManifestError::Invalid(error)) if error.contains("must name each of them")
    ));
    assert!(matches!(
        load(&repeated),
        Err(ManifestError::Invalid(error)) if error == "schedule name nightly is used twice"
    ));
}

/// Loads a manifest and returns its error message, or `None` when it is not invalid.
fn invalid_manifest_error(label: &str, file: &str, source: &str) -> Option<String> {
    let directory = TemporaryDirectory::new(label).ok()?;
    let manifest = directory.write(file, source).ok()?;
    match load(&manifest) {
        Err(ManifestError::Invalid(error)) => Some(error),
        _ => None,
    }
}

/// Asserts the error names `problem` rather than a generic shape mismatch.
fn assert_names_the_problem(error: &str, problems: &[&str]) {
    for problem in problems {
        assert!(error.contains(problem), "{error:?} lacks {problem:?}");
    }
    assert!(!error.contains("did not match any variant"), "{error:?}");
}

#[test]
fn reports_the_specific_mistake_inside_a_sandbox_table() {
    let yaml_unknown = invalid_manifest_error(
        "manifest-sandbox-colour-yaml",
        "workflow.yaml",
        "tasks:\n  - id: test\n    command: [cargo, test]\n    sandbox:\n      colour: red\n",
    )
    .unwrap();
    let json_unknown = invalid_manifest_error(
        "manifest-sandbox-colour-json",
        "workflow.json",
        r#"{"tasks":[{"id":"test","command":["cargo","test"],"sandbox":{"colour":"red"}}]}"#,
    )
    .unwrap();
    let yaml_wrong_type = invalid_manifest_error(
        "manifest-sandbox-type-yaml",
        "workflow.yaml",
        "tasks:\n  - id: test\n    command: [cargo, test]\n    sandbox:\n      network:\n        localhost: [yes]\n",
    )
    .unwrap();
    let json_wrong_type = invalid_manifest_error(
        "manifest-sandbox-type-json",
        "workflow.json",
        r#"{"tasks":[{"id":"test","command":["cargo","test"],"sandbox":{"network":{"localhost":"maybe"}}}]}"#,
    )
    .unwrap();
    let json_wrong_shape = invalid_manifest_error(
        "manifest-sandbox-shape-json",
        "workflow.json",
        r#"{"tasks":[{"id":"test","command":["cargo","test"],"sandbox":"on"}]}"#,
    )
    .unwrap();

    assert_names_the_problem(&yaml_unknown, &["unknown field", "colour"]);
    assert_names_the_problem(&json_unknown, &["unknown field", "colour"]);
    assert_names_the_problem(&yaml_wrong_type, &["localhost", "expected bool"]);
    assert_names_the_problem(&json_wrong_type, &["invalid type", "maybe"]);
    assert_names_the_problem(&json_wrong_shape, &["true, false, or a sandbox table"]);
}

#[test]
fn reports_the_specific_mistake_inside_a_schedule_table() {
    let yaml_unknown = invalid_manifest_error(
        "manifest-schedule-colour-yaml",
        "workflow.yaml",
        "schedule:\n  cron: \"0 3 * * *\"\n  colour: red\ntasks:\n  - id: test\n    command: [cargo, test]\n",
    )
    .unwrap();
    let json_unknown = invalid_manifest_error(
        "manifest-schedule-colour-json",
        "workflow.json",
        r#"{"schedule":{"cron":"0 3 * * *","colour":"red"},"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
    )
    .unwrap();
    let yaml_overlap = invalid_manifest_error(
        "manifest-schedule-overlap-yaml",
        "workflow.yaml",
        "schedule:\n  cron: \"0 3 * * *\"\n  on_overlap: sometimes\ntasks:\n  - id: test\n    command: [cargo, test]\n",
    )
    .unwrap();
    let json_cron_type = invalid_manifest_error(
        "manifest-schedule-cron-type-json",
        "workflow.json",
        r#"{"schedule":{"cron":5},"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
    )
    .unwrap();
    let yaml_list_entry = invalid_manifest_error(
        "manifest-schedule-list-yaml",
        "workflow.yaml",
        "schedule:\n  - name: early\n    cron: \"0 3 * * *\"\n  - name: late\n    cron: \"0 5 * * *\"\n    on_overlap: sometimes\ntasks:\n  - id: test\n    command: [cargo, test]\n",
    )
    .unwrap();
    let json_list_entry = invalid_manifest_error(
        "manifest-schedule-list-json",
        "workflow.json",
        r#"{"schedule":[{"name":"early","cron":"0 3 * * *"},{"name":"late","cron":"0 5 * * *","colour":"red"}],"tasks":[{"id":"test","command":["cargo","test"]}]}"#,
    )
    .unwrap();
    let json_wrong_shape = invalid_manifest_error(
        "manifest-schedule-shape-json",
        "workflow.json",
        r#"{"schedule":"0 3 * * *","tasks":[{"id":"test","command":["cargo","test"]}]}"#,
    )
    .unwrap();

    assert_names_the_problem(&yaml_unknown, &["unknown field", "colour"]);
    assert_names_the_problem(&json_unknown, &["unknown field", "colour"]);
    assert_names_the_problem(&yaml_overlap, &["unknown variant", "sometimes"]);
    assert_names_the_problem(&json_cron_type, &["invalid type: integer `5`"]);
    assert_names_the_problem(&yaml_list_entry, &["unknown variant", "sometimes"]);
    assert_names_the_problem(&json_list_entry, &["unknown field", "colour"]);
    assert_names_the_problem(
        &json_wrong_shape,
        &["a schedule table or a list of schedule tables"],
    );
}
