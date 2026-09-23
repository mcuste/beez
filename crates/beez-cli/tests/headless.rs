//! Tests Beez's process execution contract.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::process::Output;

use beez_test_support::{TemporaryDirectory, extended_path, fake_harness};

mod common;

/// `beez-process` unit tests cover each harness's arguments; this pins the result relay.
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

    let output = common::beez()
        .args([
            "run",
            "claude",
            "--model",
            "opus",
            "--effort",
            "high",
            "inspect the repository",
        ])
        .env("PATH", extended_path(directory.path()))
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"[--print][--model][opus][--effort][high][inspect the repository]"
    );
}

#[test]
fn passes_the_effort_to_codex_as_a_config_override() {
    let directory = TemporaryDirectory::new("cli-codex-options").unwrap();
    fake_harness(&directory, "codex", 0).unwrap();

    let output = common::beez()
        .args([
            "run",
            "codex",
            "--model",
            "gpt-5",
            "--effort",
            "high",
            "inspect the repository",
        ])
        .env("PATH", extended_path(directory.path()))
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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

    let output = common::beez()
        .args(["run", "pi", "--print me"])
        .env("PATH", directory.path())
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
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
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    harness: codex\n    prompt: inspect the repository\n    model: gpt-5\n    effort: high\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow"])
        .arg(workflow)
        .env("PATH", extended_path(directory.path()))
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"[exec][--model][gpt-5][-c][model_reasoning_effort=high][inspect the repository]"
    );
}

#[test]
fn runs_a_workflow_harness_task_with_a_model_and_effort() {
    let directory = TemporaryDirectory::new("cli-workflow-harness-options").unwrap();
    fake_harness(&directory, "omp", 0).unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    harness: omp\n    prompt: inspect the repository\n    model: opus\n    effort: high\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow"])
        .arg(workflow)
        .env("PATH", extended_path(directory.path()))
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"[--print][--model][opus][--thinking][high][inspect the repository]"
    );
}

#[test]
fn passes_the_output_of_a_task_into_a_prompt_file() {
    let directory = TemporaryDirectory::new("cli-workflow-task-output").unwrap();
    let harness = fake_harness(&directory, "claude", 0).unwrap();
    // Echo to stdout only: grouped output merges streams and would interleave two copies.
    fs::write(&harness, "#!/bin/sh\nprintf '[%s]' \"$@\"\n").unwrap();
    directory
        .write(
            "review.md",
            "review these files:\n{{ tasks.inspect.stdout }}\n",
        )
        .unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    command: [printf, 'a.rs\\nb.rs\\n']\n  - id: review\n    depends_on: [inspect]\n    harness: claude\n    prompt_file: review.md\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow", "--output", "grouped", "--color", "never"])
        .arg(workflow)
        .env("PATH", extended_path(directory.path()))
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[--print][review these files:\n    a.rs\n    b.rs\n    ]"),
        "{stdout}"
    );
}

#[test]
fn rejects_an_unsupported_harness() {
    let output = common::beez()
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
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: inspect\n    harness: cursor\n    prompt: inspect the repository\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"unknown headless harness \"cursor\"; expected one of pi, omp, claude, codex\n"
    );
}

#[test]
fn reports_when_a_harness_cannot_start() {
    let directory = TemporaryDirectory::new("cli-missing-harness").unwrap();

    let output = common::beez()
        .args(["run", "pi", "inspect the repository"])
        .env("PATH", directory.path())
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn runs_a_command_with_literal_arguments() {
    let directory = TemporaryDirectory::new("cli-command").unwrap();
    let output = common::beez()
        .current_dir(directory.path())
        .args([
            "run",
            "command",
            "--no-log",
            "bash",
            "--noprofile",
            "-c",
            "printf 'stdout:%s:%s' \"$1\" \"$2\"\nprintf 'stderr:%s:%s' \"$1\" \"$2\" >&2\nexit 29",
            "beez",
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
    let workflow = directory
        .write("workflow.yaml", format!(
            "tasks:\n  - id: prepare\n    command: [bash, -c, 'printf ready > {state}']\n  - id: test\n    depends_on: [prepare]\n    command: [bash, -c, 'test \"$(cat {state})\" = ready && printf done']\n",
            state = state.display()
        ))
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow", "--color", "never"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, "test    \u{2502} done\n".as_bytes());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Summary   2 passed in"),
        "{output:?}"
    );
}

#[test]
fn prefixes_every_line_of_every_task_that_runs_together() {
    let directory = TemporaryDirectory::new("cli-concurrent-output").unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: first\n    command: [bash, -c, 'printf first-out; printf first-err >&2']\n  - id: second\n    command: [bash, -c, 'printf second-out; printf second-err >&2']\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow", "--color", "never"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    // Both tasks run in one batch, so Beez may relay them in either order.
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        [
            "first  \u{2502} first-out",
            "first  \u{250a} first-err",
            "second \u{2502} second-out",
            "second \u{250a} second-err",
        ]
    );
}

