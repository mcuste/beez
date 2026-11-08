//! Renders run events for a terminal or a log.

use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anstream::AutoStream;
use anstyle::{AnsiColor, Color, Effects, Style};
use beez_process::{OutputStream, ProcessOutput};
use beez_record::{
    Labels, LogSettings, OpenLog, RunRecorder, RunTally, RunTarget, TaskOutcome, TaskState,
    VERB_WIDTH, seconds, stream_mark, trim_newline, utc,
};
use beez_runner::RunEvent;
use clap::{Args, ValueEnum};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

/// How Beez renders the output of workflow tasks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum OutputMode {
    /// Print every line as it arrives, behind the task ID.
    #[default]
    Stream,
    /// Collect each task and print it when the task ends.
    Grouped,
}

/// What Beez puts in front of every line it writes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum Timestamps {
    /// Date and time in UTC.
    #[default]
    DateTime,
    /// Seconds since the run started.
    Elapsed,
}

/// When Beez colours its own output.
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

/// How Beez draws a run on the terminal.
///
/// The default draws the way a direct request does: streamed, coloured on a
/// terminal, without timestamps.
#[derive(Args, Clone, Copy, Debug, Default)]
pub(crate) struct RenderOptions {
    /// How to render the output of the tasks.
    #[arg(long, value_enum, default_value_t)]
    output: OutputMode,
    /// When to colour Beez's own output.
    #[arg(long, value_enum, default_value_t)]
    color: ColorMode,
    /// Prefix every line Beez writes with a timestamp.
    ///
    /// A value needs an equals sign, so the bare flag cannot take the path of
    /// the manifest as its value.
    #[arg(
        long,
        value_enum,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "date-time"
    )]
    timestamps: Option<Timestamps>,
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

/// What the reporter does with each line, and what it keeps to do it.
enum Layout {
    /// Prefix every line, with a spinner for each running task.
    Stream {
        /// The spinner row of each task, while the task runs.
        spinners: Vec<Option<ProgressBar>>,
        /// How every spinner row is drawn.
        style: ProgressStyle,
    },
    /// Collect a task and print it behind status lines when it ends.
    Grouped {
        /// The rendered output of each task, until the task ends.
        buffers: Vec<Vec<u8>>,
    },
    /// Relay both streams with no change, for a workflow of one task.
    Plain,
}

impl Layout {
    fn name(&self) -> &'static str {
        match self {
            Self::Stream { .. } => "Stream",
            Self::Grouped { .. } => "Grouped",
            Self::Plain => "Plain",
        }
    }
}

/// Which stream a write belongs on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Target {
    /// Relayed task output.
    Out,
    /// Beez's own lines.
    Err,
}

struct Streams {
    stdout: AutoStream<io::Stdout>,
    stderr: AutoStream<io::Stderr>,
}

/// The single writer for everything Beez prints.
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
    labels: Labels,
    terminal: Arc<Terminal>,
    tally: RunTally,
}

impl std::fmt::Debug for Reporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Reporter")
            .field("layout", &self.layout.name())
            .field("labels", &self.labels)
            .finish_non_exhaustive()
    }
}

impl Reporter {
    /// A run of fewer than two tasks needs no label to be readable, so it
    /// relays its streams unchanged.
    pub(crate) fn new(
        target: &RunTarget<'_>,
        render: RenderOptions,
        settings: LogSettings<'_>,
        working_directory: &Path,
    ) -> Self {
        let labels = target.labels();
        let layout = resolve_layout(render.output, labels.len());
        let bars = matches!(layout, Layout::Stream { .. })
            .then(|| MultiProgress::with_draw_target(ProgressDrawTarget::stderr()));
        let choice = color_choice(render.color);
        let (log, failure) = OpenLog::open(settings, target, working_directory);
        let directory = log.directory().map(Path::to_path_buf);
        let terminal = Arc::new(Terminal {
            bars,
            streams: Mutex::new(Streams {
                stdout: AutoStream::new(io::stdout(), choice),
                stderr: AutoStream::new(io::stderr(), choice),
            }),
            log: Mutex::new(log),
            started: Instant::now(),
            timestamps: render.timestamps,
        });
        // A plain run relays the bytes of its task, so it names no run
        // directory. A log that could not open is still worth a warning.
        let announced = directory.filter(|_| !matches!(layout, Layout::Plain));
        announce(&terminal, announced.as_deref(), failure, working_directory);

        Self {
            layout,
            labels,
            terminal,
            tally: RunTally::default(),
        }
    }

