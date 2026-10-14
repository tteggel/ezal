//! Response framing for the orbital-element download. Every case is fed both
//! whole and in every two-part split, as TCP may deliver it either way.

use ezal_core::catalog::{Catalog, OrbitalDownload};
use ezal_core::http::{Error, Response, MAX_BODY, MAX_HEADER_BYTES};
use ezal_core::orbit::SATELLITES;

const TLES: &str = include_str!("fixtures/weather-2026-10-07.tle");
const START: i64 = 1_791_385_200;

/// Feed `parts`, stopping like the firmware once the message is complete,
/// then signal end of stream. Returns the body or the first error.
fn decode(parts: &[&[u8]]) -> Result<Vec<u8>, Error> {
    let mut response = Response::new();
    let mut body = Vec::new();
    for part in parts {
        response.feed(part, |data| body.extend_from_slice(data))?;
        if response.is_done() {
            return Ok(body);
        }
    }
    response.finish().map(|()| body)
}

/// Decode whole, then at every split point; all must agree.
fn decode_all_splits(raw: &[u8]) -> Result<Vec<u8>, Error> {
    let whole = decode(&[raw]);
    for split in 0..=raw.len() {
        let (a, b) = raw.split_at(split);
        assert_eq!(decode(&[a, b]), whole, "split at {split}");
    }
    whole
}

#[test]
fn content_length_bodies_ignore_bytes_after_the_declared_length() {
    let exact = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789";
    assert_eq!(decode_all_splits(exact), Ok(b"0123456789".to_vec()));
    // reqwless 0.14 underflowed `remaining -= read` here and panicked.
    let extra = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789EXTRA";
    assert_eq!(decode_all_splits(extra), Ok(b"0123456789".to_vec()));
    assert_eq!(
        decode(&[
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n",
            b"0123456789EXTRA"
        ]),
        Ok(b"0123456789".to_vec())
    );
    // A real body followed by a stray CRLF in the same write.
    let mut stray =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", TLES.len()).into_bytes();
    stray.extend_from_slice(TLES.as_bytes());
    stray.extend_from_slice(b"\r\n");
    assert_eq!(decode(&[&stray]), Ok(TLES.as_bytes().to_vec()));
    // Data after completion is never read into the body.
    let mut response = Response::new();
    let mut body = Vec::new();
    response
        .feed(exact, |data| body.extend_from_slice(data))
        .unwrap();
    assert!(response.is_done());
    response
        .feed(b"more", |data| body.extend_from_slice(data))
        .unwrap();
    assert_eq!(body, b"0123456789");
    assert_eq!(
        decode_all_splits(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"),
        Ok(Vec::new())
    );
}

#[test]
fn a_connection_closed_early_is_truncated_never_a_hang() {
    // reqwless 0.14 spun forever on EOF inside a chunk without awaiting.
    for raw in [
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\n0123"[..],
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n0123\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n0123\r\n0\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1",
        b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n01234",
        b"HTTP/1.1 200 OK\r\nContent-Le",
        b"HTTP/1.1 2",
        b"",
    ] {
        assert_eq!(decode_all_splits(raw), Err(Error::Truncated), "{raw:?}");
    }
}

#[test]
fn chunked_bodies_with_extensions_trailers_and_bare_line_feeds() {
    let raw = b"HTTP/1.1 200 OK\r\ntransfer-encoding: Chunked\r\n\r\n\
                4;name=value\r\n0123\r\nA\r\n456789abcd\r\n0\r\nTrailer: x\r\n\r\n";
    assert_eq!(decode_all_splits(raw), Ok(b"0123456789abcd".to_vec()));
    let bare = b"HTTP/1.1 200 OK\nTransfer-Encoding: chunked\n\n3\nabc\n0\n\n";
    assert_eq!(decode_all_splits(bare), Ok(b"abc".to_vec()));
    for (raw, error) in [
        (
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n01234\r\n"[..],
            Error::Chunk,
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
            Error::Chunk,
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\r\n",
            Error::Chunk,
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffffffff\r\n",
            Error::Chunk,
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n100000\r\n",
            Error::TooLarge,
        ),
    ] {
        assert_eq!(decode_all_splits(raw), Err(error), "{raw:?}");
    }
}

#[test]
fn unframed_bodies_end_at_close_and_http_1_0_is_accepted() {
    assert_eq!(
        decode_all_splits(b"HTTP/1.0 200 OK\r\n\r\nuntil close"),
        Ok(b"until close".to_vec())
    );
    assert_eq!(
        decode_all_splits(b"HTTP/1.1 200\r\nServer: x\r\n\r\nbody"),
        Ok(b"body".to_vec())
    );
}

#[test]
fn only_a_200_status_line_is_accepted() {
    for raw in [
        &b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"[..],
        b"HTTP/1.1 2000 OK\r\n\r\n",
        b"HTTP/2 200\r\n\r\n",
        b"HTTP/1.1 100 Continue\r\n\r\n",
        b"<html>200</html>\r\n",
    ] {
        assert_eq!(decode_all_splits(raw), Err(Error::Status), "{raw:?}");
    }
    let long = format!("HTTP/1.1 200 {}\r\n\r\n", "x".repeat(400));
    assert_eq!(decode(&[long.as_bytes()]), Err(Error::Status));
}

#[test]
fn ambiguous_or_unsupported_framing_is_refused() {
    for (headers, error) in [
        (
            "Content-Length: 4\r\nTransfer-Encoding: chunked",
            Error::Framing,
        ),
        ("Transfer-Encoding: gzip, chunked", Error::Framing),
        (
            "Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked",
            Error::Framing,
        ),
        ("Content-Length: 4\r\nContent-Length: 5", Error::Framing),
        ("Content-Length: +4", Error::Framing),
        ("Content-Length: 4, 4", Error::Framing),
        ("Content-Length: 99999999999999999999999", Error::Framing),
        ("Content-Length : 4", Error::Header),
        ("X-Folded: a\r\n Content-Length: 4", Error::Header),
        ("no colon here", Error::Header),
        ("Content-Length: 70000", Error::TooLarge),
    ] {
        let raw = format!("HTTP/1.1 200 OK\r\n{headers}\r\n\r\nbody");
        assert_eq!(decode_all_splits(raw.as_bytes()), Err(error), "{headers}");
    }
    // Identical repeats are harmless; long unrelated headers are skipped.
    let raw = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: 4\r\ncontent-length: 4\r\nX-Long: {}\r\n\r\nbody",
        "y".repeat(1000)
    );
    assert_eq!(decode_all_splits(raw.as_bytes()), Ok(b"body".to_vec()));
    // A framing header too long to examine cannot be trusted.
    let raw = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}4\r\n\r\nbody",
        " ".repeat(300)
    );
    assert_eq!(decode(&[raw.as_bytes()]), Err(Error::Framing));
}

