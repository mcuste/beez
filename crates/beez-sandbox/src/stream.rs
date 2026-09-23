use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::net::UnixStream;
use std::thread;

/// A connected byte stream that can be split for two-way copying.
pub(crate) trait Stream: Read + Write + Send {
    /// A second handle to the same connection.
    fn duplicate(&self) -> io::Result<Self>
    where
        Self: Sized;

    /// Signals end of output to the peer.
    fn finish_writing(&self) -> io::Result<()>;
}

impl Stream for TcpStream {
    fn duplicate(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn finish_writing(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Write)
    }
}

impl Stream for UnixStream {
    fn duplicate(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn finish_writing(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Write)
    }
}

/// Copies bytes in both directions until both peers stop sending.
pub(crate) fn pipe<A: Stream, B: Stream>(mut first: A, mut second: B) -> io::Result<()> {
    let mut first_reader = first.duplicate()?;
    let mut second_reader = second.duplicate()?;
    thread::scope(|scope| {
        scope.spawn(move || {
            let _ = io::copy(&mut first_reader, &mut second);
            let _ = second.finish_writing();
        });
        let _ = io::copy(&mut second_reader, &mut first);
        let _ = first.finish_writing();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::thread;

    use super::pipe;

    #[test]
    fn copies_both_directions_and_passes_on_each_half_close() {
        let (mut client, first) = UnixStream::pair().unwrap();
        let (second, mut upstream) = UnixStream::pair().unwrap();
        let piped = thread::spawn(move || pipe(first, second));

        client.write_all(b"request").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut request = Vec::new();
        upstream.read_to_end(&mut request).unwrap();
        upstream.write_all(b"answer").unwrap();
        upstream.shutdown(Shutdown::Write).unwrap();
        let mut answer = Vec::new();
        client.read_to_end(&mut answer).unwrap();

        assert_eq!(request, b"request");
        assert_eq!(answer, b"answer");
        piped.join().unwrap().unwrap();
    }
}