    /// A sink that prints sandbox diagnostics as Beez's own status lines.
    ///
    /// The status word keeps them apart from task output in a log, and the
    /// terminal keeps them off the spinner rows.
    pub(crate) fn diagnostic_sink(&self) -> Box<dyn Fn(&str) + Send + Sync> {
        let terminal = Arc::clone(&self.terminal);
        Box::new(move |message| {
            terminal.record("Sandbox", message);
            // Shown in a plain run too: the line goes to standard error, so
            // the task's standard output keeps its exact bytes, and a denied
            // connection must never pass in silence.
            let _ = terminal.line(Target::Err, &styled_status(YELLOW, "Sandbox", message));
        })
    }

    pub(crate) fn event(&mut self, event: &RunEvent) -> io::Result<()> {
        self.terminal.log(|log| log.event(event));
        let index = event.position();
        self.tally.record(event);

        if let RunEvent::Output { stream, line, .. } = event {
            return self.output(index, *stream, line);
        }
        match &mut self.layout {
            Layout::Plain => {
                // A plain run relays the captured streams of its one task,
                // and keeps its status lines to the log.
                return match event {
                    RunEvent::Finished { output, .. } => self.terminal.captured(output),
                    _ => Ok(()),
                };
            }
            // A stream layout shows a running task as a spinner row, not a line.
            Layout::Stream { spinners, style } => {
                if matches!(event, RunEvent::Started { .. }) {
                    let prefix = prefix(&self.labels, index);
                    start_spinner(&self.terminal, spinners, style, index, prefix);
                    return Ok(());
                }
                // Every other event ends the task, so its spinner goes.
                stop_spinner(&self.terminal, spinners, index);
            }
            // The status line closes the task, so its collected output comes
            // first. A task finishes once, so its buffer goes with it.
            Layout::Grouped { buffers } => {
                if matches!(event, RunEvent::Finished { .. }) {
                    let buffer = buffers.get_mut(index).map(std::mem::take);
                    self.terminal
                        .bytes(Target::Out, &buffer.unwrap_or_default())?;
                }
            }
        }

        let Some((state, message)) =
            TaskOutcome::of(event).map(|outcome| outcome.status(&self.labels.get(index)))
        else {
            return Ok(());
        };
        // The event wrote this line to the run log already.
        self.show(style_of(state), state.verb(), &message)
    }

    /// Prints the closing summary of a decorated run, then writes its record.
    ///
    /// `exit_status` is the status Beez itself returns. It is absent when the
    /// run ended in an execution error.
    pub(crate) fn finish(&mut self, exit_status: Option<i32>) -> io::Result<()> {
        if let Some(bars) = &self.terminal.bars {
            let _ = bars.clear();
        }

        let style = if self.tally.failed() > 0 { RED } else { GREEN };
        let message = self.tally.summary(self.terminal.started.elapsed());
        self.terminal.log(|log| log.finish(&message, exit_status));

        self.show(style, "Summary", &message)
    }

    fn output(&mut self, index: usize, stream: OutputStream, line: &[u8]) -> io::Result<()> {
        match &mut self.layout {
            // Plain relays the captured streams once the task ends.
            Layout::Plain => Ok(()),
            Layout::Grouped { buffers } => {
                let rendered = grouped_line(&self.terminal, line);
                if let Some(buffer) = buffers.get_mut(index) {
                    buffer.extend_from_slice(rendered.as_bytes());
                }
                Ok(())
            }
            Layout::Stream { .. } => {
                // Unstyled, so a task keeps the colours it chose.
                let text = String::from_utf8_lossy(trim_newline(line));
                let line = format!(
                    "{} {} {text}",
                    prefix(&self.labels, index),
                    paint(DIM, stream_mark(stream))
                );
                self.terminal.line(Target::Out, &line)
            }
        }
    }

    /// Shows one status line on the terminal, for a line the log already holds.
    ///
    /// A plain run keeps its terminal free of Beez's lines, so there the line
    /// reaches the log alone.
    fn show(&self, style: Style, verb: &str, message: &str) -> io::Result<()> {
        if matches!(self.layout, Layout::Plain) {
            return Ok(());
        }

        self.terminal
            .line(Target::Err, &styled_status(style, verb, message))
    }
}

