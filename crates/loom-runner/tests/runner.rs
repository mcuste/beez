//! Runner integration tests.
#![cfg(unix)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use loom_core::{TaskDefinition, TaskIndex, TaskRequest, Workflow};
use loom_process::{ExecutionRequest, ProcessCall};
use loom_runner::{RunEvent, Runner};

macro_rules! assert_ok {
    ($result:expr) => {{
        let result = $result;
        assert!(result.is_ok());
        let Ok(value) = result else {
            return;
        };
        value
    }};
}

macro_rules! assert_err {
    ($result:expr) => {{
        let result = $result;
        assert!(result.is_err());
        let Err(error) = result else {
            return;
        };
        error
    }};
}

#[derive(Debug, Eq, PartialEq)]
enum RecordedEvent {
    Started {
        task: Option<usize>,
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
}

#[test]
fn runs_ready_tasks_and_releases_dependents() {
    let workflow = assert_ok!(Workflow::try_from(vec![
        assert_ok!(command_task(
            "prepare",
            &[],
            "bash",
            arguments(&["-c", "printf prepare"]),
        )),
        assert_ok!(command_task(
            "independent",
            &[],
            "bash",
            arguments(&["-c", "printf independent"]),
        )),
        assert_ok!(command_task(
            "verify",
            &["prepare"],
            "bash",
            arguments(&["-c", "printf verify"]),
        )),
    ]));
    let mut events = Vec::new();

    let status = assert_ok!(Runner.run_workflow(&workflow, &mut |event| {
        record_event(&mut events, event);
        Ok(())
    }));

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

    let prepare_finished = assert_ok!(event_position(&events, |event| is_finished(event, 0)));
    let verify_started = assert_ok!(event_position(&events, |event| is_started(event, 2)));
    assert!(prepare_finished < verify_started);
}

#[test]
fn blocks_dependents_after_a_failed_task() {
    let directory = assert_ok!(temporary_directory("failed-dependency"));
    let marker = directory.path().join("blocked-task-ran");
    let workflow = assert_ok!(Workflow::try_from(vec![
        assert_ok!(command_task(
            "prepare",
            &[],
            "bash",
            arguments(&["-c", "exit 23"]),
        )),
        assert_ok!(command_task(
            "verify",
            &["prepare"],
            "bash",
            vec![
                "-c".to_owned(),
                "printf ran > \"$1\"".to_owned(),
                "loom".to_owned(),
                marker.to_string_lossy().into_owned(),
            ],
        )),
    ]));
    let mut events = Vec::new();

    let status = assert_ok!(Runner.run_workflow(&workflow, &mut |event| {
        record_event(&mut events, event);
        Ok(())
    }));

    assert_eq!(status, 23);
    assert!(!marker.exists());
    assert_eq!(count_started(&events, 0), 1);
    assert_eq!(count_finished(&events, 0), 1);
    assert_eq!(count_started(&events, 1), 0);
    assert_eq!(count_finished(&events, 1), 0);
}

#[test]
fn reports_siblings_that_completed_when_a_task_cannot_start() {
    let directory = assert_ok!(temporary_directory("missing-program"));
    let missing_program = directory.path().join("missing-program");
    let workflow = assert_ok!(Workflow::try_from(vec![
        assert_ok!(command_task(
            "missing",
            &[],
            &missing_program.to_string_lossy(),
            Vec::new(),
        )),
        assert_ok!(command_task(
            "succeeds",
            &[],
            "bash",
            arguments(&["-c", "printf sibling"]),
        )),
    ]));
    let mut events = Vec::new();

    let error = assert_err!(Runner.run_workflow(&workflow, &mut |event| {
        record_event(&mut events, event);
        Ok(())
    }));

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
fn reports_direct_start_failures_as_terminal_events() {
    let directory = assert_ok!(temporary_directory("direct-missing-program"));
    let mut events = Vec::new();

    let error = assert_err!(Runner.run_request(
        ExecutionRequest::Command(ProcessCall::new(directory.path().join("missing-program"))),
        &mut |event| {
            record_event(&mut events, event);
            Ok(())
        },
    ));

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
    let directory = assert_ok!(temporary_directory("callback-failure"));
    let marker = directory.path().join("request-ran");
    let request = ExecutionRequest::Command(
        ProcessCall::new("bash")
            .argument("-c")
            .argument("printf ran > \"$1\"")
            .argument("loom")
            .argument(marker.to_string_lossy().into_owned()),
    );

    let error =
        assert_err!(Runner.run_request(request, &mut |_| Err(io::Error::other("cannot report")),));

    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(!marker.exists());
}

#[test]
fn starts_independent_processes_before_either_can_finish() {
    let directory = assert_ok!(temporary_directory("concurrent-tasks"));
    let first_ready = directory.path().join("first-ready");
    let first_release = directory.path().join("first-release");
    let second_ready = directory.path().join("second-ready");
    let second_release = directory.path().join("second-release");

    for fifo in [&first_ready, &first_release, &second_ready, &second_release] {
        assert_ok!(create_fifo(fifo));
    }

    let workflow = assert_ok!(Workflow::try_from(vec![
        assert_ok!(waiting_task("first", &first_ready, &first_release)),
        assert_ok!(waiting_task("second", &second_ready, &second_release)),
    ]));
    let first_ready_receiver = read_fifo(first_ready);
    let second_ready_receiver = read_fifo(second_ready);
    let (finished_sender, finished_receiver) = mpsc::channel();
    let runner_thread = thread::spawn(move || {
        let mut on_event = |_: &RunEvent| Ok(());
        let result = Runner.run_workflow(&workflow, &mut on_event);
        let _ = finished_sender.send(result);
    });

    let timeout = Duration::from_secs(3);
    let first_started = received_fifo_byte(&first_ready_receiver, timeout);
    let second_started = received_fifo_byte(&second_ready_receiver, timeout);
    let first_release_receiver = write_fifo(first_release);
    let second_release_receiver = write_fifo(second_release);
    assert_ok!(received_fifo_write(&first_release_receiver, timeout));
    assert_ok!(received_fifo_write(&second_release_receiver, timeout));
    let status = assert_ok!(received_workflow_status(&finished_receiver, timeout));
    assert_ok!(
        runner_thread
            .join()
            .map_err(|_| io::Error::other("runner thread panicked"))
    );

    assert!(first_started);
    assert!(second_started);
    assert_eq!(status, 0);
}

fn command_task(
    id: &str,
    depends_on: &[&str],
    program: &str,
    arguments: Vec<String>,
) -> Result<TaskDefinition, loom_core::TaskIdError> {
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

fn record_event(events: &mut Vec<RecordedEvent>, event: &RunEvent) {
    let event = match event {
        RunEvent::Started { task } => RecordedEvent::Started {
            task: (*task).map(TaskIndex::position),
        },
        RunEvent::Finished { task, output } => RecordedEvent::Finished {
            task: (*task).map(TaskIndex::position),
            stdout: output.stdout().to_vec(),
            stderr: output.stderr().to_vec(),
            status: output.status_code(),
        },
        RunEvent::Failed { task, error_kind } => RecordedEvent::Failed {
            task: (*task).map(TaskIndex::position),
            error_kind: *error_kind,
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

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temporary_directory(name: &str) -> io::Result<TemporaryDirectory> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("loom-runner-{name}-{}-{timestamp}", process::id()));

    fs::create_dir(&directory)?;
    Ok(TemporaryDirectory(directory))
}

fn waiting_task(
    id: &str,
    ready: &Path,
    release: &Path,
) -> Result<TaskDefinition, loom_core::TaskIdError> {
    command_task(
        id,
        &[],
        "bash",
        vec![
            "-c".to_owned(),
            "printf ready > \"$1\"; read _ < \"$2\"".to_owned(),
            "loom".to_owned(),
            ready.to_string_lossy().into_owned(),
            release.to_string_lossy().into_owned(),
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
    receiver: &mpsc::Receiver<io::Result<i32>>,
    timeout: Duration,
) -> io::Result<i32> {
    receiver
        .recv_timeout(timeout)
        .map_err(|error| io::Error::other(format!("workflow did not finish: {error}")))?
}
