//! Runner integration tests.
#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use beez_core::{TaskDefinition, TaskIndex, TaskRequest, Workflow};
use beez_process::{ExecutionRequest, OutputStream, ProcessCall};
use beez_runner::{RunError, RunEvent, Runner};
use beez_test_support::{TemporaryDirectory, text};

#[derive(Debug, Eq, PartialEq)]
enum RecordedEvent {
    Started {
        task: Option<usize>,
    },
    Output {
        task: Option<usize>,
        stream: OutputStream,
        line: Vec<u8>,
    },
    Finished {
        task: Option<usize>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        status: Option<i32>,
    },
    Failed {
        task: Option<usize>,
        error_kind: io::ErrorKind,
    },
    Blocked {
        task: usize,
    },
}

#[test]
fn runs_ready_tasks_and_releases_dependents() {
    let workflow = Workflow::try_from(vec![
        command_task("prepare", &[], "bash", arguments(&["-c", "printf prepare"])).unwrap(),
        command_task(
            "independent",
            &[],
            "bash",
            arguments(&["-c", "printf independent"]),
        )
        .unwrap(),
        command_task(
            "verify",
            &["prepare"],
            "bash",
            arguments(&["-c", "printf verify"]),
        )
        .unwrap(),
    ])
    .unwrap();
    let mut events = Vec::new();

    let status = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |event| {
            record_event(&mut events, event);
            Ok(())
        })
        .unwrap();

    assert_eq!(status, 0);
    assert_eq!(count_started(&events, 0), 1);
    assert_eq!(count_started(&events, 1), 1);
    assert_eq!(count_started(&events, 2), 1);
    assert_eq!(count_finished(&events, 0), 1);
    assert_eq!(count_finished(&events, 1), 1);
    assert_eq!(count_finished(&events, 2), 1);
    assert!(events.iter().any(|event| matches!(
        event,
        RecordedEvent::Finished {
            task: Some(2),
            stdout,
            stderr,
            status: Some(0),
        } if stdout == b"verify" && stderr.is_empty()
    )));

    let prepare_finished = event_position(&events, |event| is_finished(event, 0)).unwrap();
    let verify_started = event_position(&events, |event| is_started(event, 2)).unwrap();
    assert!(prepare_finished < verify_started);
}

#[test]
fn fills_output_placeholders_from_finished_dependencies() {
    let workflow = Workflow::try_from(vec![
        command_task(
            "inspect",
            &[],
            "bash",
            arguments(&["-c", "printf 'first\\nsecond\\n'; printf 'warn\\r\\n' >&2"]),
        )
        .unwrap(),
        command_task(
            "review",
            &["inspect"],
            "printf",
            arguments(&[
                "%s|%s|%s",
                "{{ tasks.inspect.stdout }}",
                "{{tasks.inspect.stderr}}",
                "{{ literal }}",
            ]),
        )
        .unwrap(),
    ])
    .unwrap();
    let mut events = Vec::new();

    let status = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |event| {
            record_event(&mut events, event);
            Ok(())
        })
        .unwrap();

    assert_eq!(status, 0);
    assert!(
        events.iter().any(|event| matches!(
            event,
            RecordedEvent::Finished {
                task: Some(1),
                stdout,
                status: Some(0),
                ..
            } if stdout == b"first\nsecond|warn|{{ literal }}"
        )),
        "{events:?}"
    );
}

#[test]
fn blocks_dependents_after_a_failed_task() {
    let directory = TemporaryDirectory::new("runner-failed-dependency").unwrap();
    let marker = directory.path().join("blocked-task-ran");
    let workflow = Workflow::try_from(vec![
        command_task("prepare", &[], "bash", arguments(&["-c", "exit 23"])).unwrap(),
        marker_task("verify", &["prepare"], &marker).unwrap(),
    ])
    .unwrap();
    let mut events = Vec::new();

    let status = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |event| {
            record_event(&mut events, event);
            Ok(())
        })
        .unwrap();

    assert_eq!(status, 23);
    assert!(!marker.exists());
    assert_eq!(count_started(&events, 0), 1);
    assert_eq!(count_finished(&events, 0), 1);
    assert_eq!(count_started(&events, 1), 0);
    assert_eq!(count_finished(&events, 1), 0);
}

