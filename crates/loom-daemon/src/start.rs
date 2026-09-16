//! Starts the daemon in the background.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::control::{self, Response};
use crate::paths::DaemonPaths;

/// How long to wait between two looks for the answer of a started daemon.
const WAIT: Duration = Duration::from_millis(100);
/// How many times to look, so a slow start still reports.
const ATTEMPTS: usize = 50;

/// Starts the daemon in its own process group, so a closing terminal leaves it
/// running, with its own lines in `daemon.log`.
///
/// The arguments are read back by Loom's own `daemon run` command, so their
/// names must match its options.
pub fn start(paths: &DaemonPaths, limit: usize, keep_runs: usize) -> io::Result<Response> {
    if control::is_running(paths.socket()) {
        return Ok(error(&format!(
            "a daemon already runs for {}",
            paths.root().display()
        )));
    }
    paths.create()?;
    let log = std::fs::File::options()
        .create(true)
        .append(true)
        .open(paths.log())?;
    let child = Command::new(std::env::current_exe()?)
        .args(["daemon", "run", "--root"])
        .arg(paths.root())
        .arg("--limit")
        .arg(limit.to_string())
        .arg("--keep-runs")
        .arg(keep_runs.to_string())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;

    for _ in 0..ATTEMPTS {
        if control::is_running(paths.socket()) {
            return Ok(Response::Done {
                message: format!(
                    "daemon started, pid {}, writing to {}",
                    child.id(),
                    paths.log().display()
                ),
            });
        }
        std::thread::sleep(WAIT);
    }

    Ok(error(&format!(
        "the daemon did not answer on {}, see {}",
        paths.socket().display(),
        paths.log().display()
    )))
}

fn error(message: &str) -> Response {
    Response::Error {
        message: message.to_owned(),
    }
}
