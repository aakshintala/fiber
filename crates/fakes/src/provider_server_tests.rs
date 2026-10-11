use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use crate::deadline::Deadline;

const READ_WITHIN: Duration = crate::MUST_SUCCEED_WITHIN;

const STATUS_WITHIN: Duration = crate::MUST_SUCCEED_WITHIN;

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
        server.await_requests(1, READ_WITHIN),
        "the request is recorded before the connection drops"
    );
    let (read, body) = Deadline::after(READ_WITHIN)
        .recv(&rx)
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
        server.await_requests(1, READ_WITHIN),
        "the request is recorded while the response is held"
    );
    assert!(
        Deadline::after(Duration::from_secs(2)).recv(&rx).is_err(),
        "the client has no body while the response is held"
    );

    server.release();
    let body = Deadline::after(READ_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("release sends the held body within {READ_WITHIN:?}"));
    assert!(body.ends_with(b"hello"));
}

/// Sends a GET and reads the whole reply on a thread, naming it on `tx`.
fn get_on(addr: SocketAddr, name: &'static str, tx: mpsc::Sender<(&'static str, Vec<u8>)>) {
    thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET /v1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut body = Vec::new();
        stream.read_to_end(&mut body).unwrap();
        tx.send((name, body)).unwrap();
    });
}

#[test]
fn hold_from_answers_earlier_requests_and_holds_the_rest_until_release() {
    let server =
        ProviderServer::start([Response::stream(b"one"), Response::stream(b"two")]).unwrap();
    server.hold_from(2, Duration::from_secs(5));
    let (tx, rx) = mpsc::channel();
    get_on(server.addr, "a", tx.clone());
    let (name, body) = Deadline::after(READ_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("the first request is answered at once"));
    assert!(body.ends_with(b"one"), "{name}");
    get_on(server.addr, "b", tx);
    assert!(server.await_requests(2, READ_WITHIN));
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&rx)
            .is_err(),
        "the second reply is held"
    );
    server.release();
    let (_, body) = Deadline::after(READ_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("release sends the held reply"));
    assert!(body.ends_with(b"two"));
}

#[test]
fn the_default_hold_outlasts_every_test_wait() {
    assert!(HELD_LIMIT >= crate::deadline::WAITS);
}

#[test]
fn a_held_reply_nobody_releases_becomes_a_500_at_its_deadline() {
    let server = ProviderServer::start([Response::stream(b"one")]).unwrap();
    server.hold_from(1, Duration::from_millis(200));
    let (tx, rx) = mpsc::channel();
    get_on(server.addr, "a", tx);
    let (_, body) = Deadline::after(READ_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("the deadline ends the hold"));
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("500"), "{text}");
    assert!(text.contains("never released"), "{text}");
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
        assert!(server.await_requests(if name == "a" { 1 } else { 2 }, READ_WITHIN));
    }
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&rx)
            .is_err(),
        "the second body is still held"
    );

    server.release_one();
    let (first, body) = Deadline::after(READ_WITHIN).recv(&rx).unwrap_or_else(|_| {
        panic!("one held response arrives after release_one within {READ_WITHIN:?}")
    });
    assert!(body.ends_with(b"one") || body.ends_with(b"two"), "{first}");
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&rx)
            .is_err(),
        "the other response is still held"
    );

    server.release_one();
    Deadline::after(READ_WITHIN)
        .recv(&rx)
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
    let mut content_lengths = 0;
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
            "content-length" => {
                content_length = value.trim().to_owned();
                content_lengths += 1;
            }
            "content-type" => content_type = value.trim().to_owned(),
            _ => {}
        }
    }
    assert_eq!(content_length, "100");
    assert_eq!(
        content_lengths, 1,
        "the stall's head carries its one scripted content-length"
    );
    assert_eq!(content_type, "text/plain");
    let mut prefix = vec![0u8; 7];
    reader
        .read_exact(&mut prefix)
        .unwrap_or_else(|e| panic!("the stall's body prefix arrives within {READ_WITHIN:?}: {e}"));
    assert_eq!(prefix, b"partial");
    assert!(
        server.await_partial(1, READ_WITHIN),
        "the partial response was sent"
    );
    assert_eq!(server.requests().len(), 1);
    // The rest of the declared body never arrives while the client holds.
    // Nothing arrives, so the read runs out its bound: a short one.
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut one = [0u8; 1];
    let held = reader.read_exact(&mut one);
    assert!(held.is_err(), "the connection is held past its prefix");
    drop(reader);
    drop(stream);
    assert!(
        server.await_closed(1, READ_WITHIN),
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
    // Past the count the wait is already over, as at it.
    assert!(server.await_closed(0, READ_WITHIN), "zero closes needed");
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
        server.await_partial(1, READ_WITHIN),
        "the one partial arrived"
    );
    // Past the count the wait is already over, as at it.
    assert!(server.await_partial(0, READ_WITHIN), "zero partials needed");
    assert!(
        !server.await_partial(2, Duration::from_millis(200)),
        "no second partial is coming"
    );
    drop(reader);
    drop(stream);
    assert!(
        server.await_closed(1, READ_WITHIN),
        "the client close ends the stall"
    );
}

