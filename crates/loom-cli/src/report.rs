//! Renders run events for a terminal or a log.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anstream::AutoStream;
use anstyle::{AnsiColor, Color, Effects, Style};
use clap::ValueEnum;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use loom_core::TaskIndex;
use loom_process::{OutputStream, ProcessOutput};
use loom_runner::RunEvent;

use crate::log::{LogSettings, RunLog, RunTarget, TaskOutcome};
use crate::time::utc;

/// How Loom renders the output of workflow tasks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum OutputMode {
    /// Print every line as it arrives, behind the task ID.
    #[default]
    Stream,
    /// Collect each task and print it when the task ends.
    Grouped,
}

/// What Loom puts in front of every line it writes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum Timestamps {
    /// Date and time in UTC.
    #[default]
    DateTime,
    /// Seconds since the run started.
    Elapsed,
}

/// When Loom colours its own output.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum ColorMode {
    /// Colour a terminal only.
    #[default]
    Auto,
    /// Always colour.
    Always,
    /// Never colour.
    Never,
}

/// Colours for task prefixes. Red stays free for failures.
const PALETTE: [AnsiColor; 6] = [
    AnsiColor::Cyan,
    AnsiColor::Magenta,
    AnsiColor::Green,
    AnsiColor::Yellow,
    AnsiColor::Blue,
    AnsiColor::BrightMagenta,
];

/// Width of the status word column, as wide as the longest word.
pub(crate) const VERB_WIDTH: usize = 8;

/// Indent of grouped task output. Nothing else is indented, so the indent
/// alone marks a line as a task's.
const BLOCK_INDENT: &str = "    ";

/// Solid marks a task's standard output, dashed its standard error.
pub(crate) const STDOUT_MARK: &str = "\u{2502}";
pub(crate) const STDERR_MARK: &str = "\u{250a}";

const CYAN: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
    .bold();
const GREEN: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Green)))
    .bold();
const RED: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Red)))
    .bold();
const YELLOW: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Yellow)))
    .bold();
const DIM: Style = Style::new().effects(Effects::DIMMED);

/// What the reporter does with each line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Layout {
    /// Prefix every line, with a spinner for each running task.
    Stream,
    /// Collect a task and print it behind status lines when it ends.
    Grouped,
    /// Relay both streams with no change, for a workflow of one task.
    Plain,
}

/// Which stream a write belongs on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Target {
    /// Relayed task output.
    Out,
    /// Loom's own lines.
    Err,
}

struct Streams {
    stdout: AutoStream<io::Stdout>,
    stderr: AutoStream<io::Stderr>,
}

/// The single writer for everything Loom prints.
///
/// Spinners redraw over whatever else reaches the terminal, so every write
/// goes through here, including diagnostics from sandbox threads. The run log
/// sits here for the same reason.
struct Terminal {
    bars: Option<MultiProgress>,
    streams: Mutex<Streams>,
    log: Mutex<Option<RunLog>>,
    started: Instant,
    timestamps: Option<Timestamps>,
}

/// Renders lifecycle and output events of one run.
pub(crate) struct Reporter {
    layout: Layout,
    ids: Vec<String>,
    width: usize,
    terminal: Arc<Terminal>,
    bars: Option<MultiProgress>,
    spinner: ProgressStyle,
    spinners: Vec<Option<ProgressBar>>,
    buffers: Vec<Vec<u8>>,
    passed: usize,
    failed: usize,
    blocked: usize,
    started: Instant,
}

impl std::fmt::Debug for Reporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Reporter")
            .field("layout", &self.layout)
            .field("ids", &self.ids)
            .finish_non_exhaustive()
    }
}

