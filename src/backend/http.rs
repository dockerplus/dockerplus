//! A very small HTTP/1.1 client over a Unix socket.
//!
//! The Docker Engine API is the only thing we talk to, so this needs no TLS, no
//! connection pool and no async runtime. Keeping it this small is what lets the
//! app stay light: one short-lived thread per request, nothing idle in between.

use std::io::{self, ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Aborts an in-flight stream from another thread by shutting the socket down,
/// which wakes the blocked `read` immediately.
#[derive(Clone, Default)]
pub struct Cancel(Arc<Mutex<CancelState>>);

#[derive(Default)]
struct CancelState {
    cancelled: bool,
    stream: Option<UnixStream>,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        let mut state = self.0.lock().unwrap();
        state.cancelled = true;
        if let Some(stream) = state.stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.lock().unwrap().cancelled
    }

    fn attach(&self, stream: &UnixStream) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        if state.cancelled {
            stream.shutdown(Shutdown::Both)?;
        } else {
            state.stream = Some(stream.try_clone()?);
        }
        Ok(())
    }
}

/// Sends a request and collects the whole response body.
pub fn request(
    sock: &Path,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> io::Result<Response> {
    let mut collected = Vec::new();
    let status = exchange(sock, method, path, body, Some(timeout), None, |_, chunk| {
        collected.extend_from_slice(chunk);
    })?;
    Ok(Response { status, body: collected })
}

/// Sends a request and hands the status plus each decoded body chunk to
/// `on_chunk` as they arrive. Blocks until the server ends the response or
/// `cancel` fires.
pub fn stream(
    sock: &Path,
    method: &str,
    path: &str,
    cancel: &Cancel,
    on_chunk: impl FnMut(u16, &[u8]),
) -> io::Result<u16> {
    exchange(sock, method, path, None, None, Some(cancel), on_chunk)
}

fn exchange(
    sock: &Path,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Option<Duration>,
    cancel: Option<&Cancel>,
    mut on_chunk: impl FnMut(u16, &[u8]),
) -> io::Result<u16> {
    let mut stream = UnixStream::connect(sock)?;
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    if let Some(cancel) = cancel {
        cancel.attach(&stream)?;
    }

    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: docker\r\nUser-Agent: DockerPlus\r\nConnection: close\r\n"
    );
    if let Some(body) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(body) = body {
        stream.write_all(body)?;
    }

    let mut buf = vec![0u8; 64 * 1024];
    let mut pending = Vec::new();
    let (status, mut decoder, leftover) = loop {
        let n = read_some(&mut stream, &mut buf, cancel)?;
        if n == 0 {
            return Err(io::Error::new(ErrorKind::UnexpectedEof, "connection closed before response"));
        }
        pending.extend_from_slice(&buf[..n]);
        if let Some(end) = find_head_end(&pending) {
            let parsed = parse_head(&pending[..end])?;
            break (parsed.0, BodyDecoder::new(parsed.1), pending[end + 4..].to_vec());
        }
    };

    let mut sink = |chunk: &[u8]| on_chunk(status, chunk);
    if decoder.feed(&leftover, &mut sink) {
        return Ok(status);
    }
    loop {
        let n = read_some(&mut stream, &mut buf, cancel)?;
        if n == 0 || decoder.feed(&buf[..n], &mut sink) {
            return Ok(status);
        }
    }
}

fn read_some(stream: &mut UnixStream, buf: &mut [u8], cancel: Option<&Cancel>) -> io::Result<usize> {
    loop {
        match stream.read(buf) {
            // A signal is not the end of the response.
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            // A cancelled stream ending in a reset is the expected way out.
            Err(_) if cancel.is_some_and(Cancel::is_cancelled) => return Ok(0),
            other => return other,
        }
    }
}

fn find_head_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Framing {
    Length(u64),
    Chunked,
    UntilClose,
}

fn parse_head(head: &[u8]) -> io::Result<(u16, Framing)> {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "malformed HTTP status line"))?;

    let mut framing = Framing::UntilClose;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        if name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked") {
            framing = Framing::Chunked;
        } else if name.eq_ignore_ascii_case("content-length") && framing != Framing::Chunked {
            if let Ok(length) = value.parse() {
                framing = Framing::Length(length);
            }
        }
    }
    // 204 and 304 have no body whatever the headers say.
    if status == 204 || status == 304 {
        framing = Framing::Length(0);
    }
    Ok((status, framing))
}

