//! N2YO, the device's source of orbital elements.
//!
//! The firmware asks for each catalogued satellite in turn on one connection:
//! `GET /rest/v1/satellite/tle/{norad}&apiKey={key}`. N2YO relays Space-Track's
//! element sets and allows 1000 requests an hour, so a refresh, one request per
//! satellite, leaves room for frequent reboots. Each reply is a JSON object
//! whose `tle` member holds both element lines, separated by an escaped
//! newline. An invalid key or an exhausted allowance is an `error` member
//! instead, still with status 200, so a reply without `tle` is the failure.
//!
//! [`TleField`] streams one reply body and passes on only the unescaped `tle`
//! string, so no reply-sized buffer exists. It reads just enough JSON to find
//! that member: in valid JSON the bytes `"tle"` followed by a colon can only be
//! that key. [`orbit`](crate::orbit) still validates every line it yields.

use core::fmt::Write;
use heapless::String;

/// The API host, for DNS, the TLS server name and the `Host` header.
pub const HOST: &str = "api.n2yo.com";
/// HTTPS. The device does not verify N2YO's certificate, so this keeps the
/// elements private on the way but does not prove where they came from.
pub const PORT: u16 = 443;
/// Longest key [`valid_key`] accepts.
pub const MAX_KEY_LEN: usize = 40;
/// Room for [`request`] with the longest NORAD number and key.
pub const REQUEST_CAPACITY: usize = 192;

/// Whether `key` can go into a request line unchanged: at most
/// [`MAX_KEY_LEN`] letters, digits and dashes, the characters N2YO keys use.
/// Anything else could end the line early and change the request. An empty
/// key passes, so a build without one still compiles; N2YO refuses it.
pub const fn valid_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.len() > MAX_KEY_LEN {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        if !(bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
            return false;
        }
        i += 1;
    }
    true
}

/// The HTTP/1.1 request for one satellite's elements. The connection stays
/// open for the next one. `None` if the key is not [`valid_key`].
pub fn request(norad_id: u64, key: &str) -> Option<String<REQUEST_CAPACITY>> {
    if !valid_key(key) {
        return None;
    }
    let mut request = String::new();
    write!(
        request,
        "GET /rest/v1/satellite/tle/{norad_id}&apiKey={key} HTTP/1.1\r\n\
         Host: {HOST}\r\nUser-Agent: ezal\r\nAccept: application/json\r\n\r\n"
    )
    .ok()?;
    Some(request)
}

/// Why a reply cannot supply elements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyError {
    /// No complete `tle` string, as in an `error` reply for an invalid key.
    NoElements,
    /// `tle` is not a string, or holds an escape or control byte that element
    /// lines never contain.
    Malformed,
}

const KEY: &[u8] = b"\"tle\"";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Bytes of `"tle"` matched so far.
    Seek(usize),
    /// After `"tle"`: a colon makes it the key rather than a value.
    Colon,
    /// After the colon: the value must be a string.
    Open,
    Value,
    /// After a backslash inside the value.
    Escape,
    /// The value has ended; later bytes are ignored.
    Done,
}

/// Incremental reader for the `tle` member of one reply body. Bytes may be
/// split anywhere.
pub struct TleField {
    state: State,
}

impl Default for TleField {
    fn default() -> Self {
        Self::new()
    }
}

impl TleField {
    /// Expect the start of a reply body.
    pub const fn new() -> Self {
        Self {
            state: State::Seek(0),
        }
    }

    /// Read the next body bytes, passing the unescaped `tle` string to `out`
    /// as it arrives. After an error the reply is unusable; stop reading.
    pub fn push(&mut self, bytes: &[u8], mut out: impl FnMut(&[u8])) -> Result<(), ReplyError> {
        for &byte in bytes {
            self.state = match self.state {
                State::Seek(matched) => seek(matched, byte),
                State::Colon => match byte {
                    b':' => State::Open,
                    b' ' | b'\t' | b'\r' | b'\n' => State::Colon,
                    // That `"tle"` was a value; this byte may begin the key.
                    _ => seek(0, byte),
                },
                State::Open => match byte {
                    b'"' => State::Value,
                    b' ' | b'\t' | b'\r' | b'\n' => State::Open,
                    _ => return Err(ReplyError::Malformed),
                },
                State::Value => match byte {
                    b'"' => State::Done,
                    b'\\' => State::Escape,
                    0x00..=0x1f => return Err(ReplyError::Malformed),
                    _ => {
                        out(&[byte]);
                        State::Value
                    }
                },
                State::Escape => {
                    out(&[match byte {
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'"' | b'\\' | b'/' => byte,
                        _ => return Err(ReplyError::Malformed),
                    }]);
                    State::Value
                }
                State::Done => return Ok(()),
            };
        }
        Ok(())
    }

    /// The body has ended; it must have held a complete `tle` string.
    pub fn finish(&self) -> Result<(), ReplyError> {
        if self.state == State::Done {
            Ok(())
        } else {
            Err(ReplyError::NoElements)
        }
    }
}

/// Advance the match of `"tle"`. A mismatch can only restart on a quote,
/// since no other byte of the key is a quote.
fn seek(matched: usize, byte: u8) -> State {
    if byte == KEY[matched] {
        if matched + 1 == KEY.len() {
            State::Colon
        } else {
            State::Seek(matched + 1)
        }
    } else if byte == KEY[0] {
        State::Seek(1)
    } else {
        State::Seek(0)
    }
}