#[test]
fn header_and_body_sizes_are_bounded() {
    let mut headers = String::from("HTTP/1.1 200 OK\r\n");
    while headers.len() <= MAX_HEADER_BYTES {
        headers.push_str("X-Filler: 0123456789012345678901234567890123456789\r\n");
    }
    assert_eq!(decode(&[headers.as_bytes()]), Err(Error::Header));
    let mut unframed = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
    unframed.resize(unframed.len() + MAX_BODY + 1, b'x');
    assert_eq!(decode(&[&unframed]), Err(Error::TooLarge));
    // Chunk framing and trailers count too, so they cannot run forever.
    let mut trailers = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n".to_vec();
    while trailers.len() < MAX_BODY + 100 {
        trailers.extend_from_slice(b"X: y\r\n");
    }
    assert_eq!(decode(&[&trailers]), Err(Error::TooLarge));
}

#[test]
fn a_chunked_element_download_streams_into_the_catalog() {
    // CelesTrak could switch framing at any time; both must give one result.
    let mut raw =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n"
            .to_vec();
    for chunk in TLES.as_bytes().chunks(100) {
        raw.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        raw.extend_from_slice(chunk);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"0\r\n\r\n");
    for read in [1, 13, 536, 1460] {
        let mut download = OrbitalDownload::new(START);
        for segment in raw.chunks(read) {
            download.push(segment).unwrap();
        }
        assert!(download.is_complete());
        let mut catalog = Catalog::new();
        assert_eq!(
            catalog.commit(download.finish().unwrap(), 0),
            Ok(SATELLITES.len()),
            "read size {read}"
        );
    }
}
