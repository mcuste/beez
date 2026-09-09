//! Process runner integration tests.
#![cfg(unix)]

use loom_process::{ExecutionRequest, OutputStream, ProcessCall, ProcessRunner};

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

#[test]
fn forwards_every_line_to_the_sink_while_it_runs() {
    let request = ExecutionRequest::Command(
        ProcessCall::new("bash")
            .argument("-c")
            .argument("echo one; echo two >&2; printf three"),
    );
    let lines = std::sync::Mutex::new(Vec::new());

    let output = ProcessRunner
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

fn stream_lines(lines: &[(OutputStream, Vec<u8>)], stream: OutputStream) -> Vec<Vec<u8>> {
    lines
        .iter()
        .filter(|(line_stream, _)| *line_stream == stream)
        .map(|(_, line)| line.clone())
        .collect()
}
