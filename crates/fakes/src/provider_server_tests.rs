use crate::MUST_SUCCEED_WITHIN;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use super::*;
use crate::clock::FakeClock;
use crate::deadline::Deadline;
use crate::http_server::{CheckGate, Pause};
use contract::clock::Wake;

const READ_WITHIN: Duration = crate::MUST_SUCCEED_WITHIN;

const STATUS_WITHIN: Duration = crate::MUST_SUCCEED_WITHIN;

/// How long the stalled connection is read for when nothing arrives.
const ABSENT: Duration = Duration::from_secs(1);

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
        server.await_requests(1, Deadline::after(READ_WITHIN)),
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
        stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
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
        server.await_requests(1, Deadline::after(READ_WITHIN)),
        "the request is recorded while the response is held"
    );
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&rx)
            .is_err(),
        "the client has no body while the response is held"
    );

    server.release();
    let body = Deadline::after(READ_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("release sends the held body within {READ_WITHIN:?}"));
    assert!(body.ends_with(b"hello"));
}

/// Sends a GET and reads the whole reply on a thread, naming it on `tx`.
/// The read is bounded, so a failure ends the thread instead of hanging
/// the test's teardown.
fn get_on(addr: SocketAddr, name: &'static str, tx: mpsc::Sender<(&'static str, Vec<u8>)>) {
    thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
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
    server.hold_from(2, MUST_SUCCEED_WITHIN);
    let (tx, rx) = mpsc::channel();
    get_on(server.addr, "a", tx.clone());
    let (name, body) = Deadline::after(READ_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("the first request is answered at once"));
    assert!(body.ends_with(b"one"), "{name}");
    get_on(server.addr, "b", tx);
    assert!(server.await_requests(2, Deadline::after(READ_WITHIN)));
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
    let clock = FakeClock::new();
    let server = ProviderServer::start_on(clock.clone(), vec![Response::stream(b"one")]).unwrap();
    server.hold_from(1, Duration::from_secs(5));
    let acks = server.test_acknowledgments();
    let (tx, rx) = mpsc::channel();
    get_on(server.addr, "a", tx);
    assert!(
        server.await_requests(1, Deadline::after(READ_WITHIN)),
        "the held request is recorded"
    );
    // The hold waits on the injected clock: the test advances only past
    // its announced wait.
    let ack = Deadline::after(READ_WITHIN).recv_or_fail(&acks, "the held response's wait");
    assert_eq!(ack.phase, "to wait out the held response");
    assert!(
        clock.await_thread_parked(ack.thread, ack.until, Deadline::after(READ_WITHIN)),
        "the hold parks on the clock before it moves"
    );
    clock.advance(Duration::from_secs(5));
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
            stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
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
        assert!(server.await_requests(
            if name == "a" { 1 } else { 2 },
            Deadline::after(READ_WITHIN)
        ));
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
        server.await_partial(1, Deadline::after(READ_WITHIN)),
        "the partial response was sent"
    );
    assert_eq!(server.requests().len(), 1);
    // The rest of the declared body never arrives while the client holds.
    // Nothing arrives, so the read runs out its bound: a short one.
    stream.set_read_timeout(Some(ABSENT)).unwrap();
    let mut one = [0u8; 1];
    let held = reader.read_exact(&mut one);
    assert!(held.is_err(), "the connection is held past its prefix");
    drop(reader);
    drop(stream);
    assert!(
        server.await_closed(1, Deadline::after(READ_WITHIN)),
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
    assert!(!server.await_partial(1, Deadline::after(Duration::from_millis(200))));
    assert!(!server.await_closed(1, Deadline::after(Duration::from_millis(200))));
    // Past the count the wait is already over, as at it.
    assert!(
        server.await_closed(0, Deadline::after(READ_WITHIN)),
        "zero closes needed"
    );
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
        server.await_partial(1, Deadline::after(READ_WITHIN)),
        "the one partial arrived"
    );
    // Past the count the wait is already over, as at it.
    assert!(
        server.await_partial(0, Deadline::after(READ_WITHIN)),
        "zero partials needed"
    );
    assert!(
        !server.await_partial(2, Deadline::after(Duration::from_millis(200))),
        "no second partial is coming"
    );
    drop(reader);
    drop(stream);
    assert!(
        server.await_closed(1, Deadline::after(READ_WITHIN)),
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
/// `MUST_SUCCEED_WITHIN`, so a wait that never ends fails the test instead of
/// hanging it.
fn answered_within(
    server: &Arc<ProviderServer>,
    wait: impl FnOnce(&ProviderServer) -> bool + Send + 'static,
) -> bool {
    let (tx, rx) = mpsc::channel();
    let server = Arc::clone(server);
    thread::spawn(move || tx.send(wait(&server)).unwrap());
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("the wait returns within {MUST_SUCCEED_WITHIN:?}"))
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
    assert!(
        server.await_partial(1, Deadline::after(READ_WITHIN)),
        "one partial arrived"
    );
    assert!(answered_within(&server, move |s| s
        .await_partial(1, Deadline::after(event_within))));

    let (second_stream, second_reader) = stalled_client(server.addr);
    assert!(
        server.await_partial(2, Deadline::after(READ_WITHIN)),
        "two partials arrived"
    );
    assert!(answered_within(&server, move |s| s
        .await_partial(1, Deadline::after(event_within))));
    assert!(answered_within(&server, move |s| s
        .await_partial(2, Deadline::after(event_within))));
    assert!(
        !server.await_partial(3, Deadline::after(Duration::from_millis(50))),
        "no third partial is coming"
    );

    drop(first_reader);
    drop(first_stream);
    assert!(
        server.await_closed(1, Deadline::after(READ_WITHIN)),
        "one client closed"
    );
    assert!(answered_within(&server, move |s| s
        .await_closed(1, Deadline::after(event_within))));

    drop(second_reader);
    drop(second_stream);
    assert!(
        server.await_closed(2, Deadline::after(READ_WITHIN)),
        "both clients closed"
    );
    assert!(answered_within(&server, move |s| s
        .await_closed(2, Deadline::after(event_within))));
}

