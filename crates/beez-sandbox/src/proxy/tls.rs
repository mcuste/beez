//! Reads the server name out of a TLS `ClientHello` without decrypting anything.
//!
//! A tunnel only tells the proxy the host it opens a connection to. Shared
//! hosting, such as a CDN, serves many sites on that one address and picks the
//! site from the server name inside the `ClientHello`. The proxy reads that name
//! so a tunnel to an allowed host cannot ask the same address for another site.

use std::collections::BTreeSet;
use std::io::{self, Read};

/// Record type of a TLS handshake record.
const HANDSHAKE_RECORD: u8 = 22;
/// Handshake type of a `ClientHello`.
const CLIENT_HELLO: u8 = 1;
/// Extension that names the server.
const SERVER_NAME: u16 = 0;
/// Name type of a DNS host name in the server name extension.
const HOST_NAME: u8 = 0;
/// Extension that carries an encrypted `ClientHello`, which hides the real name.
const ENCRYPTED_CLIENT_HELLO: u16 = 0xfe0d;
/// Largest record a TLS sender may write.
const MAX_RECORD: usize = 1 << 14;
/// Length of a TLS record header.
const RECORD_HEADER: usize = 5;

/// What the first record of a tunnel says about where it goes.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Hello {
    /// A `ClientHello`, with the server name it asks for.
    Tls {
        server_name: Option<String>,
        encrypted: bool,
    },
    /// Bytes that do not form a single, well-formed `ClientHello` record.
    Other,
}

/// Reads from `stream` until `received` holds the first TLS record.
///
/// Stops early when the bytes cannot start a handshake record, and returns
/// every byte read so the caller can send them on.
pub(super) fn read_first_record(
    stream: &mut impl Read,
    mut received: Vec<u8>,
) -> io::Result<Vec<u8>> {
    let mut chunk = [0_u8; 4096];
    loop {
        match record_length(&received) {
            Some(length) if received.len() >= length => return Ok(received),
            None if !received.is_empty() => return Ok(received),
            _ => {}
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "tunnel closed before its first TLS record ended",
            ));
        }
        received.extend_from_slice(chunk.get(..read).unwrap_or_default());
    }
}

/// The full length of the record `bytes` starts, once the header is known.
///
/// `None` means the bytes cannot start a handshake record. A partial header
/// asks for more bytes by answering with a length past the end.
fn record_length(bytes: &[u8]) -> Option<usize> {
    match bytes {
        [kind, ..] if *kind != HANDSHAKE_RECORD => None,
        [_, _, _, high, low, ..] => {
            let length = usize::from(u16::from_be_bytes([*high, *low]));
            (length <= MAX_RECORD).then_some(RECORD_HEADER + length)
        }
        _ => Some(RECORD_HEADER),
    }
}

/// Parses the `ClientHello` in the first record of a tunnel.
///
/// A `ClientHello` split over several records, a server name list with more
/// than one name, or an extension that appears twice counts as `Other`, so a
/// client cannot show the proxy one name and the server another.
pub(super) fn parse(bytes: &[u8]) -> Hello {
    parse_client_hello(bytes).unwrap_or(Hello::Other)
}

fn parse_client_hello(bytes: &[u8]) -> Option<Hello> {
    let mut record = Reader(bytes);
    if record.u8()? != HANDSHAKE_RECORD {
        return None;
    }
    record.u16()?;
    let mut body = record.vector16()?;
    if body.u8()? != CLIENT_HELLO {
        return None;
    }
    let length = body.u24()?;
    let mut hello = Reader(body.take(length)?);
    // Version, random, session ID, cipher suites, and compression methods.
    hello.u16()?;
    hello.take(32)?;
    hello.vector8()?;
    hello.vector16()?;
    hello.vector8()?;
    let mut extensions = if hello.is_empty() {
        Reader(&[])
    } else {
        hello.vector16()?
    };

    let mut seen = BTreeSet::new();
    let mut server_name = None;
    let mut encrypted = false;
    while !extensions.is_empty() {
        let kind = extensions.u16()?;
        let mut data = extensions.vector16()?;
        if !seen.insert(kind) {
            return None;
        }
        match kind {
            SERVER_NAME => server_name = Some(host_name(&mut data)?),
            ENCRYPTED_CLIENT_HELLO => encrypted = true,
            _ => {}
        }
    }
    Some(Hello::Tls {
        server_name,
        encrypted,
    })
}

/// The one host name in a server name extension.
fn host_name(data: &mut Reader<'_>) -> Option<String> {
    let mut list = data.vector16()?;
    if list.u8()? != HOST_NAME {
        return None;
    }
    let name = list.vector16()?;
    if !list.is_empty() || !data.is_empty() || name.is_empty() {
        return None;
    }
    std::str::from_utf8(name.0).ok().map(str::to_owned)
}

