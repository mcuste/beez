//! The commands that reach a running daemon.
//!
//! One request and one response cross the socket, each as one line of JSON.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::message;

/// What a command asks the daemon to do.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
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
        job: String,
    },
    /// Lets a paused job fire again.
    Resume {
        /// Job ID.
        job: String,
    },
    /// Runs one job now, beside its schedule.
    Trigger {
        /// Job ID.
        job: String,
    },
    /// Reads every watched manifest again.
    Reload,
    /// Stops the daemon once its running runs end.
    Stop,
}

/// What the daemon answers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    /// The daemon and its jobs.
    Status {
        /// Process ID of the daemon.
        pid: u32,
        /// When the daemon started, as a UTC date and time.
        started: String,
        /// How many runs are going.
        running: usize,
        /// Every job the daemon holds.
        jobs: Vec<JobReport>,
    },
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
    pub fn done(message: impl Into<String>) -> Self {
        Self::Done {
            message: message.into(),
        }
    }

    /// The command did not take effect.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    /// The command named a job the daemon does not hold.
    pub(crate) fn unknown_job(job: &str) -> Self {
        Self::error(message::unknown_job(job))
    }
}

/// One job, as a command reports it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JobReport {
    /// Job ID: the manifest name, and the schedule name after a colon.
    pub id: String,
    /// Path of the manifest that holds the schedule.
    pub manifest: String,
    /// The schedule as the manifest writes it.
    pub schedule: Option<String>,
    /// What the job is doing: waiting, running, queued, paused, disabled,
    /// done, or broken.
    pub condition: String,
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

/// Sends one command to the daemon and reads its answer.
pub(crate) fn send(socket: &Path, request: &Request) -> io::Result<Response> {
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
    let line = serde_json::to_string(message).map_err(io::Error::other)?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;

    stream.flush()
}

/// Reads one message from one line.
pub(crate) fn read_request(stream: &UnixStream) -> io::Result<Request> {
    read_line(stream, "no command arrived")
}

/// Reads one message from one line, and reports `empty` when none arrives.
fn read_line<T: DeserializeOwned>(stream: &UnixStream, empty: &'static str) -> io::Result<T> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(io::Error::other(empty));
    }

    serde_json::from_str(&line).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::{Request, Response};

    #[test]
    fn writes_a_command_as_one_line_of_json() {
        let request = Request::Trigger {
            job: "nightly".to_owned(),
        };
        let line = serde_json::to_string(&request).unwrap();

        assert_eq!(line, r#"{"command":"trigger","job":"nightly"}"#);
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
}