/// Idle windows never expire the provider: with no client for longer
/// than a request bound, the next request is still served in order.
#[test]
fn idle_windows_do_not_expire_the_provider() {
    let clock = FakeClock::new();
    let server =
        ProviderServer::start_on(clock.clone(), vec![Response::status(200, "ok")]).unwrap();
    // Longer than a request bound with no client: idle windows renew,
    // and no missing request fails.
    clock.advance(Duration::from_secs(31));
    assert_eq!(get_status(&server), "200");
    assert_eq!(server.requests().len(), 1);
    assert_eq!(get_status(&server), "500");
}

/// An incomplete request never blocks another connection: a client
/// holding a partial head open does not stall a complete request.
#[test]
fn an_incomplete_request_does_not_block_another_connection() {
    let server = ProviderServer::start([Response::status(200, "ok")]).unwrap();
    let mut partial = TcpStream::connect(server.addr).unwrap();
    partial
        .write_all(b"POST /v1 HTTP/1.1\r\nHost: localhost\r\n")
        .unwrap();
    // The partial head stays open and unanswered while a complete
    // request goes through.
    assert_eq!(get_status(&server), "200");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    drop(partial);
}

/// Dropping the server releases held responses and reaps stalled ones:
/// every client gets what the server promised before the port closes.
#[test]
fn dropping_releases_holds_and_reaps_stalls() {
    let server = Arc::new(
        ProviderServer::start([
            Response::stall(200, b"partial".to_vec(), 100),
            Response::stream(b"two"),
        ])
        .unwrap(),
    );
    server.hold();
    let addr = server.addr;
    let (tx, rx) = mpsc::channel();
    for name in ["stalled", "streamed"] {
        let tx = tx.clone();
        thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut body = Vec::new();
            stream.read_to_end(&mut body).unwrap_or_else(|e| {
                panic!("the released body arrives within {READ_WITHIN:?}: {e}")
            });
            tx.send((name, body)).unwrap();
        });
    }
    assert!(
        server.await_requests(2, Deadline::after(READ_WITHIN)),
        "both requests are recorded while held"
    );
    drop(server);
    // Either connection may take either scripted response: the script
    // pops in arrival order, which thread launch order does not fix.
    let mut bodies = [
        Deadline::after(READ_WITHIN)
            .recv(&rx)
            .unwrap_or_else(|_| panic!("the drop releases the first within {READ_WITHIN:?}"))
            .1,
        Deadline::after(READ_WITHIN)
            .recv(&rx)
            .unwrap_or_else(|_| panic!("the drop releases the second within {READ_WITHIN:?}"))
            .1,
    ];
    bodies.sort();
    assert!(
        bodies.iter().any(|body| body.ends_with(b"two")),
        "the streamed reply arrives whole"
    );
    assert!(
        bodies.iter().any(|body| body.ends_with(b"partial")),
        "the stalled reply sent its prefix"
    );
}

/// Dropping an idle server closes its port: the drop joins the serving
/// threads instead of hanging on them.
#[test]
fn dropping_an_idle_server_closes_its_port() {
    let server = ProviderServer::start([Response::status(200, "ok")]).unwrap();
    let probe = server.core.drop_probe();
    drop(server);
    assert!(
        probe.listener_closed(),
        "the dropped server closed its listener"
    );
    assert!(
        probe.accept_joined(),
        "the dropped server joined its accept thread"
    );
}

