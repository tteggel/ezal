//! Sans-I/O HTTP/1.1 response framing for the orbital-element download.
//!
//! The firmware owns DNS, the TCP socket and the deadline. This decoder
//! accepts arbitrarily split reads, validates the status line and framing
//! headers, and hands body bytes to the caller as they arrive, so no
//! response-sized buffer is needed. It never trusts lengths for arithmetic:
//! bytes after a complete body are ignored rather than subtracted, and a
//! connection closed early is an explicit [`Error::Truncated`] instead of a
//! loop waiting for data that cannot arrive.

use heapless::Vec;

/// Longest header or chunk-size line examined. Longer lines are skipped
/// unless they carry framing, which must be short.
const MAX_LINE: usize = 256;
/// Header block limit, bounding work before the body.
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Body limit, bounding work for a hostile or broken server.
pub const MAX_BODY: usize = 64 * 1024;

/// Why a response cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The status line is not `HTTP/1.x 200`.
    Status,
    /// A malformed or oversized header block.
    Header,
    /// Conflicting or unsupported `Content-Length` / `Transfer-Encoding`.
    Framing,
    /// Malformed chunked transfer coding.
    Chunk,
    /// The body exceeds [`MAX_BODY`].
    TooLarge,
    /// The connection closed before the message was complete.
    Truncated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    StatusLine,
    Headers,
    /// Content-Length bytes still to deliver.
    Length(usize),
    /// No framing headers: the body ends when the server closes.
    UntilClose,
    ChunkSize,
    ChunkData(usize),
    /// The CRLF that ends each chunk's data.
    ChunkEnd,
    Trailers,
    Done,
}

/// Incremental decoder for one response. Feed it everything the socket
/// returns, then call [`finish`](Self::finish) at end of stream.
pub struct Response {
    state: State,
    line: Vec<u8, MAX_LINE>,
    overlong: bool,
    header_bytes: usize,
    body_bytes: usize,
    content_length: Option<usize>,
    chunked: bool,
}

impl Default for Response {
    fn default() -> Self {
        Self::new()
    }
}

impl Response {
    /// Expect a status line first.
    pub const fn new() -> Self {
        Self {
            state: State::StatusLine,
            line: Vec::new(),
            overlong: false,
            header_bytes: 0,
            body_bytes: 0,
            content_length: None,
            chunked: false,
        }
    }

    /// The whole message has been received; later bytes are ignored.
    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// Process received bytes, passing body data to `body` in order. After an
    /// error the response is unusable; stop reading.
    pub fn feed(&mut self, mut input: &[u8], mut body: impl FnMut(&[u8])) -> Result<(), Error> {
        while !input.is_empty() {
            match self.state {
                State::Done => return Ok(()),
                State::Length(remaining) | State::ChunkData(remaining) => {
                    let (data, rest) = input.split_at(remaining.min(input.len()));
                    self.deliver(data, &mut body)?;
                    let remaining = remaining - data.len();
                    self.state = match (self.state, remaining) {
                        (State::Length(_), 0) => State::Done,
                        (State::Length(_), _) => State::Length(remaining),
                        (_, 0) => State::ChunkEnd,
                        _ => State::ChunkData(remaining),
                    };
                    input = rest;
                }
                State::UntilClose => {
                    self.deliver(input, &mut body)?;
                    input = &[];
                }
                _ => {
                    let Some((&byte, rest)) = input.split_first() else {
                        break;
                    };
                    input = rest;
                    if matches!(self.state, State::StatusLine | State::Headers) {
                        self.header_bytes += 1;
                        if self.header_bytes > MAX_HEADER_BYTES {
                            return Err(Error::Header);
                        }
                    } else {
                        // Chunk framing and trailers count against the body limit.
                        self.count(1)?;
                    }
                    if byte != b'\n' {
                        if self.line.push(byte).is_err() {
                            self.overlong = true;
                        }
                        continue;
                    }
                    let line = core::mem::take(&mut self.line);
                    let overlong = core::mem::replace(&mut self.overlong, false);
                    // RFC 9112 permits a bare LF line terminator.
                    let line = line.strip_suffix(b"\r").unwrap_or(&line);
                    self.line(line, overlong)?;
                }
            }
        }
        Ok(())
    }

    /// End of stream. A body without framing headers ends here; any other
    /// unfinished state means the server closed early.
    pub fn finish(&self) -> Result<(), Error> {
        match self.state {
            State::Done | State::UntilClose => Ok(()),
            _ => Err(Error::Truncated),
        }
    }

    fn deliver(&mut self, data: &[u8], body: &mut impl FnMut(&[u8])) -> Result<(), Error> {
        self.count(data.len())?;
        if !data.is_empty() {
            body(data);
        }
        Ok(())
    }

