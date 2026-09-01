//! Process runner integration tests.
#![cfg(unix)]

use loom_process::{ExecutionRequest, ProcessCall, ProcessRunner};

#[test]
fn runs_a_process_call_and_captures_its_streams() {
    let request = ExecutionRequest::Command(
        ProcessCall::new("bash")
            .argument("-c")
            .argument("printf loom"),
    );

    let output = ProcessRunner.run(request).unwrap();

    assert!(output.succeeded());
    assert_eq!(output.stdout(), b"loom");
    assert!(output.stderr().is_empty());
}

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