/// Dropping the server gives up on a responder that never returns: the
/// drop ends within its bound instead of hanging on the callback, and
/// the port still closes.
#[test]
fn dropping_gives_up_on_a_responder_that_never_returns() {
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let server = ProviderServer::start_responding(move |_: &Request| {
        // The record is pushed under the state lock just before the
        // responder runs under it, so entering here proves the request
        // is recorded without taking the lock the responder holds.
        match started_tx.send(()) {
            Ok(()) | Err(_) => {}
        }
        match release_rx.lock().unwrap().recv() {
            Ok(()) | Err(_) => {}
        }
        Response::status(200, "late")
    })
    .unwrap();
    let addr = server.addr;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
        stream
            .write_all(b"GET /v1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut body = Vec::new();
        match stream.read_to_end(&mut body) {
            Ok(_) | Err(_) => {}
        }
        match tx.send(body) {
            Ok(()) | Err(_) => {}
        }
    });
    Deadline::after(READ_WITHIN).recv_or_fail(&started_rx, "the blocked responder");
    // The drop ends while the responder is still blocked: it gives the
    // callback up instead of hanging on it. The probe is taken before
    // the server moves into the dropper thread; it holds the shared
    // core, so it stays valid past the drop.
    let probe = server.core.drop_probe();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        drop(server);
        match done_tx.send(()) {
            Ok(()) | Err(_) => {}
        }
    });
    Deadline::after(READ_WITHIN).recv_or_fail(&done_rx, "the drop to give up on the responder");
    assert!(
        probe.listener_closed(),
        "the dropped server closed its listener"
    );
    assert!(
        !probe.accept_joined(),
        "the drop gives up on the stuck responder instead of joining it"
    );
    drop(release_tx);
    match Deadline::after(READ_WITHIN).recv(&rx) {
        Ok(_) | Err(_) => {}
    }
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
fn dropped_bodies_keep_the_count_and_the_lengths_but_not_the_bytes() {
    let server =
        ProviderServer::start([Response::status(200, "ok"), Response::status(200, "ok")]).unwrap();
    server.drop_bodies();
    let addr = server.addr;

    let first = vec![b'a'; 1_000_000];
    let second = vec![b'b'; 2_000_000];
    post(addr, &first);
    post(addr, &second);
    assert!(
        server.await_requests(2, Deadline::after(READ_WITHIN)),
        "both requests are recorded"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body_len, first.len());
    assert_eq!(requests[1].body_len, second.len());
    assert!(
        requests.iter().all(|request| request.body.is_empty()),
        "no stored body holds its bytes"
    );
}

#[test]
fn a_responder_sees_the_full_body_while_dropped_bodies_store_none() {
    let server = ProviderServer::start_responding(|request: &Request| {
        Response::status(200, format!("saw {}", request.body.len()))
    })
    .unwrap();
    server.drop_bodies();
    let addr = server.addr;

    let body = vec![b'a'; 100_000];
    let response = post(addr, &body);
    assert!(response.ends_with(b"saw 100000"), "{response:?}");
    assert!(
        server.await_requests(1, Deadline::after(READ_WITHIN)),
        "the request is recorded"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body_len, body.len());
    assert!(
        requests[0].body.is_empty(),
        "the stored body holds no bytes"
    );
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
        server.await_requests(1, Deadline::after(READ_WITHIN)),
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

/// A hold release pins every side: no hold goes with no permit, a hold
/// waits with no permit, and a permit lets a held response go.
#[test]
fn a_hold_release_pins_every_side() {
    assert!(
        hold_released(false, 0),
        "a released hold goes with no permit"
    );
    assert!(
        !hold_released(true, 0),
        "a held response waits with no permit"
    );
    assert!(hold_released(true, 1), "a permit lets one held response go");
    assert!(
        hold_released(false, 1),
        "a released hold with a permit goes"
    );
}

/// A pipelined discard pins every side: only a declared fixed body
/// without transfer-encoding drains already-arrived pipelined bytes.
#[test]
fn a_pipelined_discard_pins_every_side() {
    let content_length = ("content-length".to_string(), "4".to_string());
    let transfer_encoding = ("transfer-encoding".to_string(), "chunked".to_string());
    let host = ("host".to_string(), "localhost".to_string());
    assert!(
        discards_pipelined(std::slice::from_ref(&content_length)),
        "a declared fixed body discards pipelined bytes"
    );
    assert!(
        !discards_pipelined(&[content_length.clone(), transfer_encoding.clone()]),
        "a chunked declared body never drains"
    );
    assert!(
        !discards_pipelined(std::slice::from_ref(&transfer_encoding)),
        "a chunked body with no declared length never drains"
    );
    assert!(
        !discards_pipelined(&[host]),
        "a request with no declared body never drains"
    );
}

// An injected clock that counts monotonic reads, wrapping [`FakeClock`]:
// the regression test below proves the fixed-body reader checks the
// bound once per fill rather than once per byte by counting the clock
// reads one request costs. Every clock delegation keeps the fake's
// semantics; only `now` adds the count.
struct CountingClock {
    inner: Arc<FakeClock>,
    reads: AtomicU64,
}

impl CountingClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: FakeClock::new(),
            reads: AtomicU64::new(0),
        })
    }

    fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }
}

