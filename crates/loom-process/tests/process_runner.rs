//! Process runner integration tests.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use loom_process::{ExecutionRequest, HarnessCall, HeadlessHarness, ProcessCall, ProcessRunner};

#[test]
fn reports_signal_termination() {
    let request = ExecutionRequest::Command(
        ProcessCall::new("bash")
            .argument("-c")
            .argument("kill -TERM $$"),
    );

    let output = ProcessRunner.run(request).unwrap();

    assert_eq!(output.status_code(), None);
    assert!(!output.succeeded());
}

#[test]
fn invokes_the_configured_harness_program() {
    let directory = temporary_directory("harness").unwrap();
    let program = directory.0.join("custom-pi");
    fs::write(
        &program,
        "#!/bin/sh\nprintf 'stdout:%s:%s' \"$1\" \"$2\"\nprintf 'stderr:%s:%s' \"$1\" \"$2\" >&2\nexit 17\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

    let request = ExecutionRequest::Harness(HarnessCall::new(
        HeadlessHarness::Pi,
        program,
        "inspect the repository",
    ));

    let output = ProcessRunner.run(request).unwrap();

    assert_eq!(output.status_code(), Some(17));
    assert_eq!(output.stdout(), b"stdout:--print:inspect the repository");
    assert_eq!(output.stderr(), b"stderr:--print:inspect the repository");
}

struct TemporaryDirectory(PathBuf);

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temporary_directory(name: &str) -> Result<TemporaryDirectory, Box<dyn std::error::Error>> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let directory = std::env::temp_dir().join(format!("loom-{name}-{}-{timestamp}", process::id()));

    fs::create_dir(&directory)?;

    Ok(TemporaryDirectory(directory))
}
