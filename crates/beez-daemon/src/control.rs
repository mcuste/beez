//! The commands that reach a running daemon.
//!
//! One request and one response cross the socket, each as one line of JSON.

use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::DaemonError;
use crate::message::Refusal;

/// The name of one job: the manifest name, and the schedule name after a
/// colon.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct JobId(String);

impl JobId {
    /// The ID of the schedule `name` of the manifest named `stem`, or of the
    /// only schedule of the manifest when the schedule has no name.
    pub(crate) fn new(stem: &str, name: Option<&str>) -> Self {
        match name {
            Some(name) => Self(format!("{stem}:{name}")),
            None => Self(stem.to_owned()),
        }
    }

    /// True when a manifest named `stem` may hold this job, under any
    /// schedule name.
    ///
    /// The ID alone cannot name its manifest for certain: a stem or a schedule
    /// name may hold a colon, so `a:b:c` belongs to both `a` and `a:b`.
    pub(crate) fn belongs_to(&self, stem: &str) -> bool {
        self.0
            .strip_prefix(stem)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(':'))
    }
}

impl From<String> for JobId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// What a command asks the daemon to do.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub(crate) enum Request {
    /// Reports the daemon and every job it holds.
    Status,
    /// Watches one more manifest.
    Add {
        /// Absolute path of the manifest.
        manifest: PathBuf,
        /// Directory the tasks run in.
        working_directory: PathBuf,
    },
    /// Stops watching one manifest.
    Remove {
        /// Absolute path of the manifest.
        manifest: PathBuf,
    },
    /// Holds one job back until it resumes.
    Pause {
        /// Job ID.
        job: JobId,
    },
    /// Lets a paused job fire again.
    Resume {
        /// Job ID.
        job: JobId,
    },
    /// Runs one job now, beside its schedule.
    Trigger {
        /// Job ID.
        job: JobId,
    },
    /// Reads every watched manifest again.
    Reload,
    /// Stops the daemon once its running runs end.
    Stop,
}

/// What the daemon answers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum Response {
    /// The daemon and its jobs.
    Status(DaemonStatus),
    /// The command took effect.
    Done {
        /// One line for the person who sent the command.
        message: String,
    },
    /// The command did not take effect.
    Error {
        /// Why the command did not take effect.
        message: String,
    },
}

impl Response {
    /// The command took effect.
    #[must_use]
    pub(crate) fn done(message: impl Into<String>) -> Self {
        Self::Done {
            message: message.into(),
        }
    }

    /// The command did not take effect.
    #[must_use]
    pub(crate) fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }
}

impl From<Refusal> for Response {
    fn from(refusal: Refusal) -> Self {
        Self::error(refusal.to_string())
    }
}

/// A running daemon and every job it holds.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DaemonStatus {
    /// Process ID of the daemon.
    pub pid: u32,
    /// When the daemon started, as a UTC date and time.
    pub started: String,
    /// How many runs are going.
    pub running: usize,
    /// Every job the daemon holds.
    pub jobs: Vec<JobReport>,
}

/// One job, as a command reports it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JobReport {
    /// Job ID: the manifest name, and the schedule name after a colon.
    pub id: JobId,
    /// Path of the manifest that holds the schedule.
    pub manifest: String,
    /// The schedule as the manifest writes it.
    pub schedule: Option<String>,
    /// What the job is doing.
    pub condition: Condition,
    /// The next time the job fires, as a UTC date and time.
    pub next_fire: Option<String>,
    /// The last time the job fired.
    pub last_fire: Option<String>,
    /// Directory name of the last run the job started.
    pub last_run: Option<String>,
    /// Exit status of the last run.
    pub last_status: Option<i32>,
    /// Why the job cannot run, when it cannot.
    pub error: Option<String>,
}

/// What a job is doing, as `beez schedule list` prints it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// The job waits for its next fire.
    Waiting,
    /// At least one run of the job is going.
    Running,
    /// A fire waits for room to run.
    Queued,
    /// A person holds the job back.
    Paused,
    /// The manifest keeps the schedule off.
    Disabled,
    /// The schedule has no fire left.
    Done,
    /// The job cannot run.
    Broken,
}