/// Reads big-endian fields off the front of a byte slice.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(length)?;
        self.0 = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u16(&mut self) -> Option<u16> {
        let bytes = self.take(2)?;
        Some(u16::from_be_bytes(bytes.try_into().ok()?))
    }

    fn u24(&mut self) -> Option<usize> {
        match self.take(3)? {
            [high, middle, low] => {
                Some((usize::from(*high) << 16) | (usize::from(*middle) << 8) | usize::from(*low))
            }
            _ => None,
        }
    }

    fn vector8(&mut self) -> Option<Reader<'a>> {
        let length = self.u8()?;
        self.take(usize::from(length)).map(Reader)
    }

    fn vector16(&mut self) -> Option<Reader<'a>> {
        let length = self.u16()?;
        self.take(usize::from(length)).map(Reader)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::Cursor;

    use super::{ENCRYPTED_CLIENT_HELLO, Hello, SERVER_NAME, parse, read_first_record};

    fn vector16(bytes: &[u8]) -> Vec<u8> {
        let mut out = u16::try_from(bytes.len()).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(bytes);
        out
    }

    pub(crate) fn server_name_extension(name: &str) -> Vec<u8> {
        let mut entry = vec![0];
        entry.extend(vector16(name.as_bytes()));
        let mut extension = SERVER_NAME.to_be_bytes().to_vec();
        extension.extend(vector16(&vector16(&entry)));
        extension
    }

    pub(crate) fn encrypted_hello_extension() -> Vec<u8> {
        let mut extension = ENCRYPTED_CLIENT_HELLO.to_be_bytes().to_vec();
        extension.extend(vector16(&[0, 0, 1, 0, 1]));
        extension
    }

    /// A `ClientHello` record carrying `extensions`, as a client would send it.
    pub(crate) fn client_hello(extensions: &[Vec<u8>]) -> Vec<u8> {
        let mut hello = vec![3, 3];
        hello.extend([7; 32]);
        hello.extend([0]);
        hello.extend(vector16(&[0x13, 0x01]));
        hello.extend([1, 0]);
        hello.extend(vector16(&extensions.concat()));
        let length = u32::try_from(hello.len()).unwrap().to_be_bytes();
        let mut handshake = vec![1];
        handshake.extend_from_slice(length.get(1..).unwrap());
        handshake.extend(hello);
        let mut record = vec![22, 3, 1];
        record.extend(vector16(&handshake));
        record
    }

    fn tls(server_name: Option<&str>, encrypted: bool) -> Hello {
        Hello::Tls {
            server_name: server_name.map(str::to_owned),
            encrypted,
        }
    }

    #[test]
    fn reads_the_server_name() {
        let record = client_hello(&[server_name_extension("api.openai.com")]);

        assert_eq!(parse(&record), tls(Some("api.openai.com"), false));
    }

    #[test]
    fn notices_an_encrypted_client_hello() {
        let record = client_hello(&[
            server_name_extension("api.openai.com"),
            encrypted_hello_extension(),
        ]);

        assert_eq!(parse(&record), tls(Some("api.openai.com"), true));
    }

    #[test]
    fn reads_a_hello_without_a_server_name() {
        assert_eq!(parse(&client_hello(&[])), tls(None, false));
    }

    #[test]
    fn refuses_a_server_name_given_twice() {
        let record = client_hello(&[
            server_name_extension("api.openai.com"),
            server_name_extension("evil.example"),
        ]);

        assert_eq!(parse(&record), Hello::Other);
    }

    #[test]
    fn refuses_a_hello_cut_short_or_split_over_records() {
        let record = client_hello(&[server_name_extension("api.openai.com")]);
        let mut split = record.clone();
        // Claim a longer handshake than the record holds.
        *split.get_mut(8).unwrap() += 1;

        assert_eq!(parse(record.get(..record.len() - 1).unwrap()), Hello::Other);
        assert_eq!(parse(&split), Hello::Other);
    }

    #[test]
    fn treats_plain_text_as_other() {
        assert_eq!(parse(b"GET / HTTP/1.1\r\n\r\n"), Hello::Other);
        assert_eq!(parse(b""), Hello::Other);
    }

    #[test]
    fn reads_exactly_one_record_however_the_bytes_arrive() {
        let record = client_hello(&[server_name_extension("api.openai.com")]);
        let (first, rest) = record.split_at(3);
        let mut stream = Cursor::new([rest, b"after"].concat());

        let read = read_first_record(&mut stream, first.to_vec()).unwrap();

        assert!(read.starts_with(&record), "{read:?}");
    }

    #[test]
    fn stops_reading_at_bytes_that_are_not_tls() {
        let mut stream = Cursor::new(b"never read".to_vec());

        let read = read_first_record(&mut stream, b"GET".to_vec()).unwrap();

        assert_eq!(read, b"GET");
    }

    #[test]
    fn reports_a_tunnel_that_closes_mid_record() {
        let record = client_hello(&[server_name_extension("api.openai.com")]);
        let mut stream = Cursor::new(record.get(5..10).unwrap().to_vec());

        let error = read_first_record(&mut stream, record.get(..5).unwrap().to_vec()).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}
