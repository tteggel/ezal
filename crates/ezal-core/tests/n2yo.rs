//! The N2YO source: requests, the `tle` reader, and a recorded refresh of
//! every catalogued satellite on one connection. Reader cases are fed whole
//! and in every two-part split, as TLS records may divide them either way.

use ezal_core::catalog::{Catalog, DownloadError, OrbitalDownload};
use ezal_core::http;
use ezal_core::n2yo::{request, valid_key, ReplyError, TleField, MAX_KEY_LEN};
use ezal_core::orbit::SATELLITES;

/// Reply bodies recorded on 2026-11-21, one per line, in `SATELLITES` order.
const REPLIES: &str = include_str!("fixtures/n2yo-2026-11-21.jsonl");
/// 2026-11-21 18:00 UTC, when every recorded element set is fresh.
const NOW: i64 = 1_795_284_000;

/// Read `body` in two parts split at `split`, returning the `tle` string.
fn read(body: &[u8], split: usize) -> Result<Vec<u8>, ReplyError> {
    let mut field = TleField::new();
    let mut lines = Vec::new();
    let (a, b) = body.split_at(split);
    field.push(a, |bytes| lines.extend_from_slice(bytes))?;
    field.push(b, |bytes| lines.extend_from_slice(bytes))?;
    field.finish().map(|()| lines)
}

/// Read whole, then at every split point; all must agree.
fn read_all_splits(body: &str) -> Result<Vec<u8>, ReplyError> {
    let whole = read(body.as_bytes(), 0);
    for split in 0..=body.len() {
        assert_eq!(read(body.as_bytes(), split), whole, "split at {split}");
    }
    whole
}

/// Frame a recorded body as N2YO does: chunked, after a blank line.
fn chunked(body: &str) -> Vec<u8> {
    let body = format!("\r\n{body}");
    let mut raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                    Transfer-Encoding: chunked\r\n\r\n"
        .to_vec();
    for chunk in body.as_bytes().chunks(100) {
        raw.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        raw.extend_from_slice(chunk);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"0\r\n\r\n");
    raw
}

#[test]
fn a_recorded_reply_yields_its_two_element_lines() {
    let body = REPLIES.lines().nth(2).unwrap();
    let lines = read_all_splits(&format!("\r\n{body}")).unwrap();
    assert_eq!(
        String::from_utf8(lines).unwrap(),
        "1 57166U 23091A   26325.55568389 -.00000005  00000-0  16607-4 0  9997\n\
         2 57166  98.5888  17.1460 0005022  77.6488 282.5253 14.24057010176916"
    );
}

#[test]
fn an_error_reply_or_unfinished_string_has_no_elements() {
    assert_eq!(
        read_all_splits(r#"{"error":"Invalid API Key!"}"#),
        Err(ReplyError::NoElements)
    );
    assert_eq!(
        read_all_splits(r#"{"tle":"1 2"#),
        Err(ReplyError::NoElements)
    );
}

#[test]
fn only_the_tle_key_is_read_and_its_escapes_are_decoded() {
    // `"tle"` as a value is not the member; whitespace may surround the colon.
    assert_eq!(
        read_all_splits(r#"{"info":{"satname":"tle"},"tle" : "a\nb\r\t\"\\\/c"}"#).unwrap(),
        b"a\nb\r\t\"\\/c"
    );
    // Escaped quotes inside another member cannot form the key.
    assert_eq!(
        read_all_splits(r#"{"x":"\"tle\":\"no","tle":"yes"}"#).unwrap(),
        b"yes"
    );
}

#[test]
fn a_non_string_control_byte_or_unicode_escape_is_malformed() {
    for body in [
        r#"{"tle":null}"#,
        "{\"tle\":\"a\nb\"}",
        r#"{"tle":"\u0031"}"#,
    ] {
        assert_eq!(
            read_all_splits(body),
            Err(ReplyError::Malformed),
            "{body:?}"
        );
    }
}

#[test]
fn requests_name_the_satellite_and_refuse_keys_that_could_change_the_line() {
    assert_eq!(
        request(57166, "ABC-123").unwrap().as_str(),
        "GET /rest/v1/satellite/tle/57166&apiKey=ABC-123 HTTP/1.1\r\n\
         Host: api.n2yo.com\r\nUser-Agent: ezal\r\nAccept: application/json\r\n\r\n"
    );
    assert!(request(u64::MAX, &"K".repeat(MAX_KEY_LEN)).is_some());
    let long = "K".repeat(MAX_KEY_LEN + 1);
    for key in ["a b", "a\r\nHost: x", "a&b", "a/b", long.as_str()] {
        assert!(!valid_key(key), "{key:?}");
        assert!(request(1, key).is_none(), "{key:?}");
    }
    // A build without a key still compiles; N2YO refuses the request.
    assert!(valid_key(""));
}

#[test]
fn a_refresh_reads_every_satellite_from_one_connection() {
    for read in [1, 13, 536, 1460] {
        let mut download = OrbitalDownload::new(NOW);
        for (index, body) in REPLIES.lines().enumerate() {
            if index > 0 {
                download.next_reply().unwrap();
            }
            for segment in chunked(body).chunks(read) {
                download.push(segment).unwrap();
            }
            assert!(download.is_complete());
        }
        let mut catalog = Catalog::new();
        assert_eq!(
            catalog.commit(download.finish().unwrap(), 0),
            Ok(SATELLITES.len()),
            "read size {read}"
        );
    }
}

#[test]
fn a_refused_or_unfinished_reply_fails_the_whole_refresh() {
    let first = chunked(REPLIES.lines().next().unwrap());
    let refused = chunked(r#"{"error":"Invalid API Key!"}"#);

    // A refusal as the last reply publishes none of the earlier ones.
    let mut download = OrbitalDownload::new(NOW);
    download.push(&first).unwrap();
    download.next_reply().unwrap();
    download.push(&refused).unwrap();
    assert_eq!(
        download.finish().err(),
        Some(DownloadError::Reply(ReplyError::NoElements))
    );

    // A refusal earlier is latched; a good reply afterwards cannot rescue it.
    let mut download = OrbitalDownload::new(NOW);
    download.push(&refused).unwrap();
    assert_eq!(
        download.next_reply(),
        Err(DownloadError::Reply(ReplyError::NoElements))
    );
    assert!(download.push(&first).is_err());
    assert!(download.finish().is_err());

    // The next request cannot follow a reply that has not finished.
    let mut download = OrbitalDownload::new(NOW);
    download.push(&first[..first.len() - 1]).unwrap();
    assert_eq!(
        download.next_reply(),
        Err(DownloadError::Http(http::Error::Truncated))
    );
}