impl Reporter {
    /// A run of fewer than two tasks needs no label to be readable, so it
    /// relays its streams unchanged.
    pub(crate) fn new(
        target: &RunTarget<'_>,
        mode: OutputMode,
        color: ColorMode,
        timestamps: Option<Timestamps>,
        settings: LogSettings<'_>,
    ) -> Self {
        let ids = target.labels();
        let layout = resolve_layout(mode, ids.len());
        let bars = (layout == Layout::Stream)
            .then(|| MultiProgress::with_draw_target(ProgressDrawTarget::stderr()));
        let choice = color_choice(color);
        let (log, failure) = match settings.create(target) {
            Some(Ok(log)) => (Some(log), None),
            Some(Err(error)) => (None, Some(error)),
            None => (None, None),
        };
        let directory = log.as_ref().map(|log| log.directory().to_path_buf());
        let terminal = Arc::new(Terminal {
            bars: bars.clone(),
            streams: Mutex::new(Streams {
                stdout: AutoStream::new(io::stdout(), choice),
                stderr: AutoStream::new(io::stderr(), choice),
            }),
            log: Mutex::new(log),
            started: Instant::now(),
            timestamps,
        });
        // A plain run relays the bytes of its task, so Loom keeps quiet there.
        if layout != Layout::Plain {
            announce(&terminal, directory.as_deref(), failure);
        }

        Self {
            layout,
            width: ids.iter().map(String::len).max().unwrap_or(0),
            spinners: vec![None; ids.len()],
            buffers: vec![Vec::new(); ids.len()],
            ids,
            terminal,
            bars,
            spinner: spinner_style("{spinner:.dim} {prefix} {msg} {elapsed:.dim}"),
            passed: 0,
            failed: 0,
            blocked: 0,
            started: Instant::now(),
        }
    }

    /// A sink that prints sandbox diagnostics as Loom's own status lines.
    ///
    /// The status word keeps them apart from task output in a log, and the
    /// terminal keeps them off the spinner rows.
    pub(crate) fn diagnostic_sink(&self) -> Box<dyn Fn(&str) + Send + Sync> {
        let terminal = Arc::clone(&self.terminal);
        Box::new(move |message| {
            terminal.record("Sandbox", message);
            let _ = terminal.line(Target::Err, &status_line(YELLOW, "Sandbox", message));
        })
    }

    pub(crate) fn event(&mut self, event: &RunEvent) -> io::Result<()> {
        match event {
            RunEvent::Started { task } => self.started(*task),
            RunEvent::Output { task, stream, line } => self.output(*task, *stream, line),
            RunEvent::Finished {
                task,
                output,
                elapsed,
            } => self.finished(*task, output, *elapsed),
            RunEvent::Failed {
                task,
                error_kind,
                elapsed,
            } => self.failed(*task, *error_kind, *elapsed),
            RunEvent::Blocked { task } => self.blocked(*task),
        }
    }

    /// Prints the closing summary of a decorated run, then writes its record.
    ///
    /// `exit_status` is the status Loom itself returns. It is absent when the
    /// run ended in an execution error.
    pub(crate) fn finish(&mut self, exit_status: Option<i32>) -> io::Result<()> {
        if let Some(bars) = &self.bars {
            let _ = bars.clear();
        }

        let style = if self.failed > 0 { RED } else { GREEN };
        let message = format!(
            "{} in {}",
            counts(self.passed, self.failed, self.blocked),
            seconds(self.started.elapsed())
        );
        let result = self.line(style, "Summary", &message);
        self.terminal.log(|log| log.finish(exit_status));
        result
    }

    fn started(&mut self, task: Option<TaskIndex>) -> io::Result<()> {
        let index = position(task);
        self.terminal.outcome(index, TaskOutcome::Started);
        let id = self.id(index);
        // Stream shows a spinner instead of a line, so only the log gets this.
        self.terminal.record("Running", &id);

        match self.layout {
            Layout::Plain => Ok(()),
            Layout::Grouped => self
                .terminal
                .line(Target::Err, &status_line(CYAN, "Running", &id)),
            Layout::Stream => {
                let prefix = self.prefix(index);
                if let (Some(bars), Some(slot)) = (&self.bars, self.spinners.get_mut(index)) {
                    let bar = bars.add(ProgressBar::new_spinner().with_style(self.spinner.clone()));
                    bar.set_prefix(prefix);
                    bar.set_message("running");
                    // Without a steady tick the spinner moves only when a line arrives.
                    bar.enable_steady_tick(Duration::from_millis(80));
                    *slot = Some(bar);
                }
                Ok(())
            }
        }
    }