#[test]
fn treats_a_signalled_task_as_a_failure_and_blocks_dependents() {
    let directory = TemporaryDirectory::new("runner-signalled-task").unwrap();
    let marker = directory.path().join("blocked-task-ran");
    let workflow = Workflow::try_from(vec![
        command_task(
            "signalled",
            &[],
            "bash",
            arguments(&["-c", "kill -TERM $$"]),
        )
        .unwrap(),
        marker_task("verify", &["signalled"], &marker).unwrap(),
    ])
    .unwrap();
    let mut events = Vec::new();

    let status = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |event| {
            record_event(&mut events, event);
            Ok(())
        })
        .unwrap();

    // A signalled task has no exit code, so the workflow reports 1.
    assert_eq!(status, 1);
    assert!(!marker.exists());
    assert!(events.iter().any(|event| matches!(
        event,
        RecordedEvent::Finished {
            task: Some(0),
            status: None,
            ..
        }
    )));
    assert_eq!(count_started(&events, 1), 0);
}

#[test]
fn reports_siblings_that_completed_when_a_task_cannot_start() {
    let directory = TemporaryDirectory::new("runner-missing-program").unwrap();
    let missing_program = directory.path().join("missing-program");
    let workflow = Workflow::try_from(vec![
        command_task("missing", &[], &text(&missing_program), Vec::new()).unwrap(),
        command_task(
            "succeeds",
            &[],
            "bash",
            arguments(&["-c", "printf sibling"]),
        )
        .unwrap(),
    ])
    .unwrap();
    let mut events = Vec::new();

    let error = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |event| {
            record_event(&mut events, event);
            Ok(())
        })
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(events.iter().any(|event| matches!(
        event,
        RecordedEvent::Failed {
            task: Some(0),
            error_kind: io::ErrorKind::NotFound,
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        RecordedEvent::Finished {
            task: Some(1),
            stdout,
            status: Some(0),
            ..
        } if stdout == b"sibling"
    )));
}

#[test]
fn reports_dependents_of_a_task_that_cannot_start_as_blocked() {
    let directory = TemporaryDirectory::new("runner-missing-program-dependents").unwrap();
    let missing_program = directory.path().join("missing-program");
    let marker = directory.path().join("blocked-task-ran");
    let workflow = Workflow::try_from(vec![
        command_task("missing", &[], &text(&missing_program), Vec::new()).unwrap(),
        marker_task("verify", &["missing"], &marker).unwrap(),
        marker_task("publish", &["verify"], &marker).unwrap(),
    ])
    .unwrap();
    let mut events = Vec::new();

    let error = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |event| {
            record_event(&mut events, event);
            Ok(())
        })
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(!marker.exists());
    assert_eq!(
        events,
        vec![
            RecordedEvent::Started { task: Some(0) },
            RecordedEvent::Failed {
                task: Some(0),
                error_kind: io::ErrorKind::NotFound,
            },
            RecordedEvent::Blocked { task: 1 },
            RecordedEvent::Blocked { task: 2 },
        ]
    );
}

#[test]
fn reports_direct_start_failures_as_terminal_events() {
    let directory = TemporaryDirectory::new("runner-direct-missing-program").unwrap();
    let mut events = Vec::new();

    let error = Runner::here()
        .unwrap()
        .run_request_in(
            ExecutionRequest::Command(ProcessCall::new(directory.path().join("missing-program"))),
            None,
            &mut |event| {
                record_event(&mut events, event);
                Ok(())
            },
        )
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(matches!(
        events.as_slice(),
        [
            RecordedEvent::Started { task: None },
            RecordedEvent::Failed {
                task: None,
                error_kind: io::ErrorKind::NotFound,
            }
        ]
    ));
}

