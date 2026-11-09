//! Reports why the daemon or a command to it could not do its work.

use std::error::Error;
use std::io;
use std::path::PathBuf;

use crate::message;

/// Reports why the daemon or a command to it could not do its work.
///
/// A command the daemon refuses is not an error: it answers with
/// [`Answer::Refused`](crate::Answer::Refused) instead.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// A file or the socket could not be read or written.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The daemon keeps its files in the home directory, which is unknown.
    #[error("the daemon needs the home directory")]
    NoHome,
    /// The directory that holds the daemon's files could not be created.
    #[error("cannot create {}", .path.display())]
    CreateDirectory {
        /// The directory the daemon needs.
        path: PathBuf,
        /// Why it could not be created.
        #[source]
        source: io::Error,
    },
    /// Another daemon already serves this Beez root.
    #[error("{}", message::already_running(.0))]
    AlreadyRunning(PathBuf),
    /// A file the daemon keeps holds something Beez cannot read.
    #[error("cannot read {}", .path.display())]
    Unreadable {
        /// The file.
        path: PathBuf,
        /// What is wrong with its contents.
        #[source]
        source: serde_json::Error,
    },
    /// The other end of the socket hung up before it sent a line.
    #[error("{0}")]
    Closed(&'static str),
    /// A line on the socket is not a message Beez knows.
    #[error("cannot read the message on the daemon socket")]
    Message(#[source] serde_json::Error),
    /// The daemon answered a command with something else than a line.
    #[error("unexpected answer from the daemon")]
    UnexpectedAnswer,
    /// The daemon answered a status request with a line that says why it
    /// has no status.
    #[error("{0}")]
    NoStatus(String),
}

/// The error and every cause behind it, on one line.
///
/// Errors that leave the process as text, over the socket or in a job's
/// status, keep their causes this way.
pub(crate) fn one_line(error: &dyn Error) -> String {
    let mut line = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        line.push_str(": ");
        line.push_str(&cause.to_string());
        source = cause.source();
    }
    line
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use super::{DaemonError, one_line};

    #[test]
    fn writes_every_cause_on_one_line() {
        let error = DaemonError::CreateDirectory {
            path: PathBuf::from("/beez"),
            source: io::Error::other("disk full"),
        };

        assert_eq!(one_line(&error), "cannot create /beez: disk full");
    }
}