/// Push-style decoder, so a chunk boundary may fall anywhere in a socket read.
struct BodyDecoder {
    state: BodyState,
}

enum BodyState {
    Remaining(u64),
    UntilClose,
    ChunkSize(Vec<u8>),
    ChunkData(u64),
    ChunkDataEnd,
    Done,
}

impl BodyDecoder {
    fn new(framing: Framing) -> Self {
        let state = match framing {
            Framing::Length(0) => BodyState::Done,
            Framing::Length(n) => BodyState::Remaining(n),
            Framing::Chunked => BodyState::ChunkSize(Vec::new()),
            Framing::UntilClose => BodyState::UntilClose,
        };
        Self { state }
    }

    /// Returns true once the body is complete.
    fn feed(&mut self, mut input: &[u8], sink: &mut impl FnMut(&[u8])) -> bool {
        while !input.is_empty() {
            match &mut self.state {
                BodyState::Done => return true,
                BodyState::UntilClose => {
                    sink(input);
                    return false;
                }
                BodyState::Remaining(left) => {
                    let take = (*left).min(input.len() as u64) as usize;
                    sink(&input[..take]);
                    input = &input[take..];
                    *left -= take as u64;
                    if *left == 0 {
                        self.state = BodyState::Done;
                    }
                }
                BodyState::ChunkSize(line) => {
                    let Some(newline) = input.iter().position(|b| *b == b'\n') else {
                        line.extend_from_slice(input);
                        return false;
                    };
                    line.extend_from_slice(&input[..newline]);
                    input = &input[newline + 1..];
                    let size = chunk_size(line);
                    self.state = if size == 0 { BodyState::Done } else { BodyState::ChunkData(size) };
                }
                BodyState::ChunkData(left) => {
                    let take = (*left).min(input.len() as u64) as usize;
                    sink(&input[..take]);
                    input = &input[take..];
                    *left -= take as u64;
                    if *left == 0 {
                        self.state = BodyState::ChunkDataEnd;
                    }
                }
                BodyState::ChunkDataEnd => {
                    let Some(newline) = input.iter().position(|b| *b == b'\n') else {
                        return false; // only the CR arrived so far
                    };
                    input = &input[newline + 1..];
                    self.state = BodyState::ChunkSize(Vec::new());
                }
            }
        }
        matches!(self.state, BodyState::Done)
    }
}

fn chunk_size(line: &[u8]) -> u64 {
    let text = String::from_utf8_lossy(line);
    let digits = text.trim().split(';').next().unwrap_or("");
    u64::from_str_radix(digits, 16).unwrap_or(0)
}

/// Percent-encodes a query value or path segment.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_in_pieces(framing: Framing, data: &[u8], piece: usize) -> (Vec<u8>, bool) {
        let mut decoder = BodyDecoder::new(framing);
        let mut out = Vec::new();
        let mut done = false;
        for part in data.chunks(piece) {
            done = decoder.feed(part, &mut |c| out.extend_from_slice(c));
            if done {
                break;
            }
        }
        (out, done)
    }

    #[test]
    fn chunked_body_survives_any_split() {
        let wire = b"5\r\nhello\r\n7;ext=1\r\n, world\r\n0\r\n\r\n";
        for piece in 1..wire.len() {
            let (out, done) = decode_in_pieces(Framing::Chunked, wire, piece);
            assert_eq!(out, b"hello, world", "piece size {piece}");
            assert!(done, "piece size {piece}");
        }
    }

    #[test]
    fn content_length_stops_at_the_limit() {
        let (out, done) = decode_in_pieces(Framing::Length(5), b"helloEXTRA", 3);
        assert_eq!(out, b"hello");
        assert!(done);
    }

    #[test]
    fn head_parsing_prefers_chunked() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: chunked";
        assert_eq!(parse_head(head).unwrap(), (200, Framing::Chunked));
        let head = b"HTTP/1.1 204 No Content\r\nContent-Length: 12";
        assert_eq!(parse_head(head).unwrap(), (204, Framing::Length(0)));
        assert!(parse_head(b"garbage").is_err());
    }

    #[test]
    fn encoding_leaves_safe_characters_alone() {
        assert_eq!(encode("sha256:ab-c_1.2"), "sha256%3Aab-c_1.2");
        assert_eq!(encode("{\"a\":1}"), "%7B%22a%22%3A1%7D");
        assert_eq!(encode("docker.io/library/nginx"), "docker.io%2Flibrary%2Fnginx");
    }
}