#[test]
fn stops_before_starting_a_request_when_the_callback_fails() {
    let directory = TemporaryDirectory::new("runner-callback-failure").unwrap();
    let marker = directory.path().join("request-ran");
    let request = ExecutionRequest::Command(
        ProcessCall::new("bash")
            .argument("-c")
            .argument("printf ran > \"$1\"")
            .argument("beez")
            .argument(text(&marker)),
    );

    let error = Runner::here()
        .unwrap()
        .run_request_in(request, None, &mut |_| {
            Err(io::Error::other("cannot report"))
        })
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(!marker.exists());
}

#[test]
fn reports_the_status_of_the_first_failing_task_in_declaration_order() {
    let workflow = Workflow::try_from(vec![
        command_task("first", &[], "bash", arguments(&["-c", "exit 23"])).unwrap(),
        command_task("second", &[], "bash", arguments(&["-c", "exit 7"])).unwrap(),
    ])
    .unwrap();

    let status = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |_| Ok(()))
        .unwrap();

    assert_eq!(status, 23);
}

#[test]
fn stops_before_starting_a_workflow_task_when_the_callback_fails() {
    let directory = TemporaryDirectory::new("runner-workflow-callback-failure").unwrap();
    let marker = directory.path().join("task-ran");
    let workflow = Workflow::try_from(vec![marker_task("prepare", &[], &marker).unwrap()]).unwrap();

    let error = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |_| Err(io::Error::other("cannot report")))
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(!marker.exists());
}

#[test]
fn stops_a_workflow_when_reporting_a_finished_task_fails() {
    let directory = TemporaryDirectory::new("runner-finished-callback-failure").unwrap();
    let marker = directory.path().join("dependent-ran");
    let workflow = Workflow::try_from(vec![
        command_task("prepare", &[], "bash", arguments(&["-c", "printf prepare"])).unwrap(),
        marker_task("verify", &["prepare"], &marker).unwrap(),
    ])
    .unwrap();

    let error = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |event| match event {
            RunEvent::Finished { .. } => Err(io::Error::other("cannot report")),
            _ => Ok(()),
        })
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(!marker.exists());
}

#[test]
fn starts_independent_processes_before_either_can_finish() {
    let directory = TemporaryDirectory::new("runner-concurrent-tasks").unwrap();
    let first_ready = directory.path().join("first-ready");
    let first_release = directory.path().join("first-release");
    let second_ready = directory.path().join("second-ready");
    let second_release = directory.path().join("second-release");

    for fifo in [&first_ready, &first_release, &second_ready, &second_release] {
        create_fifo(fifo).unwrap();
    }

    let workflow = Workflow::try_from(vec![
        waiting_task("first", &first_ready, &first_release).unwrap(),
        waiting_task("second", &second_ready, &second_release).unwrap(),
    ])
    .unwrap();
    let first_ready_receiver = read_fifo(first_ready);
    let second_ready_receiver = read_fifo(second_ready);
    let (finished_sender, finished_receiver) = mpsc::channel();
    let runner_thread = thread::spawn(move || {
        let mut on_event = |_: &RunEvent<'_>| Ok(());
        let result = Runner::here()
            .unwrap()
            .run_workflow(&workflow, &mut on_event);
        let _ = finished_sender.send(result);
    });

    let timeout = Duration::from_secs(3);
    let first_started = received_fifo_byte(&first_ready_receiver, timeout);
    let second_started = received_fifo_byte(&second_ready_receiver, timeout);
    let first_release_receiver = write_fifo(first_release);
    let second_release_receiver = write_fifo(second_release);
    received_fifo_write(&first_release_receiver, timeout).unwrap();
    received_fifo_write(&second_release_receiver, timeout).unwrap();
    let status = received_workflow_status(&finished_receiver, timeout).unwrap();
    runner_thread
        .join()
        .map_err(|_| io::Error::other("runner thread panicked"))
        .unwrap();

    assert!(first_started);
    assert!(second_started);
    assert_eq!(status, 0);
}

