//! Tests how Beez colours its own status lines.

#![cfg(unix)]

use std::error::Error;

use beez_test_support::TemporaryDirectory;

mod common;

const RESET: &str = "\u{1b}[0m";
const BOLD: &str = "\u{1b}[1m";

const CYAN: u8 = 36;
const GREEN: u8 = 32;
const RED: u8 = 31;
const YELLOW: u8 = 33;

/// One task passes, one fails, and one is blocked behind the failure.
const WORKFLOW: &str = "tasks:
  - id: ok
    command: [bash, -c, 'exit 0']
  - id: fail
    command: [bash, -c, 'exit 3']
  - id: blocked
    depends_on: [fail]
    command: [bash, -c, 'exit 0']
";

/// One status line as `--color always` writes it: a reset, then the status
/// word in bold and its colour, padded to its column.
fn status_line(color: u8, verb: &str, message: &str) -> String {
    format!("{RESET}{BOLD}\u{1b}[{color}m{verb:<8}{RESET}  {message}")
}

/// Runs [`WORKFLOW`] in one output mode, and returns its exit status and
/// what Beez wrote to standard error.
fn run_coloured(name: &str, output: &str) -> Result<(Option<i32>, String), Box<dyn Error>> {
    let directory = TemporaryDirectory::new(name)?;
    let workflow = directory.write("workflow.yaml", WORKFLOW)?;

    let result = common::beez()
        .args(["run", "workflow", "--output", output, "--color", "always"])
        .arg(workflow)
        .current_dir(directory.path())
        .env("BEEZ_LOG_DIR", directory.join("logs"))
        .output()?;

    Ok((result.status.code(), String::from_utf8(result.stderr)?))
}

fn assert_line(stderr: &str, color: u8, verb: &str, message: &str) {
    let expected = status_line(color, verb, message);
    assert!(
        stderr.lines().any(|line| line.starts_with(&expected)),
        "no {expected:?} in {stderr:?}"
    );
}

/// The lines every decorated layout prints, whatever it does with a running task.
fn assert_closing_lines(stderr: &str) {
    // Logging names the run directory before the tasks start.
    assert!(
        stderr
            .lines()
            .next()
            .is_some_and(|line| line.starts_with(&status_line(CYAN, "Logging", ""))),
        "{stderr:?}"
    );
    assert_line(stderr, GREEN, "Finished", "ok in ");
    assert_line(stderr, RED, "Failed", "fail in ");
    assert_line(stderr, YELLOW, "Blocked", "blocked");
    assert_line(stderr, RED, "Summary", "1 passed, 1 failed, 1 blocked in ");
}

#[test]
fn colours_each_status_word_of_a_grouped_run_by_the_state_it_names() {
    let (status, stderr) = run_coloured("cli-color-grouped", "grouped").unwrap();

    assert_eq!(status, Some(3));
    assert_closing_lines(&stderr);
    assert_line(&stderr, CYAN, "Running", "ok");
    assert_line(&stderr, CYAN, "Running", "fail");
}

#[test]
fn colours_each_status_word_of_a_streamed_run_by_the_state_it_names() {
    let (status, stderr) = run_coloured("cli-color-stream", "stream").unwrap();

    assert_eq!(status, Some(3));
    assert_closing_lines(&stderr);
    // A streamed run shows a running task as a spinner row instead.
    assert!(!stderr.contains("Running"), "{stderr:?}");
}
