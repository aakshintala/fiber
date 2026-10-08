use std::io::{Cursor, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;

const READ_WITHIN: Duration = Duration::from_secs(2);

const STATUS_WITHIN: Duration = Duration::from_secs(5);

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
        stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
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
        .unwrap_or_else(|_| panic!("the dropped connection ends the read within {READ_WITHIN:?}"));
    assert!(
        read.is_ok(),
        "the dropped connection ends the read without failing within {READ_WITHIN:?}"
    );
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
        stream.read_to_end(&mut body).unwrap_or_else(|e| {
            panic!("the held body arrives after release within {READ_WITHIN:?}: {e}")
        });
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
        .unwrap_or_else(|_| panic!("release sends the held body within {READ_WITHIN:?}"));
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
            stream.read_to_end(&mut body).unwrap_or_else(|e| {
                panic!("the released body arrives within {READ_WITHIN:?}: {e}")
            });
            tx.send((name, body)).unwrap();
        });
        // One request at a time, so the script's order is known.
        assert!(server.await_requests(if name == "a" { 1 } else { 2 }, Duration::from_secs(2)));
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "the second body is still held"
    );

    server.release_one();
    let (first, body) = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_else(|_| {
        panic!("one held response arrives after release_one within {READ_WITHIN:?}")
    });
    assert!(body.ends_with(b"one") || body.ends_with(b"two"), "{first}");
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "the other response is still held"
    );

    server.release_one();
    rx.recv_timeout(Duration::from_secs(2))
        .unwrap_or_else(|_| panic!("the second release sends the other within {READ_WITHIN:?}"));
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
    stream.set_read_timeout(Some(STATUS_WITHIN)).unwrap();
    stream
        .write_all(b"GET /v1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut text = String::new();
    stream
        .read_to_string(&mut text)
        .unwrap_or_else(|e| panic!("the status reply arrives within {STATUS_WITHIN:?}: {e}"));
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
    stream.set_read_timeout(Some(STATUS_WITHIN)).unwrap();
    write!(stream, "GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap_or_else(|e| {
        panic!("the routed status reply arrives within {STATUS_WITHIN:?}: {e}")
    });
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
    stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut status = String::new();
    reader
        .read_line(&mut status)
        .unwrap_or_else(|e| panic!("the stall's status line arrives within {READ_WITHIN:?}: {e}"));
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
    let mut content_length = String::new();
    let mut content_type = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap_or_else(|e| {
            panic!("the stall's header line arrives within {READ_WITHIN:?}: {e}")
        });
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
    reader
        .read_exact(&mut prefix)
        .unwrap_or_else(|e| panic!("the stall's body prefix arrives within {READ_WITHIN:?}: {e}"));
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
    stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut text = String::new();
    // The server closes after the body: the read ends rather than timing out.
    stream.read_to_string(&mut text).unwrap_or_else(|e| {
        panic!("the short body arrives before the close within {READ_WITHIN:?}: {e}")
    });
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
    stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    // Past the head: the prefix is what the read was blocked on.
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .unwrap_or_else(|e| panic!("the head line arrives within {READ_WITHIN:?}: {e}"));
        head.push_str(&line);
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
    }
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let mut prefix = vec![0u8; 7];
    reader
        .read_exact(&mut prefix)
        .unwrap_or_else(|e| panic!("the stall's body prefix arrives within {READ_WITHIN:?}: {e}"));
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

/// A thread that finishes only once the returned sender drops.
fn held_thread() -> (JoinHandle<()>, mpsc::Sender<()>) {
    let (release, released) = mpsc::channel::<()>();
    let handle = thread::spawn(move || if let Err(mpsc::RecvError) = released.recv() {});
    (handle, release)
}

#[test]
fn stop_gives_up_on_an_accept_thread_that_never_finishes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (accept, release) = held_thread();
    let joined = crate::within(
        "stop to give up on the held thread",
        STATUS_WITHIN,
        move || stop(addr, accept, Duration::from_millis(100)),
    );
    assert!(!joined, "a thread that never finishes is not joined");
    drop(release);
}