impl Clock for CountingClock {
    fn now(&self) -> Instant {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.now()
    }

    fn wall(&self) -> SystemTime {
        self.inner.wall()
    }

    fn sleep(&self, d: Duration) {
        self.inner.sleep(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        self.inner.wait_until(until, wait);
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        self.inner.subscribe(waker);
    }
}

/// The first offset where `recorded` and `sent` differ, or `None` when
/// their shared prefix matches: a large-body assertion reports the length
/// and this offset instead of printing both payloads whole
/// (`docs/testing.md`, "What a test asserts").
fn first_diff(recorded: &[u8], sent: &[u8]) -> Option<usize> {
    recorded
        .iter()
        .zip(sent.iter())
        .position(|(recorded, sent)| recorded != sent)
}

/// Reads `stream` to its close on a thread and receives the whole reply
/// through one named wall-clock deadline: continued partial progress
/// fails the test at the deadline instead of reaching the harness kill.
fn read_reply_within(stream: TcpStream, what: &'static str) -> Vec<u8> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stream = stream;
        let mut reply = Vec::new();
        stream
            .read_to_end(&mut reply)
            .unwrap_or_else(|e| panic!("{what} within {READ_WITHIN:?}: {e}"));
        tx.send(reply).unwrap();
    });
    Deadline::after(READ_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("{what} within {READ_WITHIN:?}"))
}

/// Runs one bench-shaped exchange (connect, head and body writes, reply
/// read to the server's close) on a worker thread and receives the whole
/// reply through one named wall-clock deadline: continued partial progress
/// in any step fails the test at the deadline instead of reaching the
/// harness kill.
fn exchange_within(addr: SocketAddr, head: Vec<u8>, body: Vec<u8>, what: &'static str) -> Vec<u8> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stream = TcpStream::connect_timeout(&addr, READ_WITHIN)
            .unwrap_or_else(|e| panic!("{what} within {READ_WITHIN:?}: {e}"));
        stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
        stream.set_write_timeout(Some(READ_WITHIN)).unwrap();
        stream
            .write_all(&head)
            .unwrap_or_else(|e| panic!("{what} within {READ_WITHIN:?}: {e}"));
        stream
            .write_all(&body)
            .unwrap_or_else(|e| panic!("{what} within {READ_WITHIN:?}: {e}"));
        let mut reply = Vec::new();
        stream
            .read_to_end(&mut reply)
            .unwrap_or_else(|e| panic!("{what} within {READ_WITHIN:?}: {e}"));
        tx.send(reply).unwrap();
    });
    Deadline::after(READ_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("{what} within {READ_WITHIN:?}"))
}

