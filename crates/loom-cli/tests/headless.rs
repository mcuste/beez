//! Tests Loom's process execution contract.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{self, Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn runs_pi_headlessly_and_relays_its_result() {
    let output =
        run_harness("pi").unwrap_or_else(|error| panic!("failed to run Pi harness: {error}"));

    assert_eq!(output.status.code(), Some(17));
    assert_eq!(output.stdout, b"stdout:--print:inspect the repository");
    assert_eq!(output.stderr, b"stderr:--print:inspect the repository");
}

#[test]
fn runs_omp_headlessly_and_relays_its_result() {
    let output =
        run_harness("omp").unwrap_or_else(|error| panic!("failed to run OMP harness: {error}"));

    assert_eq!(output.status.code(), Some(17));
    assert_eq!(output.stdout, b"stdout:--print:inspect the repository");
    assert_eq!(output.stderr, b"stderr:--print:inspect the repository");
}

#[test]
fn rejects_an_unsupported_harness() {
    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "codex", "inspect the repository"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        std::str::from_utf8(&output.stderr)
            .unwrap()
            .contains("unrecognized subcommand 'codex'")
    );
}

#[test]
fn reports_when_a_harness_cannot_start() {
    let directory = temporary_directory("missing-harness").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", "pi", "inspect the repository"])
        .env("PATH", &directory.0)
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

fn run_harness(name: &str) -> Result<Output, Box<dyn Error>> {
    let directory = temporary_directory(name)?;
    let program = directory.0.join(name);
    fs::write(
        &program,
        "#!/bin/sh\nprintf 'stdout:%s:%s' \"$1\" \"$2\"\nprintf 'stderr:%s:%s' \"$1\" \"$2\" >&2\nexit 17\n",
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;

    Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["run", name, "inspect the repository"])
        .env("PATH", &directory.0)
        .output()
        .map_err(Into::into)
}

struct TemporaryDirectory(PathBuf);

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temporary_directory(name: &str) -> Result<TemporaryDirectory, Box<dyn Error>> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let directory = std::env::temp_dir().join(format!("loom-{name}-{}-{timestamp}", process::id()));

    fs::create_dir(&directory)?;
    Ok(TemporaryDirectory(directory))
}
