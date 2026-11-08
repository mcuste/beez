use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::ScopedJoinHandle;

use beez_policy::SandboxPolicy;
use beez_sandbox::SandboxedCommand;

use crate::execution::ExecutionRequest;

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
    pub fn run(&self, request: ExecutionRequest) -> io::Result<ProcessOutput> {
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
    ) -> io::Result<ProcessOutput> {
        let harness = request.harness();
        let (program, arguments) = request.into_process_call().into_parts();

        if let Some(policy) = sandbox {
            // The sandbox must outlive the child, because it owns the proxy.
            let mut sandboxed = SandboxedCommand::new(
                policy,
                harness,
                &program,
                &arguments,
                &self.working_directory,
            )?;
            relay(sandboxed.command_mut(), sink)
        } else {
            let mut command = Command::new(program);
            command.args(arguments).current_dir(&self.working_directory);
            relay(&mut command, sink)
        }
    }
}

/// Runs `command` with piped output, forwarding and collecting every line.
fn relay(command: &mut Command, sink: &OutputSink<'_>) -> io::Result<ProcessOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let (stdout, stderr) = std::thread::scope(|scope| {
        let stdout =
            stdout.map(|stdout| scope.spawn(move || forward(stdout, OutputStream::Stdout, sink)));
        let stderr =
            stderr.map(|stderr| scope.spawn(move || forward(stderr, OutputStream::Stderr, sink)));
        (collected(stdout), collected(stderr))
    });

    let status = child.wait()?;

    Ok(ProcessOutput {
        stdout: stdout?,
        stderr: stderr?,
        status_code: status.code(),
    })
}

/// Waits for a reader thread and returns what it read; a stream that was
/// never taken read nothing.
fn collected(reader: Option<ScopedJoinHandle<'_, io::Result<Vec<u8>>>>) -> io::Result<Vec<u8>> {
    match reader {
        Some(reader) => reader
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
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
