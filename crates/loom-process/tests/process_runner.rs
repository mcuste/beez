//! Process runner integration tests.
#![cfg(unix)]

use loom_process::{ExecutionRequest, HarnessCall, HeadlessHarness, ProcessCall, ProcessRunner};
use loom_test_support::{TemporaryDirectory, fake_harness};

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
fn sends_every_harness_argument_to_the_configured_program() {
    let directory = TemporaryDirectory::new("process-harness").unwrap();
    let program = fake_harness(&directory, "custom-pi", 17).unwrap();
    let request = ExecutionRequest::Harness(
        HarnessCall::new(HeadlessHarness::Pi, program, "inspect the repository")
            .model("opus")
            .effort("high"),
    );

    let output = ProcessRunner.run(request).unwrap();

    assert_eq!(output.status_code(), Some(17));
    assert_eq!(
        output.stdout(),
        b"[--print][--model][opus][--effort][high][inspect the repository]"
    );
    assert_eq!(output.stderr(), output.stdout());
}
