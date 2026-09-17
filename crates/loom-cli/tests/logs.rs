//! Tests the artifacts Loom writes for every run.

#![cfg(unix)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Output;

use loom_test_support::TemporaryDirectory;

mod common;

/// Runs `manifest` with `root` as the artifact directory.
fn run_workflow(manifest: &Path, root: &Path, arguments: &[&str]) -> io::Result<Output> {
    common::loom()
        .args(["run", "workflow", "--color", "never"])
        .args(arguments)
        .arg(manifest)
        .env("LOOM_LOG_DIR", root)
        .output()
}

/// The one run directory under `root`.
fn only_run(root: &Path) -> io::Result<PathBuf> {
    let mut runs = Vec::new();
    for entry in fs::read_dir(root.join("runs"))? {
        runs.push(entry?.path());
    }
    if runs.len() != 1 {
        return Err(io::Error::other(format!(
            "the root holds {} runs, not one: {runs:?}",
            runs.len()
        )));
    }

    runs.pop()
        .ok_or_else(|| io::Error::other("the run wrote no directory"))
}

#[test]
fn keeps_the_whole_run_in_one_log_and_the_streams_of_each_task_apart() {
    let directory = TemporaryDirectory::new("logs-run").unwrap();
    let root = directory.join("artifacts");
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: first\n    command: [bash, -c, 'echo first-out; echo first-err >&2']\n  - id: second\n    depends_on: [first]\n    command: [bash, -c, 'echo second-out']\n",
        )
        .unwrap();

    let output = run_workflow(&manifest, &root, &[]).unwrap();

    assert!(output.status.success(), "{output:?}");
    let run = only_run(&root).unwrap();
    let log = fs::read_to_string(run.join("run.log")).unwrap();
    // Every line carries the time it arrived and the task it belongs to.
    for line in log.lines() {
        let (stamp, rest) = line.split_at(24);
        assert!(stamp.ends_with('Z'), "line: {line}");
        assert!(!rest.is_empty(), "line: {line}");
    }
    assert!(log.contains("first  \u{2502} first-out"), "log: {log}");
    assert!(log.contains("first  \u{250a} first-err"), "log: {log}");
    assert!(log.contains("second \u{2502} second-out"), "log: {log}");
    assert!(log.contains("Running   first"), "log: {log}");
    assert!(log.contains("Finished  second in"), "log: {log}");
    assert!(log.contains("Summary   2 passed in"), "log: {log}");
    // A stream file keeps the bytes of one stream of one task, and nothing else.
    assert_eq!(
        fs::read_to_string(run.join("tasks/first.stdout")).unwrap(),
        "first-out\n"
    );
    assert_eq!(
        fs::read_to_string(run.join("tasks/first.stderr")).unwrap(),
        "first-err\n"
    );
    assert_eq!(
        fs::read_to_string(run.join("tasks/second.stdout")).unwrap(),
        "second-out\n"
    );
    assert!(
        fs::read_to_string(run.join("tasks/second.stderr"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn records_the_outcome_of_every_task_of_a_failed_run() {
    let directory = TemporaryDirectory::new("logs-record").unwrap();
    let root = directory.join("artifacts");
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: works\n    command: [bash, -c, 'true']\n  - id: fails\n    command: [bash, -c, 'exit 23']\n  - id: later\n    depends_on: [fails]\n    harness: claude\n    prompt: review\n    model: opus\n    effort: high\n",
        )
        .unwrap();

    let output = run_workflow(&manifest, &root, &[]).unwrap();

    assert_eq!(output.status.code(), Some(23), "{output:?}");
    let record = fs::read_to_string(only_run(&root).unwrap().join("run.json")).unwrap();
    assert!(record.contains("\"schema\": 1"), "{record}");
    assert!(record.contains("\"exit_status\": 23"), "{record}");
    assert!(
        record.contains(&format!("\"manifest\": \"{}\"", manifest.display())),
        "{record}"
    );
    assert!(record.contains("\"state\": \"finished\""), "{record}");
    assert!(record.contains("\"state\": \"blocked\""), "{record}");
    // A task record names the request that ran.
    assert!(record.contains("\"kind\": \"harness\""), "{record}");
    assert!(record.contains("\"prompt\": \"review\""), "{record}");
    assert!(record.contains("\"model\": \"opus\""), "{record}");
    assert!(
        record.contains("\"depends_on\": [\n        \"fails\"\n      ]"),
        "{record}"
    );
    assert!(
        record.contains("\"stdout\": \"tasks/works.stdout\""),
        "{record}"
    );
}

#[test]
fn keeps_the_colours_of_a_task_out_of_the_run_log() {
    let directory = TemporaryDirectory::new("logs-colour").unwrap();
    let root = directory.join("artifacts");
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: first\n    command: [bash, -c, 'printf \"\\033[31mred\\033[0m\\n\"']\n  - id: second\n    command: [bash, -c, 'true']\n",
        )
        .unwrap();

    let output = run_workflow(&manifest, &root, &[]).unwrap();

    assert!(output.status.success(), "{output:?}");
    let run = only_run(&root).unwrap();
    let log = fs::read_to_string(run.join("run.log")).unwrap();
    assert!(log.contains("first  \u{2502} red"), "log: {log}");
    assert!(!log.contains('\u{1b}'), "log: {log}");
    // The stream file keeps the bytes as the task wrote them.
    assert_eq!(
        fs::read(run.join("tasks/first.stdout")).unwrap(),
        b"\x1b[31mred\x1b[0m\n"
    );
}

#[test]
fn points_the_latest_link_at_the_newest_run() {
    let directory = TemporaryDirectory::new("logs-latest").unwrap();
    let root = directory.join("artifacts");
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: only\n    command: [bash, -c, 'printf solo']\n",
        )
        .unwrap();

    assert!(
        run_workflow(&manifest, &root, &[])
            .unwrap()
            .status
            .success()
    );
    let first = fs::read_link(root.join("latest")).unwrap();
    assert!(
        run_workflow(&manifest, &root, &[])
            .unwrap()
            .status
            .success()
    );
    let second = fs::read_link(root.join("latest")).unwrap();

    assert_ne!(first, second);
    assert!(second.starts_with("runs"), "{second:?}");
    assert_eq!(
        fs::read_to_string(root.join("latest/tasks/only.stdout")).unwrap(),
        "solo"
    );
}