#[test]
fn reports_output_before_the_task_finishes() {
    let workflow = Workflow::try_from(vec![
        command_task(
            "stream",
            &[],
            "bash",
            arguments(&["-c", "echo early; sleep 0.2"]),
        )
        .unwrap(),
    ])
    .unwrap();
    let mut events = Vec::new();

    let status = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |event| {
            record_event(&mut events, event);
            Ok(())
        })
        .unwrap();

    assert_eq!(status, 0);
    let reported = event_position(&events, |event| {
        matches!(
            event,
            RecordedEvent::Output { line, stream, .. }
                if line == b"early\n" && *stream == OutputStream::Stdout
        )
    })
    .unwrap();
    let finished = event_position(&events, |event| is_finished(event, 0)).unwrap();
    assert!(reported < finished);
}

#[test]
fn reports_a_blocked_task_after_a_dependency_fails() {
    let workflow = Workflow::try_from(vec![
        command_task("prepare", &[], "bash", arguments(&["-c", "exit 3"])).unwrap(),
        command_task("verify", &["prepare"], "bash", arguments(&["-c", "true"])).unwrap(),
    ])
    .unwrap();
    let mut events = Vec::new();

    let status = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |event| {
            record_event(&mut events, event);
            Ok(())
        })
        .unwrap();

    assert_eq!(status, 3);
    assert!(events.contains(&RecordedEvent::Blocked { task: 1 }));
}

fn command_task(
    id: &str,
    depends_on: &[&str],
    program: &str,
    arguments: Vec<String>,
) -> Result<TaskDefinition, beez_core::TaskIdError> {
    let dependencies = depends_on
        .iter()
        .map(|dependency| dependency.parse())
        .collect::<Result<Vec<_>, _>>()?;

    Ok(TaskDefinition::new(
        id.parse()?,
        dependencies,
        TaskRequest::command(program.to_owned(), arguments),
    ))
}

fn arguments(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// Writes `marker`, so a test can prove whether the task ran.
fn marker_task(
    id: &str,
    depends_on: &[&str],
    marker: &Path,
) -> Result<TaskDefinition, beez_core::TaskIdError> {
    command_task(
        id,
        depends_on,
        "bash",
        vec![
            "-c".to_owned(),
            "printf ran > \"$1\"".to_owned(),
            "beez".to_owned(),
            text(marker),
        ],
    )
}

fn record_event(events: &mut Vec<RecordedEvent>, event: &RunEvent<'_>) {
    let event = match event {
        RunEvent::Started { task } => RecordedEvent::Started {
            task: (*task).map(TaskIndex::position),
        },
        RunEvent::Output { task, stream, line } => RecordedEvent::Output {
            task: (*task).map(TaskIndex::position),
            stream: *stream,
            line: line.to_vec(),
        },
        RunEvent::Finished { task, output, .. } => RecordedEvent::Finished {
            task: (*task).map(TaskIndex::position),
            stdout: output.stdout().to_vec(),
            stderr: output.stderr().to_vec(),
            status: output.status_code(),
        },
        RunEvent::Failed {
            task, error_kind, ..
        } => RecordedEvent::Failed {
            task: (*task).map(TaskIndex::position),
            error_kind: *error_kind,
        },
        RunEvent::Blocked { task } => RecordedEvent::Blocked {
            task: task.position(),
        },
    };
    events.push(event);
}

fn count_started(events: &[RecordedEvent], task: usize) -> usize {
    events
        .iter()
        .filter(|event| is_started(event, task))
        .count()
}

fn count_finished(events: &[RecordedEvent], task: usize) -> usize {
    events
        .iter()
        .filter(|event| is_finished(event, task))
        .count()
}

fn is_started(event: &RecordedEvent, task: usize) -> bool {
    matches!(event, RecordedEvent::Started { task: Some(index) } if *index == task)
}

fn is_finished(event: &RecordedEvent, task: usize) -> bool {
    matches!(event, RecordedEvent::Finished { task: Some(index), .. } if *index == task)
}

fn event_position(
    events: &[RecordedEvent],
    predicate: impl Fn(&RecordedEvent) -> bool,
) -> Result<usize, io::Error> {
    events
        .iter()
        .position(predicate)
        .ok_or_else(|| io::Error::other("required lifecycle event was not reported"))
}

fn waiting_task(
    id: &str,
    ready: &Path,
    release: &Path,
) -> Result<TaskDefinition, beez_core::TaskIdError> {
    command_task(
        id,
        &[],
        "bash",
        vec![
            "-c".to_owned(),
            "printf ready > \"$1\"; read _ < \"$2\"".to_owned(),
            "beez".to_owned(),
            text(ready),
            text(release),
        ],
    )
}

fn create_fifo(path: &Path) -> io::Result<()> {
    let status = process::Command::new("mkfifo").arg(path).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "mkfifo failed for {} with {status}",
            path.display()
        )))
    }
}

