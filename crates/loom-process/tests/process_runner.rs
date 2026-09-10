//! Process runner integration tests.
#![cfg(unix)]

use loom_process::{ExecutionRequest, OutputStream, ProcessCall, ProcessRunner};
use loom_test_support::TemporaryDirectory;

#[test]
fn runs_a_process_call_and_captures_its_streams() {
    let request = ExecutionRequest::Command(
        ProcessCall::new("bash")
            .argument("-c")
            .argument("printf loom"),
    );

    let output = ProcessRunner::here().unwrap().run(request).unwrap();

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

    let output = ProcessRunner::here().unwrap().run(request).unwrap();

    assert_eq!(output.status_code(), None);
    assert!(!output.succeeded());
}

#[test]
fn forwards_every_line_to_the_sink_while_it_runs() {
    let request = ExecutionRequest::Command(
        ProcessCall::new("bash")
            .argument("-c")
            .argument("echo one; echo two >&2; printf three"),
    );
    let lines = std::sync::Mutex::new(Vec::new());

    let output = ProcessRunner::here()
        .unwrap()
        .run_streaming(request, None, &|stream, line| {
            if let Ok(mut lines) = lines.lock() {
                lines.push((stream, line.to_vec()));
            }
        })
        .unwrap();

    // Two reader threads race, so only the order inside one stream is fixed.
    let lines = lines.into_inner().unwrap();
    assert_eq!(
        stream_lines(&lines, OutputStream::Stdout),
        [b"one\n".to_vec(), b"three".to_vec()]
    );
    assert_eq!(
        stream_lines(&lines, OutputStream::Stderr),
        [b"two\n".to_vec()]
    );
    assert_eq!(output.stdout(), b"one\nthree");
    assert_eq!(output.stderr(), b"two\n");
}

#[test]
fn runs_a_process_call_in_the_given_working_directory() {
    let directory = TemporaryDirectory::new("process-working-directory").unwrap();
    let request =
        ExecutionRequest::Command(ProcessCall::new("bash").argument("-c").argument("pwd"));

    let output = ProcessRunner::new(directory.path()).run(request).unwrap();

    let printed = String::from_utf8_lossy(output.stdout())
        .trim_end()
        .to_owned();
    // The temporary directory can sit behind a symbolic link, so compare the real paths.
    assert_eq!(
        std::fs::canonicalize(printed).unwrap(),
        std::fs::canonicalize(directory.path()).unwrap()
    );
}

fn stream_lines(lines: &[(OutputStream, Vec<u8>)], stream: OutputStream) -> Vec<Vec<u8>> {
    lines
        .iter()
        .filter(|(line_stream, _)| *line_stream == stream)
        .map(|(_, line)| line.clone())
        .collect()
}
