//! Renders run events for a terminal or a log.

use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use anstream::AutoStream;
use anstyle::{AnsiColor, Color, Effects, Style};
use beez_process::ProcessOutput;
use beez_record::{
    Labels, LogFailure, LogSettings, OpenLog, RunTarget, TaskOutcome, TaskState, VERB_WIDTH,
    seconds, stream_mark, trim_newline, utc,
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
    Stream(Spinners),
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
            Self::Stream(_) => "Stream",
            Self::Grouped { .. } => "Grouped",
            Self::Plain => "Plain",
        }
    }
}

/// One spinner row for each running task.
struct Spinners {
    bars: MultiProgress,
    /// The spinner row of each task, while the task runs.
    slots: Vec<Option<ProgressBar>>,
    /// How every spinner row is drawn.
    style: ProgressStyle,
}

impl Spinners {
    fn new(tasks: usize) -> Self {
        Self {
            bars: MultiProgress::with_draw_target(ProgressDrawTarget::stderr()),
            slots: vec![None; tasks],
            style: spinner_style("{spinner:.dim} {prefix} {msg} {elapsed:.dim}"),
        }
    }

    /// Shows one running task as a spinner row of its own.
    fn start(&mut self, index: usize, prefix: String) {
        if let Some(slot) = self.slots.get_mut(index) {
            let bar = self
                .bars
                .add(ProgressBar::new_spinner().with_style(self.style.clone()));
            bar.set_prefix(prefix);
            bar.set_message("running");
            // Without a steady tick the spinner moves only when a line arrives.
            bar.enable_steady_tick(Duration::from_millis(80));
            *slot = Some(bar);
        }
    }

