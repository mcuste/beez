//! The daemon's own lines.
//!
//! Every line goes to standard error, in the same shape as a run log line: a
//! UTC stamp, a status word in its own column, then the message. A detached
//! daemon has its standard error in `daemon.log`, and a service manager keeps
//! it wherever it keeps a service's output.

use std::time::SystemTime;

use loom_record::stamped_status_line;

/// Writes one line of the daemon's own log.
pub(crate) fn line(verb: &str, message: &str) {
    eprintln!("{}", stamped_status_line(SystemTime::now(), verb, message));
}