    fn output(
        &mut self,
        task: Option<TaskIndex>,
        stream: OutputStream,
        line: &[u8],
    ) -> io::Result<()> {
        let index = position(task);
        self.terminal.log(|log| log.output(index, stream, line));

        match self.layout {
            // Plain relays the captured streams once the task ends.
            Layout::Plain => Ok(()),
            Layout::Grouped => {
                let rendered = self.grouped_line(line);
                if let Some(buffer) = self.buffers.get_mut(index) {
                    buffer.extend_from_slice(rendered.as_bytes());
                }
                Ok(())
            }
            Layout::Stream => {
                // Unstyled, so a task keeps the colours it chose.
                let text = String::from_utf8_lossy(trim_newline(line));
                let separator = match stream {
                    OutputStream::Stdout => STDOUT_MARK,
                    OutputStream::Stderr => STDERR_MARK,
                };
                let line = format!("{} {} {text}", self.prefix(index), paint(DIM, separator));
                self.terminal.line(Target::Out, &line)
            }
        }
    }

    fn finished(
        &mut self,
        task: Option<TaskIndex>,
        output: &ProcessOutput,
        elapsed: Duration,
    ) -> io::Result<()> {
        let index = position(task);
        self.terminal.outcome(
            index,
            TaskOutcome::Finished {
                exit_status: output.status_code(),
                elapsed,
            },
        );
        if output.succeeded() {
            self.passed += 1;
        } else {
            self.failed += 1;
        }

        let (style, verb) = if output.succeeded() {
            (GREEN, "Finished")
        } else {
            (RED, "Failed")
        };
        let message = format!(
            "{} in {} ({})",
            self.id(index),
            seconds(elapsed),
            status_text(output.status_code())
        );

        if self.layout == Layout::Plain {
            self.terminal.record(verb, &message);
            return self.terminal.captured(output);
        }

        self.stop_spinner(index);
        // The status line closes the task, so its output comes first.
        if self.layout == Layout::Grouped {
            let buffer = self.buffers.get(index).cloned().unwrap_or_default();
            self.terminal.bytes(Target::Out, &buffer)?;
        }
        self.line(style, verb, &message)
    }

    fn failed(
        &mut self,
        task: Option<TaskIndex>,
        error_kind: io::ErrorKind,
        elapsed: Duration,
    ) -> io::Result<()> {
        let index = position(task);
        self.terminal.outcome(
            index,
            TaskOutcome::Failed {
                error: error_kind,
                elapsed,
            },
        );
        self.failed += 1;

        let message = format!("{} in {} ({error_kind})", self.id(index), seconds(elapsed));
        if self.layout == Layout::Plain {
            self.terminal.record("Failed", &message);
            return Ok(());
        }
        self.stop_spinner(index);
        self.line(RED, "Failed", &message)
    }

    fn blocked(&mut self, task: TaskIndex) -> io::Result<()> {
        let index = task.position();
        self.terminal.outcome(index, TaskOutcome::Blocked);
        self.blocked += 1;

        let id = self.id(index);
        if self.layout == Layout::Plain {
            self.terminal.record("Blocked", &id);
            return Ok(());
        }
        self.line(YELLOW, "Blocked", &id)
    }

    /// Renders one line for its task's block, with its own newline. A blank
    /// line stays blank, to keep trailing spaces out of a log.
    fn grouped_line(&self, line: &[u8]) -> String {
        if is_blank(line) {
            return "\n".to_owned();
        }
        let text = String::from_utf8_lossy(trim_newline(line));
        format!("{}{BLOCK_INDENT}{text}\n", self.terminal.stamp())
    }

