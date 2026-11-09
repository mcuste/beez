use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, PoisonError, mpsc};
use std::thread::{self, Scope, ScopedJoinHandle};

use beez_policy::SandboxPolicy;
use beez_sandbox::{SandboxError, SandboxedCommand};

use crate::execution::ExecutionRequest;

/// Reports why a request could not run to the end.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// The sandbox around the program could not be prepared.
    #[error("cannot prepare the sandbox")]
    Sandbox(#[from] SandboxError),
    /// The program could not be started.
    #[error("cannot start {}", .program.display())]
    Spawn {
        /// The program Beez tried to start.
        program: OsString,
        /// Why it could not start.
        #[source]
        source: io::Error,
    },
    /// Reading the program's output or waiting for it failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl ProcessError {
    /// The category of the underlying I/O error, or `Other` when there is none.
    #[must_use]
    pub fn kind(&self) -> io::ErrorKind {
        match self {
            Self::Sandbox(SandboxError::Io(error))
            | Self::Spawn { source: error, .. }
            | Self::Io(error) => error.kind(),
            Self::Sandbox(_) => io::ErrorKind::Other,
        }
    }
}

/// One of a child process's output streams.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Receives every output line while a process runs.
///
/// The slice holds the line as the child wrote it, with the trailing newline
/// when the child wrote one. The sink runs on the reader threads, so it must
/// work when two threads call it at the same time.
pub type OutputSink<'sink> = dyn Fn(OutputStream, &[u8]) + Send + Sync + 'sink;

/// Captured child-process streams and exit status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status_code: Option<i32>,
}

impl ProcessOutput {
    /// Captured standard output.
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Captured standard error.
    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// One captured stream.
    #[must_use]
    pub fn stream(&self, stream: OutputStream) -> &[u8] {
        match stream {
            OutputStream::Stdout => &self.stdout,
            OutputStream::Stderr => &self.stderr,
        }
    }

    /// Returns `None` after signal termination.
    #[must_use]
    pub fn status_code(&self) -> Option<i32> {
        self.status_code
    }

    /// True only when the process exits with status zero.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.status_code == Some(0)
    }
}

/// Stops every process started with it once it is cancelled.
///
/// A process that starts after the cancellation is stopped at once.
///
/// Only the direct child is killed. Processes it started itself, such as the
/// commands a shell script runs, keep running unless they exit when their
/// parent does.
#[derive(Debug, Default)]
pub struct Cancellation {
    state: Mutex<CancelState>,
}

#[derive(Debug, Default)]
struct CancelState {
    cancelled: bool,
    /// The relays of running processes, which each stop their child.
    relays: Vec<mpsc::Sender<Signal>>,
}

impl Cancellation {
    /// A cancellation that has not been cancelled yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Kills every running process started with this cancellation, and
    /// every process started with it from now on.
    pub fn cancel(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.cancelled = true;
        tracing::debug!(running = state.relays.len(), "cancelling processes");
        for relay in state.relays.drain(..) {
            let _ = relay.send(Signal::Cancel);
        }
    }

    /// True once [`Self::cancel`] has run.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancelled
    }

    /// Lets `cancel` reach one relay; false when the cancellation came first.
    fn register(&self, relay: mpsc::Sender<Signal>) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.cancelled {
            return false;
        }
        state.relays.push(relay);
        true
    }
}

/// What a relay waits for while its child runs.
#[derive(Debug)]
enum Signal {
    /// A reader thread ended, by a panic when `panicked` is set.
    ReaderDone { panicked: bool },
    /// The cancellation asks for the child to stop.
    Cancel,
}

/// Tells the relay that a reader thread ended, even when it panicked.
struct ReaderDone(mpsc::Sender<Signal>);

impl Drop for ReaderDone {
    fn drop(&mut self) {
        let _ = self.0.send(Signal::ReaderDone {
            panicked: thread::panicking(),
        });
    }
}

/// Runs Beez process requests in one working directory.
///
/// The directory is explicit, because one process can run tasks of several
/// workflows at the same time, and a process has only one current directory.
#[derive(Clone, Debug)]
pub struct ProcessRunner {
    working_directory: PathBuf,
}

impl ProcessRunner {
    /// Runs requests in `working_directory`.
    #[must_use]
    pub fn new(working_directory: impl Into<PathBuf>) -> Self {
        Self {
            working_directory: working_directory.into(),
        }
    }

    /// Runs requests in this process's own working directory.
    pub fn here() -> io::Result<Self> {
        Ok(Self::new(std::env::current_dir()?))
    }

    /// Starts a request and captures its output.
    pub fn run(&self, request: ExecutionRequest) -> Result<ProcessOutput, ProcessError> {
        self.run_streaming(request, None, &|_, _| {})
    }

    /// Runs a request and hands every output line to `sink` while it runs.
    ///
    /// The returned output holds the same bytes the sink received.
    pub fn run_streaming(
        &self,
        request: ExecutionRequest,
        sandbox: Option<&SandboxPolicy>,
        sink: &OutputSink<'_>,
    ) -> Result<ProcessOutput, ProcessError> {
        self.run_cancellable(request, sandbox, sink, &Cancellation::new())
    }