// One bench-shaped POST: a large fixed-length body on a fresh connection,
// answered at once, like each of a busy turn's model calls over its own
// `connection: close` connection. The reply completes with the body
// byte-exact, and the frozen injected clock sees only a handful of reads:
// the bulk reader checks the bound once per fill, while the per-byte
// reader it replaced read the clock once per body byte (100_003 here),
// the tax a busy turn paid on every request after #2220.
#[test]
fn a_large_fixed_body_costs_one_bound_check_per_fill_not_per_byte() {
    const BODY_LEN: usize = 100_003;
    // One check per 8 KiB fill plus the head's bytes is far below this;
    // one check per body byte exceeds it twentyfold.
    const NOW_BOUND: u64 = 4_096;
    let clock = CountingClock::new();
    let server =
        ProviderServer::start_on(clock.clone(), vec![Response::status(200, "ok")]).unwrap();
    let addr = server.addr;

    let body: Vec<u8> = (0u8..=250).cycle().take(BODY_LEN).collect();
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
    stream
        .write_all(
            format!("POST /v1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: {BODY_LEN}\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    stream.write_all(&body).unwrap();
    // The write side stays open until the reply completes, like the
    // bench's client on its fresh connection per call: a reader that
    // reads past the body waits on the peer instead of answering, so an
    // over-read fails below instead of ending at the reply.
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .unwrap_or_else(|e| panic!("the scripted reply arrives within {READ_WITHIN:?}: {e}"));
    // The reply completed: only now does the client close.
    drop(stream);
    let text = String::from_utf8_lossy(&response);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(response.ends_with(b"ok"), "{text}");

    assert!(
        server.await_requests(1, Deadline::after(READ_WITHIN)),
        "the request is recorded"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body_len, BODY_LEN);
    assert_eq!(
        requests[0].body.len(),
        body.len(),
        "the recorded body has the sent length"
    );
    assert_eq!(
        first_diff(&requests[0].body, &body),
        None,
        "the recorded body is byte-exact"
    );
    // The requests lock synchronises with the serving thread, so every
    // clock read the request cost is counted here.
    let reads = clock.reads();
    assert!(
        reads <= NOW_BOUND,
        "one request costs {reads} injected-clock reads, at most {NOW_BOUND}: the bound is checked per fill, not per byte"
    );
}

// A fixed body followed by pipelined bytes keeps only its declared
// length. The post-reply seam lets the test send bytes after the reply is
// complete, then waits for a peek that proves they reached the socket while
// leaving them unread for the close path. No scheduling or elapsed-time
// assumption decides the result.
#[test]
fn a_fixed_body_followed_by_pipelined_bytes_keeps_only_its_length() {
    const BODY_LEN: usize = 20_000;
    let clock = FakeClock::new();
    let server = ProviderServer::start_on(clock, vec![Response::status(200, "ok")]).unwrap();
    let (pause, control) = Pause::new();
    Connection::set_test_post_reply_pause(pause);
    let addr = server.addr;

    let body: Vec<u8> = (0u8..=250).cycle().take(BODY_LEN).collect();
    let pipelined = b"GET /next HTTP/1.1\r\nHost: localhost\r\n\r\n";
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
    stream
        .write_all(
            format!("POST /v1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: {BODY_LEN}\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    stream.write_all(&body).unwrap();
    assert!(
        server.await_requests(1, Deadline::after(READ_WITHIN)),
        "the request is recorded"
    );

    let deadline = Deadline::after(READ_WITHIN);
    control.wait_hit(&deadline, "the post-reply seam");
    stream.write_all(pipelined).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    control.wait_hit(&deadline, "the server's peek of pipelined bytes");
    control.release();

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .unwrap_or_else(|e| panic!("the scripted reply arrives within {READ_WITHIN:?}: {e}"));
    let text = String::from_utf8_lossy(&response);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(response.ends_with(b"ok"), "{text}");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body_len, BODY_LEN);
    assert_eq!(
        requests[0].body.len(),
        body.len(),
        "the recorded body has the sent length"
    );
    assert_eq!(
        first_diff(&requests[0].body, &body),
        None,
        "the recorded body holds its declared bytes, not the pipelined ones"
    );
}

/// Releases the armed check-to-park gate when the test unwinds past
/// it: a waiter held between its check and its park would otherwise
/// outlive the failed test, and shutdown does not release this gate,
/// so an earlier assertion or receive panic would leave server threads
/// held indefinitely. On success the test releases the gate before
/// shutdown, and this drops as a no-op backstop.
struct ReleaseOnDrop {
    gate: Arc<CheckGate>,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.gate.release();
    }
}

// Twenty bench-shaped POSTs: byte-slice bodies of growing length, each
// on a fresh connection read to the server's close, against small stream
// replies. The bodies wait ready-built before the server starts, so no
// client think time parks the server idle. Every park is held in the
// test gate and staged: a data park is passed only after its bytes are
// written, so it ends on readiness by construction. The client withholds
// its FIN until the server's post-reply drain park, then writes it and
// passes the park, so the drain ends on readiness too and the count
// includes one drain per call. The readiness counter, which accepts never
// touch, is read once after shutdown joins every connection thread, and
// equals every iteration's passed parks in total. The poll bound outlasts
// every receive's deadline, so a staged park never ends at its bound
// before loopback delivers its bytes. A data park that ignores readiness
// sleeps its bound past the bytes instead, and the count stays short.
#[test]
fn busy_shaped_posts_sleep_only_when_idle() {
    const CALLS: usize = 20;
    const STEP: usize = 2_000;
    const ACCEPT: &str = "to accept the connection";
    const HEAD: &str = "to read the request line";
    const BODY: &str = "to read the fixed body";
    // Not a server phase: the reader's end, merged into the held stream.
    const REPLY: &str = "the reply read to its close";
    // The server's post-reply drain park, held like every other park.
    const DRAIN: &str = "to drain the full reply";
    let clock = FakeClock::new();
    let script: Vec<Response> = (0..CALLS)
        .map(|_| Response::stream("event: done\ndata: {}\n\n"))
        .collect();
    // Built before the server starts, so the server never parks on the
    // client building its next body.
    let bodies: Vec<Vec<u8>> = (0..CALLS)
        .map(|n| (0u8..=250).cycle().take((n + 1) * STEP).collect())
        .collect();
    let core = HttpServer::bind(
        clock.clone() as Arc<dyn Clock>,
        Limits {
            poll: Duration::from_secs(60),
            ..Limits::default()
        },
    )
    .unwrap();
    // Armed before the first window, so every park holds from the start.
    let gate = core.test_gate();
    gate.arm();
    let held = gate
        .held_notes()
        .expect("the test takes the held announcements");
    let reply_tx = gate.held_sender();
    let server = ProviderServer::start_on_core(clock.clone(), core, script);
    let _release = ReleaseOnDrop {
        gate: Arc::clone(&gate),
    };
    let addr = server.addr;
    let next = |deadline: &Deadline, what: &str| deadline.recv_or_fail(&held, what).0;
    let deadline = Deadline::after(READ_WITHIN);
    assert_eq!(
        next(&deadline, "the first accept park"),
        ACCEPT,
        "nothing has connected"
    );
    let readied_at_start = server.core.test_readied_waits();
    let mut total_passed = 0u64;
    for body in bodies.iter() {
        let mut stream = TcpStream::connect_timeout(&addr, READ_WITHIN).unwrap();
        stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
        stream.set_write_timeout(Some(READ_WITHIN)).unwrap();
        // The held accept takes the pending connection, which parks for
        // its head, and the next window parks for the next accept, in
        // either order.
        gate.pass(ACCEPT);
        // One wait for both parks, so one deadline spans both receives.
        let deadline = Deadline::after(READ_WITHIN);
        let mut parked = [
            next(&deadline, "the head park or the next accept park"),
            next(&deadline, "the other of the two"),
        ];
        parked.sort_unstable();
        assert_eq!(parked, [ACCEPT, HEAD], "one accept and one head park");
        stream
            .write_all(
                format!(
                    "POST /v1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap_or_else(|e| panic!("the request writes within {READ_WITHIN:?}: {e}"));
        gate.pass(HEAD);
        // No body byte is on the wire yet, so the body read parks.
        let deadline = Deadline::after(READ_WITHIN);
        assert_eq!(
            next(&deadline, "the body park"),
            BODY,
            "the whole head arrives first"
        );
        stream
            .write_all(body)
            .unwrap_or_else(|e| panic!("the request writes within {READ_WITHIN:?}: {e}"));
        gate.pass(BODY);
        let mut passed = 2u64;
        let response = thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let mut response = Vec::new();
                (&stream).read_to_end(&mut response).unwrap_or_else(|e| {
                    panic!("the scripted reply arrives within {READ_WITHIN:?}: {e}")
                });
                reply_tx.send((REPLY, 0)).unwrap();
                response
            });
            // A body larger than one read can outrun loopback delivery:
            // each further park comes after all its bytes were written,
            // so it is passed and counted like the first. The drain park
            // and the reply arrive in either order: the FIN is written
            // only after the drain park holds, then passed, so the drain
            // ends on readiness and is counted once for the call.
            let mut replied = false;
            let mut drained = false;
            loop {
                // Each pass answers the next receive, so each receive is
                // its own wait with its own deadline.
                let deadline = Deadline::after(READ_WITHIN);
                let phase = next(
                    &deadline,
                    "the rest of the body's parks, the drain park or the reply",
                );
                if phase == BODY {
                    assert!(!drained, "no body park follows the drain park, got {phase}");
                    gate.pass(BODY);
                    passed += 1;
                } else if phase == REPLY {
                    assert!(!replied, "the reply arrives once, got {phase}");
                    replied = true;
                } else if phase == DRAIN {
                    assert!(!drained, "the drain parks once, got {phase}");
                    // The drain read parked on an empty socket, so the
                    // FIN written here ends that park on readiness.
                    stream.shutdown(Shutdown::Write).unwrap();
                    gate.pass(DRAIN);
                    passed += 1;
                    drained = true;
                } else {
                    panic!("only BODY, DRAIN or REPLY follow the body pass, got {phase}");
                }
                if replied && drained {
                    break;
                }
            }
            reader.join().unwrap()
        });
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(response.ends_with(b"{}\n\n"), "{text}");
        total_passed += passed;
    }
    assert!(
        server.await_requests(CALLS, Deadline::after(READ_WITHIN)),
        "every request is recorded"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), CALLS);
    for (request, expected) in requests.iter().zip(bodies.iter()) {
        assert_eq!(request.body_len, expected.len());
        assert_eq!(
            first_diff(&request.body, expected),
            None,
            "a recorded body is byte-exact"
        );
    }
    // The last window holds its accept park, announced with the last
    // connection: released, it ends stopped at shutdown.
    gate.release();
    assert!(server.core.shutdown(), "shutdown stops the server");
    // Read only after shutdown, which joins every connection thread:
    // each passed park, drain included, has ended on readiness by now.
    assert_eq!(
        server.core.test_readied_waits() - readied_at_start,
        total_passed,
        "every data park passed after its bytes ends on readiness, not its bound"
    );
    assert!(
        held.try_recv().is_err(),
        "no held note is left after shutdown"
    );
}

