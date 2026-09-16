//! Renders run events for a terminal or a log.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anstream::AutoStream;
use anstyle::{AnsiColor, Color, Effects, Style};
use clap::ValueEnum;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use loom_process::{OutputStream, ProcessOutput};
use loom_record::{
    LogSettings, OpenLog, RunRecorder, RunTally, RunTarget, STDERR_MARK, STDOUT_MARK, VERB_WIDTH,
    event_status, seconds, trim_newline, utc,
};
use loom_runner::RunEvent;

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

/// Indent of grouped task output. Nothing else is indented, so the indent
/// alone marks a line as a task's.
const BLOCK_INDENT: &str = "    ";

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
    log: Mutex<OpenLog>,
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
    tally: RunTally,
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
        working_directory: &Path,
    ) -> Self {
        let ids = target.labels();
        let layout = resolve_layout(mode, ids.len());
        let bars = (layout == Layout::Stream)
            .then(|| MultiProgress::with_draw_target(ProgressDrawTarget::stderr()));
        let choice = color_choice(color);
        let (log, failure) = OpenLog::open(settings, target, working_directory);
        let directory = log.directory().map(Path::to_path_buf);
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
            tally: RunTally::default(),
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
            let _ = terminal.line(Target::Err, &styled_status(YELLOW, "Sandbox", message));
        })
    }

    pub(crate) fn event(&mut self, event: &RunEvent) -> io::Result<()> {
        self.terminal.log(|log| log.event(event));
        self.tally.add(event);
        let index = event.position();

        match event {
            RunEvent::Output { stream, line, .. } => return self.output(index, *stream, line),
            // A plain run relays the captured streams of its one task instead.
            RunEvent::Finished { output, .. } if self.layout == Layout::Plain => {
                return self.terminal.captured(output);
            }
            // A stream layout shows a running task as a spinner row, not a line.
            RunEvent::Started { .. } if self.layout == Layout::Stream => {
                self.start_spinner(index);
                return Ok(());
            }
            RunEvent::Started { .. } => {}
            // Every other event ends the task, so its spinner goes.
            _ => self.stop_spinner(index),
        }
        // The status line closes the task, so its collected output comes first.
        if self.layout == Layout::Grouped && matches!(event, RunEvent::Finished { .. }) {
            let buffer = self.buffers.get(index).cloned().unwrap_or_default();
            self.terminal.bytes(Target::Out, &buffer)?;
        }

        let Some((verb, message)) = event_status(event, &self.id(index)) else {
            return Ok(());
        };
        // The event wrote this line to the run log already.
        self.show(style_of(verb), verb, &message)
    }

    /// Prints the closing summary of a decorated run, then writes its record.
    ///
    /// `exit_status` is the status Loom itself returns. It is absent when the
    /// run ended in an execution error.
    pub(crate) fn finish(&mut self, exit_status: Option<i32>) -> io::Result<()> {
        if let Some(bars) = &self.bars {
            let _ = bars.clear();
        }

        let style = if self.tally.failed() > 0 { RED } else { GREEN };
        let message = self.tally.summary(self.started.elapsed());
        let result = self.line(style, "Summary", &message);
        self.terminal.log(|log| log.finish(exit_status));
        result
    }

    /// Shows one running task as a spinner row of its own.
    fn start_spinner(&mut self, index: usize) {
        let prefix = self.prefix(index);
        if let (Some(bars), Some(slot)) = (&self.bars, self.spinners.get_mut(index)) {
            let bar = bars.add(ProgressBar::new_spinner().with_style(self.spinner.clone()));
            bar.set_prefix(prefix);
            bar.set_message("running");
            // Without a steady tick the spinner moves only when a line arrives.
            bar.enable_steady_tick(Duration::from_millis(80));
            *slot = Some(bar);
        }
    }

    fn output(&mut self, index: usize, stream: OutputStream, line: &[u8]) -> io::Result<()> {
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
    fn line(&self, style: Style, verb: &str, message: &str) -> io::Result<()> {
        self.terminal.record(verb, message);

        self.show(style, verb, message)
    }

    /// Shows one status line on the terminal, for a line the log already holds.
    ///
    /// A plain run keeps its terminal free of Loom's lines, so there the line
    /// reaches the log alone.
    fn show(&self, style: Style, verb: &str, message: &str) -> io::Result<()> {
        if self.layout == Layout::Plain {
            return Ok(());
        }

        self.terminal
            .line(Target::Err, &styled_status(style, verb, message))
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
            Timestamps::Elapsed => format!("{:>8}", seconds(self.started.elapsed())),
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

    /// Writes to the run log, and warns once when the log closes.
    fn log(&self, action: impl FnOnce(&mut RunRecorder) -> io::Result<()>) {
        let Ok(mut log) = self.log.lock() else {
            return;
        };
        let failure = log.write(action);
        // The warning writes a line of its own, so the log lock goes first.
        drop(log);
        if let Some(error) = failure {
            let _ = self.line(
                Target::Err,
                &styled_status(YELLOW, "Warning", &format!("run log: {error}")),
            );
        }
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
            &styled_status(CYAN, "Logging", &display(directory)),
        );
    }
    if let Some(error) = failure {
        let _ = terminal.line(
            Target::Err,
            &styled_status(YELLOW, "Warning", &format!("run log: {error}")),
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

fn is_blank(line: &[u8]) -> bool {
    line.iter()
        .all(|byte| matches!(byte, b'\n' | b'\r' | b' ' | b'\t'))
}

/// The colour of one status word an event carries.
fn style_of(verb: &str) -> Style {
    match verb {
        "Finished" => GREEN,
        "Failed" => RED,
        "Blocked" => YELLOW,
        _ => CYAN,
    }
}

/// Formats one of Loom's own lines, with the status word in its own column.
fn styled_status(style: Style, verb: &str, message: &str) -> String {
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

fn paint(style: Style, text: &str) -> String {
    format!("{style}{text}{style:#}")
}
