use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use loom_policy::SandboxPolicy;
use loom_sandbox::SandboxedCommand;

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

/// Runs Loom process requests in one working directory.
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
        let (program, arguments) = request.into_parts();

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
    let collected = Mutex::new((Vec::new(), Vec::new()));

    let collect = |stream: OutputStream, line: &[u8]| {
        if let Ok(mut collected) = collected.lock() {
            match stream {
                OutputStream::Stdout => collected.0.extend_from_slice(line),
                OutputStream::Stderr => collected.1.extend_from_slice(line),
            }
        }
        sink(stream, line);
    };
    let collect: &OutputSink<'_> = &collect;

    std::thread::scope(|scope| {
        if let Some(stdout) = stdout {
            scope.spawn(move || forward(stdout, OutputStream::Stdout, collect));
        }
        if let Some(stderr) = stderr {
            scope.spawn(move || forward(stderr, OutputStream::Stderr, collect));
        }
    });

    let status = child.wait()?;
    let (stdout, stderr) = collected
        .into_inner()
        .map_err(|_| io::Error::other("output collection lock is poisoned"))?;

    Ok(ProcessOutput {
        stdout,
        stderr,
        status_code: status.code(),
    })
}

/// Reads whole lines until the stream ends.
fn forward(reader: impl Read, stream: OutputStream, sink: &OutputSink<'_>) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => sink(stream, &line),
        }
    }
}