fn read_fifo(path: PathBuf) -> mpsc::Receiver<io::Result<()>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = File::open(path).and_then(|mut file| {
            let mut byte = [0];
            file.read_exact(&mut byte)
        });
        let _ = sender.send(result);
    });
    receiver
}

fn write_fifo(path: PathBuf) -> mpsc::Receiver<io::Result<()>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|mut file| file.write_all(b"release\n"));
        let _ = sender.send(result);
    });
    receiver
}

fn received_fifo_byte(receiver: &mpsc::Receiver<io::Result<()>>, timeout: Duration) -> bool {
    matches!(receiver.recv_timeout(timeout), Ok(Ok(())))
}

fn received_fifo_write(
    receiver: &mpsc::Receiver<io::Result<()>>,
    timeout: Duration,
) -> io::Result<()> {
    receiver.recv_timeout(timeout).map_err(|error| {
        io::Error::other(format!("release pipe did not receive a reader: {error}"))
    })?
}

fn received_workflow_status(
    receiver: &mpsc::Receiver<Result<i32, RunError>>,
    timeout: Duration,
) -> Result<i32, Box<dyn std::error::Error>> {
    Ok(receiver
        .recv_timeout(timeout)
        .map_err(|error| format!("workflow did not finish: {error}"))??)
}

// The Linux sandbox re-executes the `beez` binary inside its namespace, so
// only `beez-cli` tests can run it. They cover both platforms.
#[cfg(target_os = "macos")]
#[test]
fn runs_sandboxed_tasks_inside_the_sandbox() {
    use beez_policy::{FilesystemPolicy, NetworkPolicy, SandboxPolicy};

    let directory = TemporaryDirectory::new("runner-sandbox").unwrap();
    let outside = directory.join("outside.txt");
    let filesystem = "."
        .parse()
        .map(|work| {
            FilesystemPolicy::default()
                .with_defaults(Some(false))
                .with_write_allow(vec![work])
        })
        .unwrap();
    let policy = SandboxPolicy::new(NetworkPolicy::default(), filesystem, None);
    let workflow = Workflow::try_from(vec![
        command_task("open", &[], "bash", arguments(&["-c", "printf open"]))
            .unwrap()
            .sandboxed(Some(policy.clone())),
        command_task(
            "leak",
            &[],
            "bash",
            arguments(&["-c", &format!("printf leak > {}", outside.display())]),
        )
        .unwrap()
        .sandboxed(Some(policy)),
    ])
    .unwrap();
    let mut events = Vec::new();

    let status = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |event| {
            record_event(&mut events, event);
            Ok(())
        })
        .unwrap();

    assert_ne!(status, 0);
    assert!(!outside.exists());
    assert!(events.iter().any(|event| matches!(
        event,
        RecordedEvent::Finished { task: Some(0), stdout, status: Some(0), .. } if stdout == b"open"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        RecordedEvent::Finished { task: Some(1), status: Some(status), .. } if *status != 0
    )));
}

#[test]
fn stops_running_tasks_when_the_callback_fails() {
    let workflow = Workflow::try_from(vec![
        command_task("prepare", &[], "bash", arguments(&["-c", "printf prepare"])).unwrap(),
        command_task("wait", &[], "sleep", arguments(&["30"])).unwrap(),
    ])
    .unwrap();
    let started = Instant::now();

    let error = Runner::here()
        .unwrap()
        .run_workflow(&workflow, &mut |event| match event {
            RunEvent::Finished { .. } => Err(io::Error::other("cannot report")),
            _ => Ok(()),
        })
        .unwrap_err();

    assert_eq!(error.to_string(), "cannot report");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}
