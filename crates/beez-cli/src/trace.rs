//! Diagnostic tracing for people who debug Beez itself.
//!
//! Tracing stays off unless `BEEZ_TRACE` holds a filter, such as `debug` or
//! `beez_sandbox=trace`, in the syntax of `tracing_subscriber::EnvFilter`.
//! The lines go to standard error, apart from Beez's own output: errors,
//! status lines, and the run log keep their shape whether tracing is on or
//! not.

use std::io::{self, Write};
use std::sync::OnceLock;

use indicatif::MultiProgress;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// The variable that turns tracing on, and holds its filter.
pub(crate) const VARIABLE: &str = "BEEZ_TRACE";

/// The spinner rows of a run, which every trace line holds still.
static BARS: OnceLock<MultiProgress> = OnceLock::new();

/// Starts tracing when `BEEZ_TRACE` asks for it.
///
/// A filter that does not parse leaves tracing off and says why on standard
/// error, so a typo can neither stop Beez nor go unnoticed.
pub(crate) fn init() {
    let Some(value) = std::env::var_os(VARIABLE) else {
        return;
    };
    let value = value.to_string_lossy();
    if value.trim().is_empty() {
        return;
    }
    let filter = match EnvFilter::builder().parse(value.as_ref()) {
        Ok(filter) => filter,
        Err(error) => {
            eprintln!("warning: ignoring {VARIABLE}={value:?}, tracing stays off: {error}");
            return;
        }
    };
    // Only a second subscriber fails here, and the first one keeps working.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(TraceWriter)
        .try_init();
}

/// Holds `bars` still around every later trace line, so a line never lands
/// in the middle of a redrawn spinner row.
///
/// One process draws one run, so only the first call takes effect.
pub(crate) fn hold_still(bars: &MultiProgress) {
    let _ = BARS.set(bars.clone());
}

/// Makes one [`TraceLine`] for each trace event.
#[derive(Clone, Copy, Debug)]
struct TraceWriter;

impl<'writer> MakeWriter<'writer> for TraceWriter {
    type Writer = TraceLine;

    fn make_writer(&'writer self) -> Self::Writer {
        TraceLine(Vec::new())
    }
}

/// One trace event, collected whole and written to standard error on drop.
///
/// Writing the event in one piece lets the spinners pause once for it.
#[derive(Debug)]
struct TraceLine(Vec<u8>);

impl Write for TraceLine {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for TraceLine {
    fn drop(&mut self) {
        if self.0.is_empty() {
            return;
        }
        // Standard error may be closed, and a trace line is never worth
        // failing for.
        let write = || {
            let _ = io::stderr().lock().write_all(&self.0);
        };
        match BARS.get() {
            // Hidden spinners draw nothing, so there is nothing to hold still.
            Some(bars) if !bars.is_hidden() => bars.suspend(write),
            _ => write(),
        }
    }
}