// Five bench-shaped POSTs: byte-slice bodies of growing length, each on
// a fresh connection read to the server's close, against small stream
// replies, like five of a busy turn's model calls. Bulk body bytes
// bypass the lookahead past the head's leftover, so only each head and
// one fill's over-read transit it: funnelling every body byte through
// the per-byte lookahead, as after #2220, costs the bodies' whole
// ~300 KiB here, far past the bound below.
#[test]
fn growing_fixed_bodies_bypass_the_lookahead() {
    const CALLS: usize = 5;
    const STEP: usize = 20_000;
    // Each call's head plus one 8 KiB fill's over-read past it.
    const LOOKAHEAD_BOUND: u64 = 5 * 16_384;
    let clock = FakeClock::new();
    let script: Vec<Response> = (0..CALLS)
        .map(|_| Response::stream("event: done\ndata: {}\n\n"))
        .collect();
    let bodies: Vec<Vec<u8>> = (0..CALLS)
        .map(|n| (0u8..=250).cycle().take((n + 1) * STEP).collect())
        .collect();
    let server = ProviderServer::start_on(clock, script).unwrap();
    let addr = server.addr;
    for body in bodies.iter() {
        let head = format!(
            "POST /v1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        // The write side stays open until the reply completes, like the
        // bench's client on its fresh connection per call: the server
        // closes, so the read ends at the reply instead of its bound.
        // One wall-clock deadline spans the connect, the writes and the
        // whole reply, so continued partial progress fails here instead
        // of at the harness kill.
        let response = exchange_within(addr, head, body.clone(), "the scripted reply arrives");
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(response.ends_with(b"{}\n\n"), "{text}");
    }
    assert!(
        server.await_requests(CALLS, Deadline::after(READ_WITHIN)),
        "every request is recorded"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), CALLS);
    for (request, expected) in requests.iter().zip(bodies.iter()) {
        assert_eq!(request.body_len, expected.len());
        assert_eq!(
            first_diff(&request.body, expected),
            None,
            "a recorded body is byte-exact"
        );
    }
    let lookahead = server.core.test_lookahead_bytes();
    assert!(
        lookahead <= LOOKAHEAD_BOUND,
        "five growing bodies cost {lookahead} lookahead bytes, at most {LOOKAHEAD_BOUND}: bulk body bytes bypass the lookahead past the head's leftover"
    );
}