/// Sends a GET, reads the head and the 7-byte prefix of a stall, and returns
/// both halves of the connection still open: the client is blocked mid-body.
fn stalled_client(addr: SocketAddr) -> (TcpStream, std::io::BufReader<TcpStream>) {
    use std::io::BufRead;
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .unwrap_or_else(|e| panic!("the head line arrives within {READ_WITHIN:?}: {e}"));
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
    }
    let mut prefix = vec![0u8; 7];
    reader
        .read_exact(&mut prefix)
        .unwrap_or_else(|e| panic!("the stall's body prefix arrives within {READ_WITHIN:?}: {e}"));
    (stream, reader)
}

/// Runs one wait on its own thread and receives its answer within
/// `READ_WITHIN`, so a wait that never ends fails the test instead of
/// hanging it.
fn answered_within(
    server: &Arc<ProviderServer>,
    wait: impl FnOnce(&ProviderServer) -> bool + Send + 'static,
) -> bool {
    let (tx, rx) = mpsc::channel();
    let server = Arc::clone(server);
    thread::spawn(move || tx.send(wait(&server)).unwrap());
    Deadline::after(READ_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("the wait returns within {READ_WITHIN:?}"))
}

#[test]
fn waits_count_stalls_already_past_their_event() {
    // Every wait starts after its events have happened, so each answer is
    // decided by its count and comparison alone, never by thread timing.
    let event_within = Duration::from_secs(30);
    let server = Arc::new(
        ProviderServer::start([
            Response::stall(200, b"partial".to_vec(), 100),
            Response::stall(200, b"partial".to_vec(), 100),
        ])
        .unwrap(),
    );

    let (first_stream, first_reader) = stalled_client(server.addr);
    assert!(server.await_partial(1, READ_WITHIN), "one partial arrived");
    assert!(answered_within(&server, move |s| s.await_partial(1, event_within)));

    let (second_stream, second_reader) = stalled_client(server.addr);
    assert!(server.await_partial(2, READ_WITHIN), "two partials arrived");
    assert!(answered_within(&server, move |s| s.await_partial(1, event_within)));
    assert!(answered_within(&server, move |s| s.await_partial(2, event_within)));
    assert!(
        !server.await_partial(3, Duration::from_millis(50)),
        "no third partial is coming"
    );

    drop(first_reader);
    drop(first_stream);
    assert!(server.await_closed(1, READ_WITHIN), "one client closed");
    assert!(answered_within(&server, move |s| s.await_closed(1, event_within)));

    drop(second_reader);
    drop(second_stream);
    assert!(server.await_closed(2, READ_WITHIN), "both clients closed");
    assert!(answered_within(&server, move |s| s.await_closed(2, event_within)));
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
        server.await_requests(1, READ_WITHIN),
        "the request is recorded while the response is held"
    );
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&rx)
            .is_err(),
        "the client has no answer while the response is held"
    );

    server.release();
    let response = Deadline::after(READ_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("release sends the held answer within {READ_WITHIN:?}"));
    assert!(response.ends_with(b"saw 4"), "{response:?}");
}