/// A plain run relays the bytes of its only task, so Loom adds no line of its
/// own, and the artifacts still hold the whole run.
#[test]
fn keeps_a_single_task_run_free_of_loom_lines_and_still_records_it() {
    let directory = TemporaryDirectory::new("logs-single").unwrap();
    let root = directory.join("artifacts");
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: only\n    command: [bash, -c, 'echo out; echo err >&2']\n",
        )
        .unwrap();

    let output = run_workflow(&manifest, &root, &[]).unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"out\n");
    assert_eq!(output.stderr, b"err\n");
    let run = only_run(&root).unwrap();
    let log = fs::read_to_string(run.join("run.log")).unwrap();
    assert!(log.contains("only \u{2502} out"), "log: {log}");
    assert!(log.contains("only \u{250a} err"), "log: {log}");
    assert!(log.contains("Finished  only in"), "log: {log}");
}

#[test]
fn writes_nothing_when_the_run_asks_for_no_artifacts() {
    let directory = TemporaryDirectory::new("logs-none").unwrap();
    let root = directory.join("artifacts");
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: first\n    command: [bash, -c, 'echo one']\n  - id: second\n    command: [bash, -c, 'echo two']\n",
        )
        .unwrap();

    let output = run_workflow(&manifest, &root, &["--no-log"]).unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(!root.exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("Logging"), "{stderr}");
}

#[test]
fn keeps_the_runs_of_a_repository_together() {
    let directory = TemporaryDirectory::new("logs-repository").unwrap();
    fs::create_dir(directory.join(".git")).unwrap();
    let deep = directory.join("crates/loom-cli");
    fs::create_dir_all(&deep).unwrap();
    let manifest = directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: only\n    command: [bash, -c, 'true']\n",
        )
        .unwrap();

    let output = common::loom()
        .args(["run", "workflow"])
        .arg(&manifest)
        .current_dir(&deep)
        .env_remove("LOOM_LOG_DIR")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let root = directory.join(".loom");
    assert!(only_run(&root).unwrap().join("run.json").exists());
    // The directory ignores itself, so no repository needs a change.
    assert_eq!(fs::read_to_string(root.join(".gitignore")).unwrap(), "*\n");
    assert!(!deep.join(".loom").exists());
}

#[test]
fn names_the_manifest_by_its_absolute_path_in_the_record() {
    let directory = TemporaryDirectory::new("logs-manifest-path").unwrap();
    let root = directory.join("artifacts");
    directory
        .write(
            "workflow.yaml",
            "tasks:\n  - id: only\n    command: [bash, -c, 'true']\n",
        )
        .unwrap();

    let output = common::loom()
        .args(["run", "workflow", "workflow.yaml"])
        .current_dir(directory.path())
        .env("LOOM_LOG_DIR", &root)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let record = fs::read_to_string(only_run(&root).unwrap().join("run.json")).unwrap();
    let manifest = record
        .split("\"manifest\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .map(Path::new)
        .unwrap();
    assert!(manifest.is_absolute(), "{record}");
    assert!(manifest.ends_with("workflow.yaml"), "{record}");
    assert!(manifest.is_file(), "{record}");
}

#[test]
fn runs_a_harness_prompt_and_records_it() {
    let directory = TemporaryDirectory::new("logs-harness").unwrap();
    let root = directory.join("artifacts");
    loom_test_support::fake_harness(&directory, "pi", 0).unwrap();

    let output = common::loom()
        .args(["run", "pi", "inspect the repository"])
        .env("PATH", loom_test_support::extended_path(directory.path()))
        .env("LOOM_LOG_DIR", &root)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let run = only_run(&root).unwrap();
    assert_eq!(
        fs::read_to_string(run.join("tasks/pi.stdout")).unwrap(),
        "[--print][inspect the repository]"
    );
    let record = fs::read_to_string(run.join("run.json")).unwrap();
    assert!(record.contains("\"inspect the repository\""), "{record}");
    assert!(record.contains("\"id\": \"pi\""), "{record}");
}