impl fmt::Display for Condition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Waiting => "waiting",
            Self::Running => "running",
            Self::Queued => "queued",
            Self::Paused => "paused",
            Self::Disabled => "disabled",
            Self::Done => "done",
            Self::Broken => "broken",
        })
    }
}

/// Sends one command to the daemon and reads its answer.
pub(crate) fn send(socket: &Path, request: &Request) -> Result<Response, DaemonError> {
    let stream = UnixStream::connect(socket)?;
    write_line(&stream, request)?;

    read_line(&stream, "daemon closed the connection")
}

/// True when a daemon answers on the socket.
#[must_use]
pub fn is_running(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

/// Writes one message as one line.
pub(crate) fn write_line<T: Serialize>(mut stream: &UnixStream, message: &T) -> io::Result<()> {
    let line = serde_json::to_string(message)?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;

    stream.flush()
}

/// Reads one message from one line.
pub(crate) fn read_request(stream: &UnixStream) -> Result<Request, DaemonError> {
    read_line(stream, "no command arrived")
}

/// Reads one message from one line, and reports `empty` when none arrives.
fn read_line<T: DeserializeOwned>(
    stream: &UnixStream,
    empty: &'static str,
) -> Result<T, DaemonError> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(DaemonError::Closed(empty));
    }

    serde_json::from_str(&line).map_err(DaemonError::Message)
}

#[cfg(test)]
mod tests {
    use super::{Condition, DaemonStatus, JobId, JobReport, Request, Response};

    #[test]
    fn a_job_belongs_to_the_stem_before_its_schedule_name() {
        assert!(JobId::new("nightly", None).belongs_to("nightly"));
        assert!(JobId::new("nightly", Some("docs")).belongs_to("nightly"));
        assert!(!JobId::new("nightly-docs", None).belongs_to("nightly"));
        assert!(!JobId::new("nightly", Some("docs")).belongs_to("weekly"));
        assert!(!JobId::new("night", None).belongs_to("nightly"));
    }

    #[test]
    fn writes_a_command_as_one_line_of_json() {
        let request = Request::Trigger {
            job: JobId::new("nightly", Some("docs")),
        };
        let line = serde_json::to_string(&request).unwrap();

        assert_eq!(line, r#"{"command":"trigger","job":"nightly:docs"}"#);
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), request);
    }

    #[test]
    fn writes_an_answer_as_one_line_of_json() {
        let response = Response::Done {
            message: "paused nightly".to_owned(),
        };
        let line = serde_json::to_string(&response).unwrap();

        assert_eq!(line, r#"{"result":"done","message":"paused nightly"}"#);
        assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), response);
    }

    #[test]
    fn writes_the_status_of_a_daemon_as_one_flat_object() {
        let response = Response::Status(DaemonStatus {
            pid: 7,
            started: "2026-09-10T03:00:00Z".to_owned(),
            running: 1,
            jobs: vec![JobReport {
                id: JobId::new("nightly", Some("docs")),
                manifest: "/work/nightly.yaml".to_owned(),
                schedule: Some("0 3 * * *".to_owned()),
                condition: Condition::Running,
                next_fire: None,
                last_fire: None,
                last_run: None,
                last_status: Some(0),
                error: None,
            }],
        });
        let line = serde_json::to_string(&response).unwrap();

        assert_eq!(
            line,
            concat!(
                r#"{"result":"status","pid":7,"started":"2026-09-10T03:00:00Z","running":1,"#,
                r#""jobs":[{"id":"nightly:docs","manifest":"/work/nightly.yaml","#,
                r#""schedule":"0 3 * * *","condition":"running","next_fire":null,"#,
                r#""last_fire":null,"last_run":null,"last_status":0,"error":null}]}"#
            )
        );
        assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), response);
    }

    #[test]
    fn writes_each_condition_as_its_word() {
        for (condition, word) in [
            (Condition::Waiting, "waiting"),
            (Condition::Running, "running"),
            (Condition::Queued, "queued"),
            (Condition::Paused, "paused"),
            (Condition::Disabled, "disabled"),
            (Condition::Done, "done"),
            (Condition::Broken, "broken"),
        ] {
            let json = format!("\"{word}\"");

            assert_eq!(serde_json::to_string(&condition).unwrap(), json);
            assert_eq!(serde_json::from_str::<Condition>(&json).unwrap(), condition);
            assert_eq!(condition.to_string(), word);
        }
    }
}
