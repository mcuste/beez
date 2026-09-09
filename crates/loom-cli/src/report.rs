//! Renders run events for a terminal or a log.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anstream::AutoStream;
use anstyle::{AnsiColor, Color, Effects, Style};
use clap::ValueEnum;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use loom_core::{TaskIndex, Workflow};
use loom_process::{OutputStream, ProcessOutput};
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

/// Width of the status word column, as wide as the longest word.
const VERB_WIDTH: usize = 8;

/// Indent of grouped task output. Nothing else is indented, so the indent
/// alone marks a line as a task's.
const BLOCK_INDENT: &str = "    ";

/// Solid marks a task's standard output, dashed its standard error.
const STDOUT_MARK: &str = "\u{2502}";
const STDERR_MARK: &str = "\u{250a}";

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
/// goes through here, including diagnostics from sandbox threads.
struct Terminal {
    bars: Option<MultiProgress>,
    streams: Mutex<Streams>,
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
        workflow: Option<&Workflow>,
        mode: OutputMode,
        color: ColorMode,
        timestamps: Option<Timestamps>,
    ) -> Self {
        let ids: Vec<String> = workflow
            .map(|workflow| {
                workflow
                    .tasks()
                    .iter()
                    .map(|task| task.id().as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let layout = resolve_layout(mode, ids.len());
        let bars = (layout == Layout::Stream)
            .then(|| MultiProgress::with_draw_target(ProgressDrawTarget::stderr()));
        let choice = color_choice(color);

        Self {
            layout,
            width: ids.iter().map(String::len).max().unwrap_or(0),
            spinners: vec![None; ids.len()],
            buffers: vec![Vec::new(); ids.len()],
            ids,
            terminal: Arc::new(Terminal {
                bars: bars.clone(),
                streams: Mutex::new(Streams {
                    stdout: AutoStream::new(io::stdout(), choice),
                    stderr: AutoStream::new(io::stderr(), choice),
                }),
                started: Instant::now(),
                timestamps,
            }),
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

    /// Prints the closing summary of a decorated run.
    pub(crate) fn finish(&mut self) -> io::Result<()> {
        if let Some(bars) = &self.bars {
            let _ = bars.clear();
        }
        if self.layout == Layout::Plain {
            return Ok(());
        }

        let mut counts = vec![format!("{} passed", self.passed)];
        if self.failed > 0 {
            counts.push(format!("{} failed", self.failed));
        }
        if self.blocked > 0 {
            counts.push(format!("{} blocked", self.blocked));
        }
        let style = if self.failed > 0 { RED } else { GREEN };
        let counts = counts.join(", ");

        self.status(
            style,
            "Summary",
            &format!("{counts} in {}", seconds(self.started.elapsed())),
        )
    }

    fn started(&mut self, task: Option<TaskIndex>) -> io::Result<()> {
        let Some(index) = task.map(TaskIndex::position) else {
            return Ok(());
        };
        match self.layout {
            Layout::Plain => Ok(()),
            Layout::Grouped => {
                let id = self.id(index);
                self.status(CYAN, "Running", &id)
            }
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
        let Some(index) = task.map(TaskIndex::position) else {
            return Ok(());
        };
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
        if output.succeeded() {
            self.passed += 1;
        } else {
            self.failed += 1;
        }
        let Some(index) = task.map(TaskIndex::position) else {
            return self.terminal.captured(output);
        };
        let status = match output.status_code() {
            Some(0) => "ok".to_owned(),
            Some(code) => format!("exit {code}"),
            None => "signalled".to_owned(),
        };

        if self.layout == Layout::Plain {
            return self.terminal.captured(output);
        }

        self.stop_spinner(index);
        // The status line closes the task, so its output comes first.
        if self.layout == Layout::Grouped {
            let buffer = self.buffers.get(index).cloned().unwrap_or_default();
            self.terminal.bytes(Target::Out, &buffer)?;
        }
        let id = self.id(index);
        let (style, verb) = if output.succeeded() {
            (GREEN, "Finished")
        } else {
            (RED, "Failed")
        };
        self.status(
            style,
            verb,
            &format!("{id} in {} ({status})", seconds(elapsed)),
        )
    }

    fn failed(
        &mut self,
        task: Option<TaskIndex>,
        error_kind: io::ErrorKind,
        elapsed: Duration,
    ) -> io::Result<()> {
        self.failed += 1;
        let Some(index) = task.map(TaskIndex::position) else {
            return Ok(());
        };
        self.stop_spinner(index);
        if self.layout == Layout::Plain {
            return Ok(());
        }
        let id = self.id(index);
        self.status(
            RED,
            "Failed",
            &format!("{id} in {} ({error_kind})", seconds(elapsed)),
        )
    }

    fn blocked(&mut self, task: TaskIndex) -> io::Result<()> {
        self.blocked += 1;
        if self.layout == Layout::Plain {
            return Ok(());
        }
        let id = self.id(task.position());
        self.status(YELLOW, "Blocked", &id)
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

    /// Writes one of Loom's own status lines.
    fn status(&self, style: Style, verb: &str, message: &str) -> io::Result<()> {
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

fn is_blank(line: &[u8]) -> bool {
    line.iter()
        .all(|byte| matches!(byte, b'\n' | b'\r' | b' ' | b'\t'))
}

/// Formats `time` as a UTC date and time, to the millisecond.
///
/// Local time needs the time zone database, so Loom reports UTC and marks it
/// with the `Z`.
fn utc(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since_epoch.as_secs();
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let day_seconds = seconds.rem_euclid(86_400);
    let hour = day_seconds.div_euclid(3_600);
    let minute = day_seconds.rem_euclid(3_600).div_euclid(60);
    let second = day_seconds.rem_euclid(60);
    let millis = since_epoch.subsec_millis();

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Splits days since 1970-01-01 into a year, a month and a day.
///
/// This is Howard Hinnant's civil-from-days algorithm, for days at or after
/// the epoch only.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era = (day_of_era - day_of_era.div_euclid(1_460) + day_of_era.div_euclid(36_524)
        - day_of_era.div_euclid(146_096))
    .div_euclid(365);
    let year = year_of_era + era * 400;
    let day_of_year =
        day_of_era - (365 * year_of_era + year_of_era.div_euclid(4) - year_of_era.div_euclid(100));
    // March is month zero, so January and February belong to the year after.
    let shifted_month = (5 * day_of_year + 2).div_euclid(153);
    let day = day_of_year - (153 * shifted_month + 2).div_euclid(5) + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };

    (if month <= 2 { year + 1 } else { year }, month, day)
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

fn trim_newline(line: &[u8]) -> &[u8] {
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
    use super::{civil_from_days, utc};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn splits_days_into_a_civil_date() {
        let expected = [
            (0, (1970, 1, 1)),
            (19_675, (2023, 11, 14)),
            (20_453, (2025, 12, 31)),
            // Leap days, including the year 2000 century rule.
            (11_016, (2000, 2, 29)),
            (12_477, (2004, 2, 29)),
            (47_482, (2100, 1, 1)),
        ];

        for (days, date) in expected {
            assert_eq!(civil_from_days(days), date, "{days} days");
        }
    }

    #[test]
    fn formats_a_utc_timestamp() {
        let time = UNIX_EPOCH + Duration::from_millis(1_700_000_000_042);

        assert_eq!(utc(time), "2023-11-14T22:13:20.042Z");
    }

    #[test]
    fn formats_the_last_second_of_a_year() {
        let time = UNIX_EPOCH + Duration::from_secs(1_767_225_599);

        assert_eq!(utc(time), "2025-12-31T23:59:59.000Z");
    }
}