// A fixed body buffered with its head keeps only its declared length:
// the socket-read cap is recomputed after the buffered drain, so the
// tail and the pipelined suffix arriving together never spill into the
// body. The head and the first bytes go as one burst, so the whole head
// and every first byte is buffered before the body read parks (pinned by
// its wait announcement); the tail and the pipelined suffix arrive as a
// second burst. A stale cap reads past the tail into the suffix, the
// body exceeds its length, and the next subtraction panics instead of
// answering.
#[test]
fn a_fixed_body_buffered_with_its_head_keeps_only_its_length() {
    const BODY_LEN: usize = 10_000;
    // With the head, one burst under one 8 KiB fill: every first byte is
    // buffered, and the body read parks with the socket empty.
    const FIRST: usize = 8_100;
    let clock = FakeClock::new();
    let server =
        ProviderServer::start_on(clock.clone(), vec![Response::status(200, "ok")]).unwrap();
    let acks = server.core.test_acknowledgments();
    let addr = server.addr;

    let body: Vec<u8> = (0u8..=250).cycle().take(BODY_LEN).collect();
    let mut pipelined = b"GET /next HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec();
    pipelined.resize(2_000, 0xabu8);
    let mut stream = TcpStream::connect_timeout(&addr, READ_WITHIN)
        .unwrap_or_else(|e| panic!("the connection arrives within {READ_WITHIN:?}: {e}"));
    stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
    stream.set_write_timeout(Some(READ_WITHIN)).unwrap();
    let mut first =
        format!("POST /v1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: {BODY_LEN}\r\n\r\n")
            .into_bytes();
    first.extend_from_slice(&body[..FIRST]);
    stream.write_all(&first).unwrap_or_else(|e| {
        panic!("the head and the first bytes arrive within {READ_WITHIN:?}: {e}")
    });
    // The server is out of bytes and parked in the body read: only then
    // do the tail and the pipelined suffix arrive, together in one burst.
    // Earlier phases announce first and are passed over; every receive is
    // bounded, so a missing body wait fails, never hangs.
    let deadline = Deadline::after(READ_WITHIN);
    loop {
        let ack = deadline.recv_or_fail(&acks, "the body read's wait");
        if ack.phase == "to read the fixed body" {
            break;
        }
    }
    let mut rest = body[FIRST..].to_vec();
    rest.extend_from_slice(&pipelined);
    stream.write_all(&rest).unwrap_or_else(|e| {
        panic!("the tail and the pipelined suffix arrive within {READ_WITHIN:?}: {e}")
    });
    // Wakes the parked read deterministically: the test advances the
    // fake clock only past the wait the announcement pinned.
    clock.advance(Duration::from_secs(1));
    // Anything past the body sees the close: an over-read loses the
    // reply instead of hanging on it. One wall-clock deadline spans the
    // whole reply, so continued partial progress fails here instead of
    // at the harness kill.
    stream.shutdown(Shutdown::Write).unwrap();
    let response = read_reply_within(stream, "the scripted reply arrives");
    let text = String::from_utf8_lossy(&response);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(response.ends_with(b"ok"), "{text}");

    assert!(
        server.await_requests(1, Deadline::after(READ_WITHIN)),
        "the request is recorded"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body_len, BODY_LEN);
    assert_eq!(
        requests[0].body.len(),
        body.len(),
        "the recorded body has the sent length"
    );
    assert_eq!(
        first_diff(&requests[0].body, &body),
        None,
        "the recorded body holds its declared bytes, not the pipelined ones"
    );
}

