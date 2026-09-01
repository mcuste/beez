//! Tests Loom's process execution contract.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::process::{Command, Output};

use loom_test_support::{TemporaryDirectory, fake_harness};

/// `loom-process` unit tests cover each harness's arguments; this pins the result relay.
#[test]
fn relays_the_status_and_streams_of_a_harness() {
    let output = run_harness("pi", &["inspect the repository"])
        .unwrap_or_else(|error| panic!("failed to run Pi harness: {error}"));

    assert_eq!(output.status.code(), Some(17));
    assert_eq!(output.stdout, b"[--print][inspect the repository]");
    assert_eq!(output.stderr, b"[--print][inspect the repository]");
}

#[test]
fn passes_the_model_and_effort_to_claude() {
    let directory = TemporaryDirectory::new("cli-claude-options").unwrap();
    fake_harness(&directory, "claude", 0).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args([
            "run",
            "claude",
            "--model",
            "opus",
            "--effort",
            "high",
            "inspect the repository",
        ])
        .env("PATH", directory.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"[--print][--model][opus][--effort][high][inspect the repository]"
    );
}

#[test]
fn passes_the_effort_to_codex_as_a_config_override() {
    let directory = TemporaryDirectory::new("cli-codex-options").unwrap();
    fake_harness(&directory, "codex", 0).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args([
            "run",
            "codex",
            "--model",
            "gpt-5",
            "--effort",
            "high",
            "inspect the repository",
        ])
        .env("PATH", directory.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"[exec][--model][gpt-5][-c][model_reasoning_effort=high][inspect the repository]"
    );
}

#[test]
fn passes_a_hyphenated_prompt_to_every_harness() {
    for (name, headless_argument) in [
        ("pi", "--print"),
        ("omp", "--print"),
        ("claude", "--print"),
        ("codex", "exec"),
    ] {
        let output = run_harness(name, &["--", "--print me"])
            .unwrap_or_else(|error| panic!("failed to run {name}: {error}"));
        let expected = format!("[{headless_argument}][--][--print me]");

        assert_eq!(output.status.code(), Some(17));
        assert_eq!(output.stdout, expected.as_bytes());
        assert_eq!(output.stderr, expected.as_bytes());
    }
}

#[test]
fn rejects_a_prompt_that_starts_with_a_hyphen_without_a_separator() {
    let directory = TemporaryDirectory::new("cli-unquoted-hyphen").unwrap();
    fake_harness(&directory, "pi", 0).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "pi", "--print me"])
        .env("PATH", directory.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        std::str::from_utf8(&output.stderr)
            .unwrap()
            .contains("unexpected argument '--print me' found")
    );
}

#[test]
fn runs_a_workflow_codex_task_with_a_model_and_effort() {
    let directory = TemporaryDirectory::new("cli-workflow-codex-options").unwrap();
    fake_harness(&directory, "codex", 0).unwrap();
    let workflow = directory.join("workflow.yaml");
    fs::write(
        &workflow,
        "tasks:\n  - id: inspect\n    harness: codex\n    prompt: inspect the repository\n    model: gpt-5\n    effort: high\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "workflow"])
        .arg(workflow)
        .env("PATH", directory.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"[exec][--model][gpt-5][-c][model_reasoning_effort=high][inspect the repository]"
    );
}

#[test]
fn runs_a_workflow_harness_task_with_a_model_and_effort() {
    let directory = TemporaryDirectory::new("cli-workflow-harness-options").unwrap();
    fake_harness(&directory, "omp", 0).unwrap();
    let workflow = directory.join("workflow.yaml");
    fs::write(
        &workflow,
        "tasks:\n  - id: inspect\n    harness: omp\n    prompt: inspect the repository\n    model: opus\n    effort: high\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "workflow"])
        .arg(workflow)
        .env("PATH", directory.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"[--print][--model][opus][--thinking][high][inspect the repository]"
    );
}

#[test]
fn rejects_an_unsupported_harness() {
    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "cursor", "inspect the repository"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        std::str::from_utf8(&output.stderr)
            .unwrap()
            .contains("unrecognized subcommand 'cursor'")
    );
}

#[test]
fn rejects_an_unsupported_workflow_harness() {
    let directory = TemporaryDirectory::new("cli-unsupported-workflow-harness").unwrap();
    let workflow = directory.join("workflow.yaml");
    fs::write(
        &workflow,
        "tasks:\n  - id: inspect\n    harness: cursor\n    prompt: inspect the repository\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "workflow"])
        .arg(workflow)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"unsupported headless harness \"cursor\"; expected one of pi, omp, claude, codex\n"
    );
}

#[test]
fn reports_when_a_harness_cannot_start() {
    let directory = TemporaryDirectory::new("cli-missing-harness").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "pi", "inspect the repository"])
        .env("PATH", directory.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn runs_a_command_with_literal_arguments() {
    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args([
            "run",
            "command",
            "bash",
            "--noprofile",
            "-c",
            "printf 'stdout:%s:%s' \"$1\" \"$2\"\nprintf 'stderr:%s:%s' \"$1\" \"$2\" >&2\nexit 29",
            "loom",
            "argument with spaces",
            "$(printf injected);*",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(29));
    assert_eq!(
        output.stdout,
        b"stdout:argument with spaces:$(printf injected);*"
    );
    assert_eq!(
        output.stderr,
        b"stderr:argument with spaces:$(printf injected);*"
    );
}

#[test]
fn runs_a_yaml_workflow_in_dependency_order() {
    let directory = TemporaryDirectory::new("cli-yaml-workflow").unwrap();
    let state = directory.join("state");
    let workflow = directory.join("workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "tasks:\n  - id: prepare\n    command: [bash, -c, 'printf ready > {state}']\n  - id: test\n    depends_on: [prepare]\n    command: [bash, -c, 'test \"$(cat {state})\" = ready && printf done']\n",
            state = state.display()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "workflow"])
        .arg(workflow)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout, b"done");
    assert!(output.stderr.is_empty());
}