    /// Writes one of Loom's own status lines, to the terminal and the log.
    ///
    /// A plain run keeps its terminal free of Loom's lines, so there the line
    /// reaches the log alone.
    fn line(&self, style: Style, verb: &str, message: &str) -> io::Result<()> {
        self.terminal.record(verb, message);
        if self.layout == Layout::Plain {
            return Ok(());
        }
        self.terminal
            .line(Target::Err, &status_line(style, verb, message))
    }

    fn stop_spinner(&mut self, index: usize) {
        if let Some(slot) = self.spinners.get_mut(index)
            && let Some(bar) = slot.take()
        {
            bar.finish_and_clear();
            if let Some(bars) = &self.bars {
                bars.remove(&bar);
            }
        }
    }

    fn id(&self, index: usize) -> String {
        self.ids
            .get(index)
            .cloned()
            .unwrap_or_else(|| index.to_string())
    }

    fn prefix(&self, index: usize) -> String {
        let width = self.width;
        let color = *PALETTE
            .get(index % PALETTE.len())
            .unwrap_or(&AnsiColor::Cyan);
        let style = Style::new().fg_color(Some(Color::Ansi(color))).bold();
        paint(style, &format!("{:<width$}", self.id(index)))
    }
}

impl Terminal {
    fn line(&self, target: Target, text: &str) -> io::Result<()> {
        let stamp = self.stamp();
        self.write(|streams| {
            let stream = streams.pick(target);
            writeln!(stream, "{stamp}{text}")?;
            stream.flush()
        })
    }

    /// What goes in front of one line, empty when timestamps are off.
    fn stamp(&self) -> String {
        let Some(timestamps) = self.timestamps else {
            return String::new();
        };
        let text = match timestamps {
            Timestamps::DateTime => utc(SystemTime::now()),
            Timestamps::Elapsed => {
                format!(
                    "{:>8}",
                    format!("{:.1}s", self.started.elapsed().as_secs_f64())
                )
            }
        };
        paint(DIM, &text) + " "
    }

    /// Writes bytes with no change.
    fn bytes(&self, target: Target, bytes: &[u8]) -> io::Result<()> {
        self.write(|streams| {
            let stream = streams.pick(target);
            stream.write_all(bytes)?;
            stream.flush()
        })
    }

    /// Relays both captured streams unchanged.
    fn captured(&self, output: &ProcessOutput) -> io::Result<()> {
        self.write(|streams| {
            streams.stdout.write_all(output.stdout())?;
            streams.stdout.flush()?;
            streams.stderr.write_all(output.stderr())?;
            streams.stderr.flush()
        })
    }

    /// Records one of Loom's own status lines in the run log.
    fn record(&self, verb: &str, message: &str) {
        self.log(|log| log.status(verb, message));
    }

    fn outcome(&self, index: usize, outcome: TaskOutcome) {
        if let Ok(mut slot) = self.log.lock()
            && let Some(log) = slot.as_mut()
        {
            log.outcome(index, outcome);
        }
    }

    /// Writes to the run log, and gives the log up after one failure so the
    /// run itself continues.
    fn log(&self, action: impl FnOnce(&mut RunLog) -> io::Result<()>) {
        let Ok(mut slot) = self.log.lock() else {
            return;
        };
        let Some(log) = slot.as_mut() else {
            return;
        };
        let Err(error) = action(log) else {
            return;
        };
        *slot = None;
        drop(slot);
        let _ = self.line(
            Target::Err,
            &status_line(YELLOW, "Warning", &format!("run log: {error}")),
        );
    }

    /// Holds the spinners still, then takes the stream lock, always in this
    /// order, so two threads cannot deadlock against each other.
    fn write(&self, action: impl FnOnce(&mut Streams) -> io::Result<()>) -> io::Result<()> {
        match &self.bars {
            // A hidden MultiProgress drops everything it is given.
            Some(bars) if !bars.is_hidden() => bars.suspend(|| self.locked(action)),
            _ => self.locked(action),
        }
    }

    fn locked(&self, action: impl FnOnce(&mut Streams) -> io::Result<()>) -> io::Result<()> {
        let mut streams = self
            .streams
            .lock()
            .map_err(|_| io::Error::other("output lock is poisoned"))?;
        action(&mut streams)
    }
}

