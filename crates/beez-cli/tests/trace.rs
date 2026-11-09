//! Tests that `BEEZ_TRACE` turns on diagnostic tracing on standard error,
//! and that Beez prints no trace line without it.

use std::io;
use std::process::Output;

use beez_test_support::TemporaryDirectory;

pub mod common;

/// Runs `echo hi` with no sandbox and no artifacts, with `BEEZ_TRACE` set to
/// `trace` when it is given.
fn echo(trace: Option<&str>) -> io::Result<Output> {
    let directory = TemporaryDirectory::new("trace")?;
    let mut command = common::beez_in(&directory);
    command.args([
        "run",
        "command",
        "--no-sandbox",
        "--no-log",
        "--",
        "echo",
        "hi",
    ]);
    if let Some(trace) = trace {
        command.env("BEEZ_TRACE", trace);
    }
    command.output()
}

/// True when `line` looks like a line of the trace formatter.
fn is_trace_line(line: &str) -> bool {
    [" TRACE ", " DEBUG ", " INFO ", " WARN ", " ERROR "]
        .iter()
        .any(|level| line.contains(level))
}

#[test]
fn prints_no_trace_line_without_beez_trace() {
    let output = echo(None).unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"hi\n");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn traces_on_stderr_and_keeps_stdout_with_beez_trace() {
    let output = echo(Some("debug")).unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"hi\n");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.lines().any(is_trace_line), "{stderr}");
    assert!(stderr.contains("spawned process"), "{stderr}");
}

#[test]
fn warns_about_a_filter_it_cannot_read_and_runs_anyway() {
    let output = echo(Some("beez=loud")).unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"hi\n");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("warning: ignoring BEEZ_TRACE=\"beez=loud\""),
        "{stderr}"
    );
    assert!(!stderr.lines().any(is_trace_line), "{stderr}");
}