// A CONNECT with a declared Content-Length whose payload is larger than
// the head's initial read tunnels every byte to the origin: a CONNECT has
// no message body, so declared bytes are tunnel data, never a body, and
// no discard runs on the tunnel path. The head carries the content-length
// line without its blank line, so the handshake parks (pinned by its wait
// announcement) before the blank line and the whole payload arrive as one
// burst: the burst is queued before the parked read wakes, so a body read
// or a discard, if either ran, would certainly eat socket bytes.
#[test]
fn a_connect_payload_larger_than_the_head_read_reaches_its_origin() {
    use std::io::BufRead as _;

    const PAYLOAD_LEN: usize = 64_000;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(READ_WITHIN)).unwrap();
        // One wall-clock deadline spans the whole payload, so continued
        // partial progress fails here instead of at the harness kill.
        let payload = read_reply_within(stream, "the origin reads the payload");
        tx.send(payload).unwrap();
    });
    let proxy = crate::ConnectProxy::start().unwrap();
    let acks = proxy.test_acknowledgments();
    let target = format!("{origin}");
    let addr: SocketAddr = format!("127.0.0.1:{}", proxy.port()).parse().unwrap();
    let mut client = TcpStream::connect_timeout(&addr, READ_WITHIN)
        .unwrap_or_else(|e| panic!("the connection arrives within {READ_WITHIN:?}: {e}"));
    client.set_read_timeout(Some(READ_WITHIN)).unwrap();
    client.set_write_timeout(Some(READ_WITHIN)).unwrap();
    client
        .write_all(
            format!(
                "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nContent-Length: {PAYLOAD_LEN}\r\n"
            )
            .as_bytes(),
        )
        .unwrap_or_else(|e| panic!("the partial head arrives within {READ_WITHIN:?}: {e}"));
    let deadline = Deadline::after(READ_WITHIN);
    loop {
        let ack = deadline.recv_or_fail(&acks, "the CONNECT head read's wait");
        if ack.phase == "to read the request line" || ack.phase == "to read the headers" {
            break;
        }
    }
    let payload: Vec<u8> = (0u8..=250).cycle().take(PAYLOAD_LEN).collect();
    let mut burst = b"\r\n".to_vec();
    burst.extend_from_slice(&payload);
    client.write_all(&burst).unwrap_or_else(|e| {
        panic!("the blank line and the payload arrive within {READ_WITHIN:?}: {e}")
    });
    assert!(
        proxy.await_connects(1, Deadline::after(READ_WITHIN)),
        "the proxy recorded CONNECT {target}"
    );
    let mut reader = std::io::BufReader::new(client.try_clone().unwrap());
    let mut status = String::new();
    reader
        .read_line(&mut status)
        .unwrap_or_else(|e| panic!("the proxy's CONNECT reply within {READ_WITHIN:?}: {e}"));
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "the CONNECT reply: {status:?}"
    );
    client.shutdown(Shutdown::Write).unwrap();
    let heard = Deadline::after(READ_WITHIN)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("the origin receives the payload within {READ_WITHIN:?}"));
    assert_eq!(
        heard.len(),
        payload.len(),
        "the origin holds the whole payload"
    );
    assert_eq!(
        first_diff(&heard, &payload),
        None,
        "the tunnelled payload is byte-exact"
    );
}
