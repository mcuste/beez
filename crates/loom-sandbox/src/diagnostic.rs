use std::sync::OnceLock;

/// Receives Loom's own sandbox diagnostics.
type Sink = Box<dyn Fn(&str) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Sends every later sandbox diagnostic to `sink` instead of standard error.
///
/// The sink receives the bare message, such as `denied connection to
/// example.com:443`. Without a sink the message goes to standard error behind
/// `loom sandbox:`.
///
/// Diagnostics come from background threads, so a program that draws on the
/// terminal must route them through its own writer, or they land in the middle
/// of a redrawn line. Only the first call takes effect, and it returns false
/// when a sink is already installed.
pub fn set_diagnostic_sink(sink: Sink) -> bool {
    SINK.set(sink).is_ok()
}

pub(crate) fn report(message: &str) {
    match SINK.get() {
        Some(sink) => sink(message),
        None => eprintln!("loom sandbox: {message}"),
    }
}