#[test]
fn returns_a_failed_workflow_status_and_blocks_dependents() {
    let directory = TemporaryDirectory::new("cli-failed-workflow").unwrap();
    let marker = directory.join("blocked");
    let workflow = directory
        .write("workflow.yaml", format!(
            "tasks:\n  - id: fail\n    command: [bash, -c, 'printf failed-out; printf failed-err >&2; exit 23']\n  - id: blocked\n    depends_on: [fail]\n    command: [bash, -c, 'touch {marker}']\n",
            marker = marker.display()
        ))
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow", "--color", "never"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(23));
    // A task's own two streams both reach standard output, each marked.
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        ["fail    \u{2502} failed-out", "fail    \u{250a} failed-err"]
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Failed    fail in"), "{stderr}");
    assert!(stderr.contains("Blocked   blocked"), "{stderr}");
    assert!(!marker.exists());
}

#[test]
fn runs_a_json_workflow() {
    let directory = TemporaryDirectory::new("cli-json-workflow").unwrap();
    let workflow = directory
        .write(
            "workflow.json",
            r#"{"tasks":[{"id":"test","command":["bash","-c","printf json"]}]}"#,
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"json");
    assert!(output.stderr.is_empty());
}

#[test]
fn reports_invalid_workflow_manifests() {
    let directory = TemporaryDirectory::new("cli-invalid-workflow").unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: test\n    depends_on: [prepare]\n    command: [cargo, test]\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
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
    let workflow = directory.write("workflow.yaml", "tasks: []\n").unwrap();

    let output = common::beez()
        .args(["run", "workflow"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"workflow must define at least one task\n");
}

#[test]
fn groups_each_task_behind_a_status_line() {
    let directory = TemporaryDirectory::new("cli-grouped-output").unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: first\n    command: [bash, -c, 'echo first-out; echo first-err >&2']\n  - id: second\n    depends_on: [first]\n    command: [bash, -c, 'echo second-out']\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow", "--output", "grouped"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    // One reader thread per stream, so a task's own two lines may swap.
    let mut lines = stdout.lines();
    let first: Vec<&str> = lines.by_ref().take(2).collect();
    assert!(first.contains(&"    first-out"), "stdout: {stdout}");
    assert!(first.contains(&"    first-err"), "stdout: {stdout}");
    assert_eq!(lines.next(), Some("    second-out"));
    assert_eq!(lines.next(), None);
    assert!(stderr.contains("Running   first"), "stderr: {stderr}");
    assert!(stderr.contains("Finished  first in"), "stderr: {stderr}");
    assert!(stderr.contains("Summary   2 passed in"), "stderr: {stderr}");
}

#[test]
fn prefixes_every_line_with_its_task_in_stream_mode() {
    let directory = TemporaryDirectory::new("cli-stream-output").unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: first\n    command: [bash, -c, 'echo one; echo two >&2']\n  - id: longer_id\n    depends_on: [first]\n    command: [bash, -c, 'echo three']\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow", "--output", "stream", "--color", "never"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The spinners are hidden without a terminal, and every line still arrives.
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut lines = stdout.lines();
    let first: Vec<&str> = lines.by_ref().take(2).collect();
    // The separator says which stream a line came from.
    assert!(
        first.contains(&"first     \u{2502} one"),
        "stdout: {stdout}"
    );
    assert!(
        first.contains(&"first     \u{250a} two"),
        "stdout: {stdout}"
    );
    assert_eq!(lines.next(), Some("longer_id \u{2502} three"));
    assert_eq!(lines.next(), None);
}

#[test]
fn reports_a_blocked_task_and_counts_it_in_the_summary() {
    let directory = TemporaryDirectory::new("cli-blocked-summary").unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: fail\n    command: [bash, -c, 'exit 23']\n  - id: later\n    depends_on: [fail]\n    command: [bash, -c, 'true']\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(23));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Failed    fail in"), "stderr: {stderr}");
    assert!(stderr.contains("(exit 23)"), "stderr: {stderr}");
    assert!(stderr.contains("Blocked   later"), "stderr: {stderr}");
    assert!(
        stderr.contains("Summary   0 passed, 1 failed, 1 blocked in"),
        "stderr: {stderr}"
    );
}

#[test]
fn relays_a_single_task_workflow_without_decoration() {
    let directory = TemporaryDirectory::new("cli-single-task").unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: only\n    command: [bash, -c, 'printf solo']\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"solo");
    assert!(output.stderr.is_empty());
}