impl Streams {
    fn pick(&mut self, target: Target) -> &mut dyn Write {
        match target {
            Target::Out => &mut self.stdout,
            Target::Err => &mut self.stderr,
        }
    }
}

/// Names the directory of the run log, or why there is none.
fn announce(terminal: &Terminal, directory: Option<&Path>, failure: Option<io::Error>) {
    if let Some(directory) = directory {
        let _ = terminal.line(
            Target::Err,
            &status_line(CYAN, "Logging", &display(directory)),
        );
    }
    if let Some(error) = failure {
        let _ = terminal.line(
            Target::Err,
            &status_line(YELLOW, "Warning", &format!("run log: {error}")),
        );
    }
}

/// A run directory as it is written in a status line.
fn display(directory: &Path) -> String {
    let relative = std::env::current_dir()
        .ok()
        .and_then(|working_directory| {
            directory
                .strip_prefix(working_directory)
                .ok()
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| directory.to_path_buf());

    relative.display().to_string()
}

/// The task an event belongs to. A direct request has one task of its own.
fn position(task: Option<TaskIndex>) -> usize {
    task.map_or(0, TaskIndex::position)
}

/// The counts that close a run.
fn counts(passed: usize, failed: usize, blocked: usize) -> String {
    let mut counts = vec![format!("{passed} passed")];
    if failed > 0 {
        counts.push(format!("{failed} failed"));
    }
    if blocked > 0 {
        counts.push(format!("{blocked} blocked"));
    }
    counts.join(", ")
}

/// How a finished task ended, in brackets at the end of its line.
fn status_text(exit_status: Option<i32>) -> String {
    match exit_status {
        Some(0) => "ok".to_owned(),
        Some(code) => format!("exit {code}"),
        None => "signalled".to_owned(),
    }
}

fn is_blank(line: &[u8]) -> bool {
    line.iter()
        .all(|byte| matches!(byte, b'\n' | b'\r' | b' ' | b'\t'))
}

/// Formats one of Loom's own lines, with the status word in its own column.
fn status_line(style: Style, verb: &str, message: &str) -> String {
    // A task can leave its own styling on, so start from a known state.
    let verb = paint(style, &format!("{verb:<VERB_WIDTH$}"));
    format!("{}{verb}  {message}", anstyle::Reset)
}

fn spinner_style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template)
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_strings(&[
            "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}",
            "\u{2827}", "\u{2807}", "\u{280f}", "\u{2713}",
        ])
}

fn resolve_layout(mode: OutputMode, tasks: usize) -> Layout {
    // One task needs no label, so its streams pass through untouched.
    if tasks < 2 {
        return Layout::Plain;
    }
    match mode {
        OutputMode::Stream => Layout::Stream,
        OutputMode::Grouped => Layout::Grouped,
    }
}

fn color_choice(color: ColorMode) -> anstream::ColorChoice {
    match color {
        ColorMode::Auto => anstream::ColorChoice::global(),
        ColorMode::Always => anstream::ColorChoice::Always,
        ColorMode::Never => anstream::ColorChoice::Never,
    }
}

pub(crate) fn trim_newline(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn seconds(elapsed: Duration) -> String {
    format!("{:.1}s", elapsed.as_secs_f64())
}

fn paint(style: Style, text: &str) -> String {
    format!("{style}{text}{style:#}")
}

#[cfg(test)]
mod tests {
    use super::{counts, status_text};

    #[test]
    fn names_only_the_counts_a_run_reached() {
        assert_eq!(counts(2, 0, 0), "2 passed");
        assert_eq!(counts(0, 1, 1), "0 passed, 1 failed, 1 blocked");
    }

    #[test]
    fn names_how_a_task_ended() {
        assert_eq!(status_text(Some(0)), "ok");
        assert_eq!(status_text(Some(23)), "exit 23");
        assert_eq!(status_text(None), "signalled");
    }
}
