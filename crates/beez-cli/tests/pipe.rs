//! Tests that Beez ends cleanly when a reader closes its output early, as in
//! `beez schedule list | head -0`.

#![cfg(unix)]

use std::io;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use beez_test_support::TemporaryDirectory;

pub mod common;

/// A writer whose reader is gone before Beez starts, so its first write fails.
fn closed() -> io::Result<io::PipeWriter> {
    let (reader, writer) = io::pipe()?;
    drop(reader);
    Ok(writer)
}

/// Runs `command` with standard output closed, and keeps standard error.
fn without_stdout(mut command: Command) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(closed()?)
        .stderr(Stdio::piped())
        .output()
}

/// Runs `command` with standard error closed, and keeps standard output.
fn without_stderr(mut command: Command) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(closed()?)
        .output()
}

fn schedule(root: &Path, arguments: &[&str]) -> Command {
    common::beez_at_root(root, &[&["schedule"], arguments].concat())
}

/// The process ended on its own terms, not by a panic.
fn assert_no_panic(output: &Output) {
    assert_ne!(output.status.code(), Some(101), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn prints_the_version_to_a_closed_reader_without_a_panic() {
    let mut command = common::beez();
    command.arg("--version");

    let output = without_stdout(command).unwrap();

    assert_no_panic(&output);
}

#[test]
fn ends_a_listing_quietly_when_the_reader_closes() {
    let directory = TemporaryDirectory::new("pipe-list-empty").unwrap();
    let root = directory.join("root");

    let output = without_stdout(schedule(&root, &["list"])).unwrap();

    assert_no_panic(&output);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn ends_a_job_table_quietly_when_the_reader_closes() {
    let directory = TemporaryDirectory::new("pipe-list-table").unwrap();
    let root = directory.join("root");
    let manifest = directory
        .write(
            "nightly.yaml",
            "schedule:\n  cron: \"0 3 * * *\"\n  allow_unsandboxed: true\ntasks:\n  - id: mark\n    sandbox: false\n    command: [bash, -c, 'true']\n",
        )
        .unwrap();
    let added = schedule(&root, &["add"]).arg(&manifest).output().unwrap();
    assert!(added.status.success(), "{added:?}");

    let output = without_stdout(schedule(&root, &["list"])).unwrap();

    assert_no_panic(&output);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn keeps_the_status_of_an_answer_the_reader_did_not_wait_for() {
    let directory = TemporaryDirectory::new("pipe-answer").unwrap();
    let root = directory.join("root");
    let manifest = directory
        .write(
            "nightly.yaml",
            "schedule:\n  cron: \"0 3 * * *\"\n  allow_unsandboxed: true\ntasks:\n  - id: mark\n    sandbox: false\n    command: [bash, -c, 'true']\n",
        )
        .unwrap();

    let mut command = schedule(&root, &["add"]);
    command.arg(&manifest);

    let output = without_stdout(command).unwrap();

    assert_no_panic(&output);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
}

#[test]
fn keeps_the_status_of_a_refusal_when_standard_error_is_closed() {
    let directory = TemporaryDirectory::new("pipe-refusal").unwrap();
    let root = directory.join("root");

    let output = without_stderr(schedule(&root, &["pause", "missing"])).unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
}

#[test]
fn keeps_the_status_of_an_error_when_standard_error_is_closed() {
    let directory = TemporaryDirectory::new("pipe-error").unwrap();
    let mut command = common::beez();
    command
        .args(["run", "workflow"])
        .arg(directory.join("missing.yaml"));

    let output = without_stderr(command).unwrap();

    assert_eq!(output.status.code(), Some(2), "{output:?}");
}
