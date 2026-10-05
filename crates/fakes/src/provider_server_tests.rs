use std::io::{Cursor, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;

fn decode(bytes: &[u8]) -> (Result<(), Malformed>, Vec<u8>, String) {
    let mut reader = Cursor::new(bytes.to_vec());
    let mut body = Vec::new();
    let result = read_chunked(&mut reader, &mut body);
    let mut rest = String::new();
    reader.read_to_string(&mut rest).unwrap();
    (result, body, rest)
}

#[test]
fn a_chunked_body_is_decoded_and_its_trailers_consumed() {
    let (result, body, rest) =
        decode(b"3\r\nabc\r\n2;ext=1\r\nde\r\n0\r\nx-trailer: t\r\n\r\nNEXT");

    assert_eq!(result, Ok(()));
    assert_eq!(body, b"abcde");
    assert_eq!(rest, "NEXT");
}

#[test]
fn malformed_or_truncated_chunked_framing_is_rejected_naming_why() {
    let cases: [(&[u8], Malformed); 7] = [
        (
            b"3\r\nabcX\r\n0\r\n\r\n",
            "a chunk's data is not followed by CRLF",
        ),
        (
            b"zz\r\nabc\r\n0\r\n\r\n",
            "a chunk size is not a hex number",
        ),
        (
            b"3\nabc\r\n0\r\n\r\n",
            "a chunked framing line does not end in CRLF",
        ),
        (b"5\r\nab", TRUNCATED),
        (b"3\r\nabc\r\n", TRUNCATED),
        (b"3\r\nabc\r\n0\r\n", TRUNCATED),
        (b"3\r\nabc\r\n0\r\n\r", TRUNCATED),
    ];
    for (bytes, why) in cases {
        let (result, _, _) = decode(bytes);
        assert_eq!(result, Err(why), "{}", String::from_utf8_lossy(bytes));
    }
}

#[test]
fn a_rejected_body_keeps_what_decoded_before_the_fault() {
    let (_, body, _) = decode(b"3\r\nabc\r\n4\r\nde");

    assert_eq!(body, b"abcde");
}

#[test]
fn a_dropped_connection_records_the_request_and_answers_nothing() {
    let server = ProviderServer::start([Response::drop_connection()]).unwrap();
    let addr = server.addr;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(b"POST /v1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut body = Vec::new();
        let read = stream.read_to_end(&mut body);
        tx.send((read.map(|_| ()), body)).unwrap();
    });

    assert!(
        server.await_requests(1, Duration::from_secs(2)),
        "the request is recorded before the connection drops"
    );
    let (read, body) = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the dropped connection ends the read");
    assert!(read.is_ok(), "the read ends, it does not fail");
    assert!(body.is_empty(), "no response follows the request");
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn hold_records_the_request_and_sends_the_body_only_after_release() {
    let server = ProviderServer::start([Response::stream(b"hello")]).unwrap();
    server.hold();
    let addr = server.addr;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"POST /v1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut body = Vec::new();
        stream.read_to_end(&mut body).unwrap();
        tx.send(body).unwrap();
    });

    assert!(
        server.await_requests(1, Duration::from_secs(2)),
        "the request is recorded while the response is held"
    );
    assert!(
        rx.recv_timeout(Duration::from_secs(2)).is_err(),
        "the client has no body while the response is held"
    );

    server.release();
    let body = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("release sends the body");
    assert!(body.ends_with(b"hello"));
}

/// The status line's code of a bodiless GET answered by `server`.
fn get_status(server: &ProviderServer) -> String {
    let mut stream = TcpStream::connect(server.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"GET /v1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    text.split(' ').nth(1).unwrap().to_owned()
}

#[test]
fn a_request_past_the_script_gets_the_fallback_or_the_default_500() {
    let with = ProviderServer::start_with_fallback([], Response::status(503, "")).unwrap();
    assert_eq!(get_status(&with), "503");
    assert_eq!(get_status(&with), "503");

    let without = ProviderServer::start([Response::status(200, "")]).unwrap();
    assert_eq!(get_status(&without), "200");
    assert_eq!(get_status(&without), "500");
}