    /// Runs a request like [`Self::run_streaming`], and kills it when `cancel`
    /// is cancelled.
    ///
    /// The returned output then holds what the request wrote before it died.
    pub fn run_cancellable(
        &self,
        request: ExecutionRequest,
        sandbox: Option<&SandboxPolicy>,
        sink: &OutputSink<'_>,
        cancel: &Cancellation,
    ) -> Result<ProcessOutput, ProcessError> {
        let (harness, call) = request.into_parts();
        let (program, arguments) = call.into_parts();

        // Arguments can hold a prompt, so only their count is traced.
        tracing::debug!(
            program = %program.display(),
            arguments = arguments.len(),
            harness = ?harness,
            cwd = %self.working_directory.display(),
            sandboxed = sandbox.is_some(),
            "preparing process"
        );
        if let Some(policy) = sandbox {
            // The sandbox must outlive the child, because it owns the proxy.
            let mut sandboxed = SandboxedCommand::new(
                policy,
                harness,
                &program,
                &arguments,
                &self.working_directory,
            )?;
            relay(sandboxed.command_mut(), sink, cancel)
        } else {
            let mut command = Command::new(program);
            command.args(arguments).current_dir(&self.working_directory);
            relay(&mut command, sink, cancel)
        }
    }
}

/// Runs `command` with piped output, forwarding and collecting every line.
///
/// The child dies early when `cancel` is cancelled or a reader thread
/// panics, and it is always waited for, so no child outlives the call.
fn relay(
    command: &mut Command,
    sink: &OutputSink<'_>,
    cancel: &Cancellation,
) -> Result<ProcessOutput, ProcessError> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|source| {
        tracing::debug!(program = ?command.get_program(), error = %source, "spawn failed");
        ProcessError::Spawn {
            program: command.get_program().to_owned(),
            source,
        }
    })?;
    tracing::debug!(pid = child.id(), program = ?command.get_program(), "spawned process");
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (signals, received) = mpsc::channel();
    if !cancel.register(signals.clone()) {
        tracing::debug!(pid = child.id(), "cancelled before it started, killing");
        kill(&mut child);
    }

    let (stdout, stderr) = thread::scope(|scope| {
        let stdout =
            stdout.map(|stdout| spawn_reader(scope, stdout, OutputStream::Stdout, sink, &signals));
        let stderr =
            stderr.map(|stderr| spawn_reader(scope, stderr, OutputStream::Stderr, sink, &signals));
        let readers = usize::from(stdout.is_some()) + usize::from(stderr.is_some());
        supervise(&mut child, &received, readers);
        (
            stdout.map(ScopedJoinHandle::join),
            stderr.map(ScopedJoinHandle::join),
        )
    });

    let status = child.wait();
    let stdout = collected(stdout);
    let stderr = collected(stderr);
    let status = status?;
    tracing::debug!(pid = child.id(), %status, "process exited");
    for (stream, collected) in [("stdout", &stdout), ("stderr", &stderr)] {
        if let Err(error) = collected {
            tracing::warn!(pid = child.id(), stream, %error, "reading the output failed");
        }
    }

    Ok(ProcessOutput {
        stdout: stdout?,
        stderr: stderr?,
        status_code: status.code(),
    })
}

/// Starts a thread that forwards one stream and reports when it ends.
fn spawn_reader<'scope>(
    scope: &'scope Scope<'scope, '_>,
    reader: impl Read + Send + 'scope,
    stream: OutputStream,
    sink: &'scope OutputSink<'_>,
    signals: &mpsc::Sender<Signal>,
) -> ScopedJoinHandle<'scope, io::Result<Vec<u8>>> {
    let done = ReaderDone(signals.clone());
    scope.spawn(move || {
        let _done = done;
        forward(reader, stream, sink)
    })
}

/// Waits until every reader has ended, killing the child on a cancellation
/// or when a reader panics, so the other reader sees its stream end.
fn supervise(child: &mut Child, received: &mpsc::Receiver<Signal>, mut readers: usize) {
    while readers > 0 {
        match received.recv() {
            Ok(Signal::ReaderDone { panicked }) => {
                readers -= 1;
                if panicked {
                    tracing::error!(pid = child.id(), "an output reader panicked, killing");
                    kill(child);
                }
            }
            Ok(Signal::Cancel) => {
                tracing::debug!(pid = child.id(), "cancelled, killing");
                kill(child);
            }
            Err(_) => return,
        }
    }
}

/// Kills `child`. A child that already exited cannot be killed, which is fine.
fn kill(child: &mut Child) {
    if let Err(error) = child.kill() {
        tracing::debug!(pid = child.id(), %error, "kill failed");
    }
}

/// What a reader thread read, resuming its panic; a stream that was never
/// taken read nothing.
fn collected(reader: Option<thread::Result<io::Result<Vec<u8>>>>) -> io::Result<Vec<u8>> {
    match reader {
        Some(reader) => reader.unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
        None => Ok(Vec::new()),
    }
}

/// Reads whole lines until the stream ends, forwarding each one to `sink`.
///
/// Returns every byte it read, or the read error that cut the stream short.
fn forward(reader: impl Read, stream: OutputStream, sink: &OutputSink<'_>) -> io::Result<Vec<u8>> {
    let mut reader = BufReader::new(reader);
    let mut collected = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(collected);
        }
        collected.extend_from_slice(&line);
        tracing::trace!(?stream, bytes = line.len(), "output line");
        sink(stream, &line);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read};

    use super::{OutputStream, forward};

    /// Yields one line, then fails the way a broken pipe read would.
    struct FailingReader {
        line: &'static [u8],
    }

    impl Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.line.is_empty() {
                return Err(io::Error::other("read failed"));
            }
            self.line.read(buffer)
        }
    }

    #[test]
    fn reports_a_read_error_instead_of_truncating_the_output() {
        let sut = FailingReader { line: b"first\n" };

        let actual = forward(sut, OutputStream::Stdout, &|_, _| {});

        assert_eq!(actual.unwrap_err().to_string(), "read failed");
    }
}