/// Shows one running task as a spinner row of its own.
fn start_spinner(
    terminal: &Terminal,
    spinners: &mut [Option<ProgressBar>],
    style: &ProgressStyle,
    index: usize,
    prefix: String,
) {
    if let (Some(bars), Some(slot)) = (&terminal.bars, spinners.get_mut(index)) {
        let bar = bars.add(ProgressBar::new_spinner().with_style(style.clone()));
        bar.set_prefix(prefix);
        bar.set_message("running");
        // Without a steady tick the spinner moves only when a line arrives.
        bar.enable_steady_tick(Duration::from_millis(80));
        *slot = Some(bar);
    }
}

fn stop_spinner(terminal: &Terminal, spinners: &mut [Option<ProgressBar>], index: usize) {
    if let Some(slot) = spinners.get_mut(index)
        && let Some(bar) = slot.take()
    {
        bar.finish_and_clear();
        if let Some(bars) = &terminal.bars {
            bars.remove(&bar);
        }
    }
}

/// Renders one line for its task's block, with its own newline. A blank
/// line stays blank, to keep trailing spaces out of a log.
fn grouped_line(terminal: &Terminal, line: &[u8]) -> String {
    if is_blank(line) {
        return "\n".to_owned();
    }
    let text = String::from_utf8_lossy(trim_newline(line));
    format!("{}{BLOCK_INDENT}{text}\n", terminal.stamp())
}

/// The label of one task, padded to the widest and in the task's colour.
fn prefix(labels: &Labels, index: usize) -> String {
    let width = labels.width();
    let color = *PALETTE
        .get(index % PALETTE.len())
        .unwrap_or(&AnsiColor::Cyan);
    let style = Style::new().fg_color(Some(Color::Ansi(color))).bold();
    paint(style, &format!("{:<width$}", labels.get(index)))
}

impl Terminal {
    fn line(&self, target: Target, text: &str) -> io::Result<()> {
        let stamp = self.stamp();
        self.write_to(target, |stream| writeln!(stream, "{stamp}{text}"))
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
        self.write_to(target, |stream| stream.write_all(bytes))
    }

    /// Writes to one stream and flushes it.
    fn write_to(
        &self,
        target: Target,
        action: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    ) -> io::Result<()> {
        self.write(|streams| {
            let stream = streams.pick(target);
            action(stream)?;
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

    /// Records one of Beez's own status lines in the run log.
    fn record(&self, verb: &str, message: &str) {
        self.log(|log| log.status(verb, message));
    }

    /// Writes to the run log, and warns once on its first failed write.
    fn log(&self, action: impl FnOnce(&mut RunRecorder) -> io::Result<()>) {
        let Ok(mut log) = self.log.lock() else {
            return;
        };
        let failure = log.write(action);
        // The warning writes a line of its own, so the log lock goes first.
        drop(log);
        if let Some(failure) = failure {
            self.warn(&failure);
        }
    }

    /// Says why the run log could not be written.
    fn warn(&self, failure: &str) {
        let _ = self.line(Target::Err, &styled_status(YELLOW, "Warning", failure));
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
fn announce(
    terminal: &Terminal,
    directory: Option<&Path>,
    failure: Option<String>,
    working_directory: &Path,
) {
    if let Some(directory) = directory {
        let _ = terminal.line(
            Target::Err,
            &styled_status(CYAN, "Logging", &display(directory, working_directory)),
        );
    }
    if let Some(failure) = failure {
        terminal.warn(&failure);
    }
}

/// A run directory as it is written in a status line, relative to the
/// working directory when it lies inside it.
fn display(directory: &Path, working_directory: &Path) -> String {
    directory
        .strip_prefix(working_directory)
        .unwrap_or(directory)
        .display()
        .to_string()
}

fn is_blank(line: &[u8]) -> bool {
    line.iter()
        .all(|byte| matches!(byte, b'\n' | b'\r' | b' ' | b'\t'))
}

/// The colour of the status word for one task state.
fn style_of(state: TaskState) -> Style {
    match state {
        TaskState::Finished => GREEN,
        TaskState::Failed => RED,
        TaskState::Blocked => YELLOW,
        TaskState::Pending | TaskState::Running => CYAN,
    }
}

/// Formats one of Beez's own lines, with the status word in its own column.
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
        OutputMode::Stream => Layout::Stream {
            spinners: vec![None; tasks],
            style: spinner_style("{spinner:.dim} {prefix} {msg} {elapsed:.dim}"),
        },
        OutputMode::Grouped => Layout::Grouped {
            buffers: vec![Vec::new(); tasks],
        },
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