#[test]
fn relays_the_whole_output_of_every_task_that_runs_together() {
    let directory = TemporaryDirectory::new("cli-concurrent-output").unwrap();
    let workflow = directory.join("workflow.yaml");
    fs::write(
        &workflow,
        "tasks:\n  - id: first\n    command: [bash, -c, 'printf first-out; printf first-err >&2']\n  - id: second\n    command: [bash, -c, 'printf second-out; printf second-err >&2']\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "workflow"])
        .arg(workflow)
        .output()
        .unwrap();

    // Both tasks run in one batch, so Loom may relay them in either order.
    assert!(output.status.success());
    assert_eq!(output.stdout.len(), b"first-outsecond-out".len());
    assert_eq!(output.stderr.len(), b"first-errsecond-err".len());
    for expected in [&b"first-out"[..], &b"second-out"[..]] {
        assert!(
            output
                .stdout
                .windows(expected.len())
                .any(|window| window == expected),
            "stdout {:?} lost {:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(expected)
        );
    }
    for expected in [&b"first-err"[..], &b"second-err"[..]] {
        assert!(
            output
                .stderr
                .windows(expected.len())
                .any(|window| window == expected),
            "stderr {:?} lost {:?}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(expected)
        );
    }
}

#[test]
fn returns_a_failed_workflow_status_and_blocks_dependents() {
    let directory = TemporaryDirectory::new("cli-failed-workflow").unwrap();
    let marker = directory.join("blocked");
    let workflow = directory.join("workflow.yaml");
    fs::write(
        &workflow,
        format!(
            "tasks:\n  - id: fail\n    command: [bash, -c, 'printf failed-out; printf failed-err >&2; exit 23']\n  - id: blocked\n    depends_on: [fail]\n    command: [bash, -c, 'touch {marker}']\n",
            marker = marker.display()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "workflow"])
        .arg(workflow)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(23));
    assert_eq!(output.stdout, b"failed-out");
    assert_eq!(output.stderr, b"failed-err");
    assert!(!marker.exists());
}

#[test]
fn runs_a_json_workflow() {
    let directory = TemporaryDirectory::new("cli-json-workflow").unwrap();
    let workflow = directory.join("workflow.json");
    fs::write(
        &workflow,
        r#"{"tasks":[{"id":"test","command":["bash","-c","printf json"]}]}"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "workflow"])
        .arg(workflow)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout, b"json");
    assert!(output.stderr.is_empty());
}

#[test]
fn reports_invalid_workflow_manifests() {
    let directory = TemporaryDirectory::new("cli-invalid-workflow").unwrap();
    let workflow = directory.join("workflow.yaml");
    fs::write(
        &workflow,
        "tasks:\n  - id: test\n    depends_on: [prepare]\n    command: [cargo, test]\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "workflow"])
        .arg(workflow)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"task test depends on unknown task prepare\n"
    );
}

#[test]
fn rejects_a_workflow_without_tasks() {
    let directory = TemporaryDirectory::new("cli-empty-workflow").unwrap();
    let workflow = directory.join("workflow.yaml");
    fs::write(&workflow, "tasks: []\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "workflow"])
        .arg(workflow)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"workflow must define at least one task\n");
}

fn run_harness(name: &str, arguments: &[&str]) -> Result<Output, Box<dyn Error>> {
    let directory = TemporaryDirectory::new(&format!("cli-{name}"))?;
    fake_harness(&directory, name, 17)?;

    Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", name])
        .args(arguments)
        .env("PATH", directory.path())
        .output()
        .map_err(Into::into)
}