    fn stop(&mut self, index: usize) {
        if let Some(bar) = self.slots.get_mut(index).and_then(Option::take) {
            bar.finish_and_clear();
            self.bars.remove(&bar);
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
    /// The spinner rows of a stream layout, held still around every write.
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
        let bars = match &layout {
            Layout::Stream(spinners) => {
                crate::trace::hold_still(&spinners.bars);
                Some(spinners.bars.clone())
            }
            Layout::Grouped { .. } | Layout::Plain => None,
        };
        let choice = render.color.into();
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

    pub(crate) fn event(&mut self, event: &RunEvent<'_>) -> io::Result<()> {
        self.terminal.log(|log| log.event(event));
        let index = event.position();

        match &mut self.layout {
            Layout::Plain => plain_event(&self.terminal, event),
            Layout::Stream(spinners) => {
                stream_event(&self.terminal, &self.labels, spinners, index, event)
            }
            Layout::Grouped { buffers } => {
                grouped_event(&self.terminal, &self.labels, buffers, index, event)
            }
        }
    }

    /// Prints the closing summary of a decorated run, then writes its record.
    ///
    /// `exit_status` is the status Beez itself returns. It is absent when the
    /// run ended in an execution error.
    pub(crate) fn finish(&mut self, exit_status: Option<i32>) -> io::Result<()> {
        if let Some(bars) = &self.terminal.bars {
            let _ = bars.clear();
        }

        let (failed, message) = self.terminal.finish_log(exit_status);
        let style = if failed > 0 { RED } else { GREEN };

        // A plain run keeps its terminal free of Beez's lines, so there the
        // line reaches the log alone.
        if matches!(self.layout, Layout::Plain) {
            return Ok(());
        }
        self.terminal
            .line(Target::Err, &styled_status(style, "Summary", &message))
    }
}

/// A plain run relays the captured streams of its one task once it ends,
/// and keeps its status lines to the log.
fn plain_event(terminal: &Terminal, event: &RunEvent<'_>) -> io::Result<()> {
    match event {
        RunEvent::Finished { output, .. } => terminal.captured(output),
        _ => Ok(()),
    }
}

/// Prefixes every line, and shows a running task as a spinner row, not a line.
fn stream_event(
    terminal: &Terminal,
    labels: &Labels,
    spinners: &mut Spinners,
    index: usize,
    event: &RunEvent<'_>,
) -> io::Result<()> {
    match event {
        RunEvent::Output { stream, line, .. } => {
            // Unstyled, so a task keeps the colours it chose.
            let text = String::from_utf8_lossy(trim_newline(line));
            let line = format!(
                "{} {} {text}",
                prefix(labels, index),
                paint(DIM, stream_mark(*stream))
            );
            terminal.line(Target::Out, &line)
        }
        RunEvent::Started { .. } => {
            spinners.start(index, prefix(labels, index));
            Ok(())
        }
        // Every other event ends the task, so its spinner goes.
        _ => {
            spinners.stop(index);
            status_line(terminal, labels, index, event)
        }
    }
}

/// Collects a task's lines, and prints them before the status line that
/// closes the task. A task finishes once, so its buffer goes with it.
fn grouped_event(
    terminal: &Terminal,
    labels: &Labels,
    buffers: &mut [Vec<u8>],
    index: usize,
    event: &RunEvent<'_>,
) -> io::Result<()> {
    match event {
        RunEvent::Output { line, .. } => {
            let rendered = grouped_line(terminal, line);
            if let Some(buffer) = buffers.get_mut(index) {
                buffer.extend_from_slice(rendered.as_bytes());
            }
            Ok(())
        }
        RunEvent::Finished { .. } => {
            let buffer = buffers.get_mut(index).map(std::mem::take);
            terminal.bytes(Target::Out, &buffer.unwrap_or_default())?;
            status_line(terminal, labels, index, event)
        }
        _ => status_line(terminal, labels, index, event),
    }
}

/// Shows the status line of an event that changes its task's state.
///
/// The event wrote this line to the run log already.
fn status_line(
    terminal: &Terminal,
    labels: &Labels,
    index: usize,
    event: &RunEvent<'_>,
) -> io::Result<()> {
    let Some((state, message)) =
        TaskOutcome::of(event).map(|outcome| outcome.status(&labels.get(index)))
    else {
        return Ok(());
    };
    terminal.line(
        Target::Err,
        &styled_status(style_of(state), state.verb(), &message),
    )
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
        self.log(|log| log.write(|recorder| recorder.status(verb, message)));
    }

    /// Writes to the run log, and warns once on its first failed write.
    fn log(&self, action: impl FnOnce(&mut OpenLog) -> Option<LogFailure>) {
        let Ok(mut log) = self.log.lock() else {
            return;
        };
        let failure = action(&mut log);
        // The warning writes a line of its own, so the log lock goes first.
        drop(log);
        if let Some(failure) = failure {
            self.warn(&failure);
        }
    }

    /// Closes the run log, and returns how many tasks failed and the
    /// summary line.
    fn finish_log(&self, exit_status: Option<i32>) -> (usize, String) {
        let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        let failed = log.failed();
        let (summary, failure) = log.finish(self.started.elapsed(), exit_status);
        // The warning writes a line of its own, so the log lock goes first.
        drop(log);
        if let Some(failure) = failure {
            self.warn(&failure);
        }
        (failed, summary)
    }

    /// Says why the run log could not be written.
    fn warn(&self, failure: &LogFailure) {
        let _ = self.line(
            Target::Err,
            &styled_status(YELLOW, "Warning", &failure.to_string()),
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
        // A writer that panicked leaves at most part of a line behind, so the
        // streams stay usable.
        let mut streams = self.streams.lock().unwrap_or_else(PoisonError::into_inner);
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
    failure: Option<LogFailure>,
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
        OutputMode::Stream => Layout::Stream(Spinners::new(tasks)),
        OutputMode::Grouped => Layout::Grouped {
            buffers: vec![Vec::new(); tasks],
        },
    }
}

impl From<ColorMode> for anstream::ColorChoice {
    fn from(color: ColorMode) -> Self {
        match color {
            ColorMode::Auto => Self::global(),
            ColorMode::Always => Self::Always,
            ColorMode::Never => Self::Never,
        }
    }
}

fn paint(style: Style, text: &str) -> String {
    format!("{style}{text}{style:#}")
}
