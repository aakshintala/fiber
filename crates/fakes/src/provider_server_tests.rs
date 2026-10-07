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

#[test]
fn release_one_sends_one_held_response_and_keeps_holding_the_rest() {
    let server =
        ProviderServer::start([Response::stream(b"one"), Response::stream(b"two")]).unwrap();
    server.hold();
    let addr = server.addr;
    let (tx, rx) = mpsc::channel();
    for name in ["a", "b"] {
        let tx = tx.clone();
        thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .write_all(b"GET /v1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut body = Vec::new();
            stream.read_to_end(&mut body).unwrap();
            tx.send((name, body)).unwrap();
        });
        // One request at a time, so the script's order is known.
        assert!(server.await_requests(if name == "a" { 1 } else { 2 }, Duration::from_secs(2)));
    }
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());

    server.release_one();
    let (first, body) = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("one response is sent");
    assert!(body.ends_with(b"one") || body.ends_with(b"two"), "{first}");
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "the other response is still held"
    );

    server.release_one();
    rx.recv_timeout(Duration::from_secs(2))
        .expect("the second release sends the other");
}

#[test]
fn a_release_one_before_the_request_lets_it_through() {
    let server = ProviderServer::start([Response::stream(b"hello")]).unwrap();
    server.hold();
    server.release_one();
    assert_eq!(get_status(&server), "200");
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

fn get_status_of(server: &ProviderServer, target: &str) -> String {
    let mut stream = TcpStream::connect(server.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stream, "GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    text.split(' ').nth(1).unwrap().to_owned()
}

#[test]
fn a_routed_response_answers_the_first_request_for_its_path_in_any_order() {
    let server = ProviderServer::start_routed(
        [
            ("/a", Response::status(201, "")),
            ("/b", Response::status(202, "")),
        ],
        Response::status(503, ""),
    )
    .unwrap();

    assert_eq!(get_status_of(&server, "/b?x=1"), "202");
    assert_eq!(get_status_of(&server, "/a"), "201");
    // Each route answers once; a second request and an unrouted path get
    // the fallback.
    assert_eq!(get_status_of(&server, "/a"), "503");
    assert_eq!(get_status_of(&server, "/c"), "503");
}

#[test]
fn a_route_outranks_the_script_and_the_script_serves_unrouted_paths() {
    let server = ProviderServer::start_routed(
        [("/a", Response::status(201, ""))],
        Response::status(503, ""),
    )
    .unwrap();
    lock(&server.state)
        .script
        .push_back(Response::status(200, ""));

    assert_eq!(get_status_of(&server, "/a"), "201");
    assert_eq!(get_status_of(&server, "/z"), "200");
}

#[test]
fn a_stall_sends_its_head_and_prefix_then_holds_until_the_client_closes() {
    use std::io::BufRead;
    let server = ProviderServer::start([
        Response::stall(200, b"partial".to_vec(), 100).header("content-type", "text/plain")
    ])
    .unwrap();
    let mut stream = TcpStream::connect(server.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
    let mut content_length = String::new();
    let mut content_type = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end_matches(['\r', '\n']).to_owned();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => content_length = value.trim().to_owned(),
            "content-type" => content_type = value.trim().to_owned(),
            _ => {}
        }
    }
    assert_eq!(content_length, "100");
    assert_eq!(content_type, "text/plain");
    let mut prefix = vec![0u8; 7];
    reader.read_exact(&mut prefix).unwrap();
    assert_eq!(prefix, b"partial");
    assert!(
        server.await_partial(1, Duration::from_secs(2)),
        "the partial response was sent"
    );
    assert_eq!(server.requests().len(), 1);
    // The rest of the declared body never arrives while the client holds.
    let mut one = [0u8; 1];
    let held = reader.read_exact(&mut one);
    assert!(held.is_err(), "the connection is held past its prefix");
    drop(reader);
    drop(stream);
    assert!(
        server.await_closed(1, Duration::from_secs(2)),
        "the client close ends the stall"
    );
}

#[test]
fn a_scripted_content_length_past_the_body_without_stall_is_sent_and_closed() {
    let server = ProviderServer::start([Response {
        status: 200,
        headers: vec![("content-length".to_owned(), "100".to_owned())],
        body: b"ok".to_vec(),
        drop_connection: false,
        stall: false,
    }])
    .unwrap();
    let mut stream = TcpStream::connect(server.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut text = String::new();
    // The server closes after the body: the read ends rather than timing out.
    stream.read_to_string(&mut text).unwrap();
    assert!(text.ends_with("ok"), "{text}");
    assert!(!server.await_partial(1, Duration::from_millis(200)));
    assert!(!server.await_closed(1, Duration::from_millis(200)));
}

#[test]
fn fewer_than_ends_exactly_at_the_count() {
    assert!(fewer_than(0, 1));
    assert!(!fewer_than(1, 1));
    assert!(!fewer_than(2, 1));
}

#[test]
fn await_partial_needs_every_counted_partial() {
    use std::io::BufRead;
    let server = ProviderServer::start([Response::stall(200, b"partial".to_vec(), 100)]).unwrap();
    let mut stream = TcpStream::connect(server.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    // Past the head: the prefix is what the read was blocked on.
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        head.push_str(&line);
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
    }
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let mut prefix = vec![0u8; 7];
    reader.read_exact(&mut prefix).unwrap();
    assert_eq!(prefix, b"partial");
    assert!(
        server.await_partial(1, Duration::from_secs(2)),
        "the one partial arrived"
    );
    assert!(
        !server.await_partial(2, Duration::from_millis(200)),
        "no second partial is coming"
    );
    drop(reader);
    drop(stream);
    assert!(
        server.await_closed(1, Duration::from_secs(2)),
        "the client close ends the stall"
    );
}