#[test]
fn stop_gives_up_when_the_wake_cannot_connect() {
    let addr = crate::refused::ADDR.into();
    let accept = thread::spawn(|| {});
    let joined = crate::within(
        "stop to give up on a refused wake",
        STATUS_WITHIN,
        move || stop(addr, accept, Duration::from_millis(100)),
    );
    assert!(!joined, "a failed connection leaves the thread unjoined");
}

#[test]
fn stop_joins_an_accept_thread_that_sees_stopping() {
    let mut server = ProviderServer::start([]).unwrap();
    lock(&server.state).stopping = true;
    let accept = server.accept.take().unwrap();
    let addr = server.addr;
    let joined = crate::within("stop to join the accept thread", STATUS_WITHIN, move || {
        stop(addr, accept, STATUS_WITHIN)
    });
    assert!(
        joined,
        "waited {STATUS_WITHIN:?} for the accept thread to stop"
    );
}

fn recorded(limit: Option<usize>, count: usize) -> Vec<Request> {
    let mut state = State {
        body_limit: limit,
        ..State::default()
    };
    for n in 0..count {
        let body = format!("body-{n}").into_bytes();
        state.record(Request {
            method: "POST".to_owned(),
            path: "/".to_owned(),
            headers: Vec::new(),
            body_len: body.len(),
            body,
        });
    }
    state.requests
}

#[test]
fn by_default_the_newest_64_bodies_are_kept() {
    let requests = recorded(State::default().body_limit, 65);

    assert!(requests[0].body.is_empty());
    assert_eq!(requests[0].body_len, 6);
    assert_eq!(requests[1].body, b"body-1");
    assert_eq!(requests[64].body, b"body-64");
}

#[test]
fn a_limit_of_zero_keeps_no_bodies() {
    let requests = recorded(Some(0), 2);

    assert!(
        requests
            .iter()
            .all(|r| r.body.is_empty() && r.body_len == 6)
    );
}

#[test]
fn a_limit_past_the_request_count_drops_nothing_and_cannot_overflow() {
    let requests = recorded(Some(usize::MAX), 3);

    assert_eq!(requests[0].body, b"body-0");
}

#[test]
fn no_limit_keeps_every_body() {
    let requests = recorded(None, 70);

    assert_eq!(requests[0].body, b"body-0");
    assert_eq!(requests[69].body, b"body-69");
}

/// Posts `body` and reads the whole response.
fn post(addr: std::net::SocketAddr, body: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
    stream
        .write_all(
            format!(
                "POST /v1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
    stream.write_all(body).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    response
}

#[test]
fn a_responder_answers_each_request_from_its_body() {
    let server = ProviderServer::start_responding(|request: &Request| {
        Response::status(200, format!("saw {}", request.body.len()))
    })
    .unwrap();
    let addr = server.addr;

    let first = post(addr, b"one");
    let second = post(addr, b"three");
    assert!(first.ends_with(b"saw 3"), "{first:?}");
    assert!(second.ends_with(b"saw 5"), "{second:?}");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, b"one");
    assert_eq!(requests[1].body, b"three");
}

#[test]
fn a_responder_builds_a_later_answer_from_an_earlier_request() {
    let server = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if text.contains("Started thing t_1.") {
            Response::status(
                200,
                format!("waits on {}", &text[text.find("t_").unwrap()..][..3]),
            )
        } else {
            Response::status(200, "starts thing t_1.")
        }
    })
    .unwrap();
    let addr = server.addr;

    let first = post(addr, b"begin");
    assert!(first.ends_with(b"starts thing t_1."), "{first:?}");
    let second = post(addr, b"answer: Started thing t_1.");
    assert!(second.ends_with(b"waits on t_1"), "{second:?}");
}

#[test]
fn hold_gates_a_responded_answer_as_it_does_a_scripted_one() {
    let server = ProviderServer::start_responding(|request: &Request| {
        Response::status(200, format!("saw {}", request.body.len()))
    })
    .unwrap();
    server.hold();
    let addr = server.addr;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(post(addr, b"held")).unwrap();
    });

    assert!(
        server.await_requests(1, Duration::from_secs(2)),
        "the request is recorded while the response is held"
    );
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "the client has no answer while the response is held"
    );

    server.release();
    let response = rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_else(|_| panic!("release sends the held answer within {READ_WITHIN:?}"));
    assert!(response.ends_with(b"saw 4"), "{response:?}");
}