#[test]
fn closes_each_grouped_task_with_its_status_line() {
    let directory = TemporaryDirectory::new("cli-grouped-order").unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: first\n    command: [bash, -c, 'echo first-out']\n  - id: second\n    depends_on: [first]\n    command: [bash, -c, 'echo second-out']\n",
        )
        .unwrap();
    let merged = directory.join("merged.log");
    let stdout = fs::File::create(&merged).unwrap();
    let stderr = stdout.try_clone().unwrap();

    // Both streams share one file description, so the file keeps the order
    // Beez wrote them, the way a terminal shows it.
    let status = common::beez()
        .args(["run", "workflow", "--output", "grouped"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .stdout(stdout)
        .stderr(stderr)
        .status()
        .unwrap();

    assert!(status.success());
    let log = fs::read_to_string(&merged).unwrap();
    let mut lines = log.lines();
    // The run names its own artifact directory before it starts.
    assert!(
        lines.next().is_some_and(|line| line.starts_with("Logging")),
        "log: {log}"
    );
    assert_eq!(lines.next(), Some("Running   first"));
    assert_eq!(lines.next(), Some("    first-out"));
    assert!(
        lines
            .next()
            .is_some_and(|line| line.starts_with("Finished  first in")),
        "log: {log}"
    );
    assert_eq!(lines.next(), Some("Running   second"));
    assert_eq!(lines.next(), Some("    second-out"));
    assert!(
        lines
            .next()
            .is_some_and(|line| line.starts_with("Finished  second in")),
        "log: {log}"
    );
    assert!(
        lines
            .next()
            .is_some_and(|line| line.starts_with("Summary   2 passed in")),
        "log: {log}"
    );
    assert_eq!(lines.next(), None);
}

#[test]
fn keeps_a_grouped_block_off_the_status_line() {
    let directory = TemporaryDirectory::new("cli-grouped-newline").unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: first\n    command: [bash, -c, \"echo one; echo; printf no-newline\"]\n  - id: second\n    depends_on: [first]\n    command: [bash, -c, 'true']\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow", "--output", "grouped"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // A blank line keeps no trailing spaces, and the last line still ends.
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "    one\n\n    no-newline\n"
    );
}

#[test]
fn stamps_a_grouped_line_when_it_arrives() {
    let directory = TemporaryDirectory::new("cli-timestamps").unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: first\n    command: [bash, -c, 'echo early; sleep 1; echo late']\n  - id: second\n    depends_on: [first]\n    command: [bash, -c, 'true']\n",
        )
        .unwrap();

    let output = common::beez()
        .args([
            "run",
            "workflow",
            "--output",
            "grouped",
            "--timestamps=elapsed",
            "--color",
            "never",
        ])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    // A block prints when its task ends, so a stamp must be the arrival time
    // of its own line, not the time the block was written.
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("    0.0s     early"));
    let late = lines.next().unwrap_or_default();
    let seconds: f64 = late
        .split_whitespace()
        .next()
        .and_then(|stamp| stamp.strip_suffix('s'))
        .and_then(|stamp| stamp.parse().ok())
        .unwrap_or_else(|| panic!("stdout: {stdout}"));
    assert!(seconds >= 1.0, "stdout: {stdout}");
    assert!(late.ends_with("     late"), "stdout: {stdout}");
    assert_eq!(lines.next(), None);
}

#[test]
fn stamps_every_line_with_a_utc_date_and_time() {
    let directory = TemporaryDirectory::new("cli-datetime").unwrap();
    let workflow = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: first\n    command: [bash, -c, 'echo one']\n  - id: second\n    depends_on: [first]\n    command: [bash, -c, 'echo two']\n",
        )
        .unwrap();

    let output = common::beez()
        .args(["run", "workflow", "--timestamps", "--color", "never"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    for line in stdout.lines() {
        let (stamp, rest) = line.split_at(24);
        assert!(
            stamp.len() == 24
                && stamp.ends_with('Z')
                && stamp.is_char_boundary(24)
                && stamp.chars().filter(|byte| *byte == '-').count() == 2
                && stamp.chars().filter(|byte| *byte == ':').count() == 2,
            "line: {line}"
        );
        assert!(
            rest.starts_with(" first") || rest.starts_with(" second"),
            "line: {line}"
        );
    }
}

fn run_harness(name: &str, arguments: &[&str]) -> Result<Output, Box<dyn Error>> {
    let directory = TemporaryDirectory::new(&format!("cli-{name}"))?;
    fake_harness(&directory, name, 17)?;

    common::beez()
        .args(["run", name])
        .args(arguments)
        .env("PATH", extended_path(directory.path()))
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()
        .map_err(Into::into)
}

#[test]
fn rejects_an_invalid_sandbox_domain_rule() {
    let output = common::beez()
        .args(["run", "command", "--allow-domain", "a.*.com", "true"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(std::str::from_utf8(&output.stderr).unwrap().contains("`*`"));
}

#[test]
fn hides_the_sandbox_helper_commands() {
    let output = common::beez().arg("--help").output().unwrap();

    let help = std::str::from_utf8(&output.stdout).unwrap();
    assert!(help.contains("run"));
    assert!(!help.contains("sandbox-init"));
}
