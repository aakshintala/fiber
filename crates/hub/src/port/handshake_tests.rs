//! Tests for the opening handshake: RFC 6455's key, a browser-shaped
//! request, the refusal table, the origin list, the request cap, EOF, and
//! bytes pipelined past the blank line.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::io::{Cursor, Read};

use base64::engine::general_purpose::STANDARD;

use super::*;

const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
const ORIGIN: &str = "https://a.example";

/// A handshake request with `headers` plus the key, on `method`, `path`
/// and `version`.
fn request(key: &str, headers: &[&str], method: &str, path: &str, version: &str) -> Vec<u8> {
    let mut out = format!("{method} {path} {version}\r\nHost: 127.0.0.1:4040\r\n");
    for header in headers {
        out.push_str(header);
        out.push_str("\r\n");
    }
    out.push_str(&format!("Sec-WebSocket-Key: {key}\r\n"));
    out.push_str("\r\n");
    out.into_bytes()
}

/// The standard websocket headers besides the key.
fn standard() -> [&'static str; 3] {
    [
        "Upgrade: websocket",
        "Connection: Upgrade",
        "Sec-WebSocket-Version: 13",
    ]
}

/// The usual browser-shaped request, with `extra` headers added.
fn browser_with(key: &str, extra: &[&str]) -> Vec<u8> {
    let mut headers: Vec<&str> = standard().to_vec();
    headers.extend(extra);
    request(key, &headers, "GET", "/hub", "HTTP/1.1")
}

/// The usual browser-shaped request.
fn browser(key: &str) -> Vec<u8> {
    browser_with(key, &[])
}

/// Reads the handshake on `bytes` with `origins`.
fn read(bytes: &[u8], origins: &[String]) -> Result<String, Refusal> {
    read_request(&mut Cursor::new(bytes), origins)
}

/// A key decoding to `len` zero bytes.
fn base64_key(len: usize) -> String {
    STANDARD.encode(vec![0u8; len])
}

#[test]
fn rfc_6455_key_gives_rfc_6455_accept() {
    assert_eq!(accept_key(KEY), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}

#[test]
fn a_browser_shaped_request_gives_the_exact_101_response() {
    let accept = read(&browser(KEY), &[]).expect("the browser request is admitted");
    assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    assert_eq!(
        switching(&accept),
        format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: {accept}\r\n\
             \r\n"
        )
        .into_bytes()
    );
}

#[test]
fn mixed_case_names_and_token_lists_are_admitted() {
    let bytes = request(
        KEY,
        &[
            "uPgRaDe: WebSocket",
            "cOnNeCtIoN: keep-alive, Upgrade",
            "sec-websocket-version: 13",
        ],
        "GET",
        "/hub",
        "HTTP/1.1",
    );
    read(&bytes, &[]).expect("mixed case and token lists are admitted");
}

#[test]
fn each_refusal_answers_400() {
    let fifteen = base64_key(15);
    let seventeen = base64_key(17);
    let refusals: Vec<(&str, Vec<u8>)> = vec![
        (
            "POST",
            request(KEY, &standard(), "POST", "/hub", "HTTP/1.1"),
        ),
        (
            "HTTP/1.0",
            request(KEY, &standard(), "GET", "/hub", "HTTP/1.0"),
        ),
        (
            "no Upgrade",
            request(
                KEY,
                &["Connection: Upgrade", "Sec-WebSocket-Version: 13"],
                "GET",
                "/hub",
                "HTTP/1.1",
            ),
        ),
        (
            "Connection without the upgrade token",
            request(
                KEY,
                &[
                    "Upgrade: websocket",
                    "Connection: keep-alive",
                    "Sec-WebSocket-Version: 13",
                ],
                "GET",
                "/hub",
                "HTTP/1.1",
            ),
        ),
        (
            "version 12",
            request(
                KEY,
                &[
                    "Upgrade: websocket",
                    "Connection: Upgrade",
                    "Sec-WebSocket-Version: 12",
                ],
                "GET",
                "/hub",
                "HTTP/1.1",
            ),
        ),
        (
            "no key",
            request(
                "",
                &[
                    "Upgrade: websocket",
                    "Connection: Upgrade",
                    "Sec-WebSocket-Version: 13",
                ],
                "GET",
                "/hub",
                "HTTP/1.1",
            ),
        ),
        ("a key decoding to 15 bytes", browser(&fifteen)),
        ("a key decoding to 17 bytes", browser(&seventeen)),
        (
            "a header line without a colon",
            b"GET /hub HTTP/1.1\r\nno colon here\r\n\r\n".to_vec(),
        ),
    ];
    for (what, bytes) in &refusals {
        assert_eq!(
            read(bytes, &[]),
            Err(Refusal::BadRequest),
            "{what} is refused"
        );
    }
    assert_eq!(
        refused(Refusal::BadRequest),
        b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_vec()
    );
}

#[test]
fn origins_admit_listed_and_refuse_the_rest() {
    let listed = vec![ORIGIN.to_owned()];
    read(&browser_with(KEY, &["Origin: https://a.example"]), &listed)
        .expect("a listed origin is admitted");
    assert_eq!(
        read(&browser_with(KEY, &["Origin: https://b.example"]), &listed),
        Err(Refusal::Forbidden),
        "an unlisted origin is refused"
    );
    assert_eq!(
        read(
            &browser_with(
                KEY,
                &["Origin: https://a.example", "Origin: https://b.example"]
            ),
            &listed
        ),
        Err(Refusal::Forbidden),
        "a listed origin plus an unlisted one is refused"
    );
    for origin in ["https://a.example/", "HTTPS://a.example"] {
        let header = format!("Origin: {origin}");
        assert_eq!(
            read(&browser_with(KEY, &[header.as_str()]), &listed),
            Err(Refusal::Forbidden),
            "{origin} matches nothing without normalisation"
        );
    }
    assert_eq!(
        refused(Refusal::Forbidden),
        b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_vec()
    );
}

#[test]
fn requests_at_the_cap_are_admitted_and_past_it_refused() {
    assert!(read(&sized(HANDSHAKE_MAX - 1), &[]).is_ok());
    assert!(read(&sized(HANDSHAKE_MAX), &[]).is_ok());
    assert_eq!(
        read(&sized(HANDSHAKE_MAX + 1), &[]),
        Err(Refusal::BadRequest)
    );
}

/// A well-formed request of exactly `size` bytes, padded through the path.
fn sized(size: u64) -> Vec<u8> {
    let prefix = "GET /";
    let suffix = " HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
    let pad = usize::try_from(size - u64::try_from(prefix.len() + suffix.len()).unwrap()).unwrap();
    let mut out = prefix.as_bytes().to_vec();
    out.extend(std::iter::repeat_n(b'a', pad));
    out.extend_from_slice(suffix.as_bytes());
    assert_eq!(
        u64::try_from(out.len()).unwrap(),
        size,
        "the request is {size} bytes"
    );
    out
}

#[test]
fn eof_before_the_blank_line_is_400() {
    assert_eq!(
        read(b"GET /hub HTTP/1.1\r\nUpgrade: websocket\r\n", &[]),
        Err(Refusal::BadRequest)
    );
    assert_eq!(read(b"", &[]), Err(Refusal::BadRequest));
}

#[test]
fn bytes_pipelined_past_the_blank_line_stay_readable() {
    let mut bytes = browser(KEY);
    bytes.extend_from_slice(b"\x81\x85pipelined");
    let mut stream = Cursor::new(bytes);
    read_request(&mut stream, &[]).expect("the handshake reads");
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, b"\x81\x85pipelined");
}
