use std::io;
use std::net::{TcpListener, TcpStream};
#[cfg(target_os = "linux")]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};

/// How many connections one server handles at the same time.
const MAX_CONNECTIONS: usize = 256;

/// A socket that accepts connections and can wake its own accept loop.
pub(crate) trait Listener: Send + 'static {
    /// One accepted connection.
    type Stream: Send + 'static;

    fn accept(&self) -> io::Result<Self::Stream>;

    /// Connects to itself so a blocked `accept` returns.
    fn wake(&self) -> io::Result<()>;
}

impl Listener for TcpListener {
    type Stream = TcpStream;

    fn accept(&self) -> io::Result<Self::Stream> {
        Self::accept(self).map(|(stream, _)| stream)
    }

    fn wake(&self) -> io::Result<()> {
        TcpStream::connect(self.local_addr()?).map(drop)
    }
}

/// A Unix listener that remembers its path so it can wake itself.
#[cfg(target_os = "linux")]
pub(crate) struct UnixPathListener {
    listener: UnixListener,
    path: PathBuf,
}

#[cfg(target_os = "linux")]
impl UnixPathListener {
    pub(crate) fn bind(path: PathBuf) -> io::Result<Self> {
        let listener = UnixListener::bind(&path)?;
        Ok(Self { listener, path })
    }
}

#[cfg(target_os = "linux")]
impl Listener for UnixPathListener {
    type Stream = UnixStream;

    fn accept(&self) -> io::Result<Self::Stream> {
        self.listener.accept().map(|(stream, _)| stream)
    }

    fn wake(&self) -> io::Result<()> {
        UnixStream::connect(&self.path).map(drop)
    }
}

/// One live connection, counted against the server's limit until its handler ends.
struct ConnectionSlot(Arc<AtomicUsize>);

impl ConnectionSlot {
    /// Takes a slot, or returns `None` when the server already has its limit.
    fn claim(live: &Arc<AtomicUsize>) -> Option<Self> {
        if live.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            live.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(Arc::clone(live)))
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// An accept loop that handles each connection on its own thread and stops on drop.
pub(crate) struct Server {
    stop: Arc<AtomicBool>,
    wake: Box<dyn Fn() -> bool + Send>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Server")
            .field("stopped", &self.stop.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl Server {
    pub(crate) fn spawn<L: Listener + Sync>(
        listener: L,
        handler: impl Fn(L::Stream) + Send + Sync + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let listener = Arc::new(listener);
        let waker = Arc::clone(&listener);
        let stop_flag = Arc::clone(&stop);
        let handler = Arc::new(handler);
        let live = Arc::new(AtomicUsize::new(0));
        let reported_limit = AtomicBool::new(false);
        let thread = thread::spawn(move || {
            while !stop_flag.load(Ordering::SeqCst) {
                let stream = match listener.accept() {
                    Ok(stream) => stream,
                    // A client that goes away before the accept is normal.
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        continue;
                    }
                    // Retrying a permanent error would spin this thread.
                    Err(error) => {
                        crate::diagnostic::report(&format!(
                            "stopped accepting connections: {error}"
                        ));
                        break;
                    }
                };
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }
                let Some(slot) = ConnectionSlot::claim(&live) else {
                    if !reported_limit.swap(true, Ordering::SeqCst) {
                        crate::diagnostic::report(&format!(
                            "refused a connection, {MAX_CONNECTIONS} are already open"
                        ));
                    }
                    drop(stream);
                    continue;
                };
                let handler = Arc::clone(&handler);
                thread::spawn(move || {
                    let _slot = slot;
                    handler(stream);
                });
            }
        });

        Self {
            stop,
            wake: Box::new(move || waker.wake().is_ok()),
            thread: Some(thread),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // A blocked `accept` returns only through the wake, so joining without
        // one would never finish. Leave the thread instead.
        let woken = (self.wake)();
        if let Some(thread) = self.thread.take().filter(|_| woken) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, SyncSender};
    use std::time::Duration;

    use super::{ConnectionSlot, Listener, MAX_CONNECTIONS, Server};

    /// Fails every accept, aborted first and then permanently.
    struct FailingListener {
        calls: Arc<AtomicUsize>,
        permanent: SyncSender<()>,
    }

    impl Listener for FailingListener {
        type Stream = ();

        fn accept(&self) -> io::Result<Self::Stream> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                return Err(io::Error::from(io::ErrorKind::ConnectionAborted));
            }
            let _ = self.permanent.send(());
            Err(io::Error::from(io::ErrorKind::InvalidInput))
        }

        fn wake(&self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn retries_an_aborted_client_but_stops_on_a_permanent_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (permanent, reached) = mpsc::sync_channel(1);

        let server = Server::spawn(
            FailingListener {
                calls: Arc::clone(&calls),
                permanent,
            },
            |()| {},
        );
        reached.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(server);

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn refuses_slots_over_the_limit_and_frees_them_again() {
        let live = Arc::new(AtomicUsize::new(0));

        let slots = (0..MAX_CONNECTIONS)
            .map(|_| ConnectionSlot::claim(&live))
            .collect::<Vec<_>>();

        assert!(slots.iter().all(Option::is_some));
        assert!(ConnectionSlot::claim(&live).is_none());
        drop(slots);
        assert_eq!(live.load(Ordering::SeqCst), 0);
        assert!(ConnectionSlot::claim(&live).is_some());
    }
}