    fn count(&mut self, bytes: usize) -> Result<(), Error> {
        self.body_bytes += bytes;
        if self.body_bytes > MAX_BODY {
            return Err(Error::TooLarge);
        }
        Ok(())
    }

    fn line(&mut self, line: &[u8], overlong: bool) -> Result<(), Error> {
        self.state = match self.state {
            State::StatusLine => {
                // `HTTP/1.x 200`, then the end of the line or a reason phrase.
                let ok = line.len() >= 12
                    && line.starts_with(b"HTTP/1.")
                    && line[7].is_ascii_digit()
                    && &line[8..12] == b" 200"
                    && line.get(12).is_none_or(|&byte| byte == b' ');
                if overlong || !ok {
                    return Err(Error::Status);
                }
                State::Headers
            }
            State::Headers if line.is_empty() => self.body_state()?,
            State::Headers => {
                self.header(line, overlong)?;
                State::Headers
            }
            State::ChunkSize => match chunk_size(line) {
                Some(0) => State::Trailers,
                Some(size) if size <= MAX_BODY => State::ChunkData(size),
                Some(_) => return Err(Error::TooLarge),
                None => return Err(Error::Chunk),
            },
            State::ChunkEnd if line.is_empty() && !overlong => State::ChunkSize,
            State::ChunkEnd => return Err(Error::Chunk),
            // Trailer fields carry nothing this client uses.
            State::Trailers if line.is_empty() => State::Done,
            State::Trailers => State::Trailers,
            state => state,
        };
        Ok(())
    }

    fn header(&mut self, line: &[u8], overlong: bool) -> Result<(), Error> {
        // Folded continuation lines are obsolete and could hide framing.
        if line.first().is_some_and(u8::is_ascii_whitespace) {
            return Err(Error::Header);
        }
        let Some(colon) = line.iter().position(|&byte| byte == b':') else {
            // An overlong line may have lost its colon to truncation.
            return if overlong { Ok(()) } else { Err(Error::Header) };
        };
        let name = &line[..colon];
        // Whitespace before the colon is invalid and could disguise framing.
        if name.is_empty() || name.last().is_some_and(u8::is_ascii_whitespace) {
            return Err(Error::Header);
        }
        let value = line[colon + 1..].trim_ascii();
        let content_length = name.eq_ignore_ascii_case(b"content-length");
        let transfer_encoding = name.eq_ignore_ascii_case(b"transfer-encoding");
        if !(content_length || transfer_encoding) {
            return Ok(());
        }
        // Framing values are short; a truncated one cannot be trusted.
        if overlong {
            return Err(Error::Framing);
        }
        if content_length {
            let length = decimal(value).ok_or(Error::Framing)?;
            if self
                .content_length
                .is_some_and(|previous| previous != length)
            {
                return Err(Error::Framing);
            }
            self.content_length = Some(length);
        } else if value.eq_ignore_ascii_case(b"chunked") && !self.chunked {
            self.chunked = true;
        } else {
            // Compression or repeated codings were never requested.
            return Err(Error::Framing);
        }
        Ok(())
    }

    fn body_state(&self) -> Result<State, Error> {
        match (self.chunked, self.content_length) {
            // Both is a request-smuggling shape; refuse rather than guess.
            (true, Some(_)) => Err(Error::Framing),
            (true, None) => Ok(State::ChunkSize),
            (false, Some(length)) if length > MAX_BODY => Err(Error::TooLarge),
            (false, Some(0)) => Ok(State::Done),
            (false, Some(length)) => Ok(State::Length(length)),
            (false, None) => Ok(State::UntilClose),
        }
    }
}

/// Non-empty ASCII decimal without sign or overflow.
fn decimal(text: &[u8]) -> Option<usize> {
    if text.is_empty() {
        return None;
    }
    text.iter().try_fold(0usize, |value, &byte| {
        let digit = (byte as char).to_digit(10)?;
        value.checked_mul(10)?.checked_add(digit as usize)
    })
}

/// A chunk-size line: hexadecimal digits, optionally followed by extensions.
fn chunk_size(line: &[u8]) -> Option<usize> {
    let digits = line
        .iter()
        .position(|&byte| !byte.is_ascii_hexdigit())
        .unwrap_or(line.len());
    let rest = &line[digits..];
    if digits == 0 || !(rest.is_empty() || matches!(rest[0], b';' | b' ' | b'\t')) {
        return None;
    }
    line[..digits].iter().try_fold(0usize, |value, &byte| {
        let digit = (byte as char).to_digit(16)?;
        value.checked_mul(16)?.checked_add(digit as usize)
    })
}
