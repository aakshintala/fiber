//! The raw-reply fake through its public API, driven by a plain
//! TCP client over a `TcpStream`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{Deadline, RawHttpServer, Request, fingerprint};

/// The deadline on each wait: connecting, writing the whole request, and
/// reading the whole reply. A stall fails naming the wait (`docs/testing.md`,
/// "Waits and timeouts").
const DEADLINE: Duration = fakes::MUST_SUCCEED_WITHIN;

/// The refusal the refusing-proxy migration serves: exact bytes, then EOF.
const REFUSED: &[u8] = b"HTTP/1.1 403 Forbidden\r\nconnection: close\r\ncontent-length: 0\r\n\r\n";
/// The close-delimited body the lengthless migration serves: no measured
/// length, then EOF.
const LENGTHLESS: &[u8] = b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\nabc";
/// The cut-short body the truncated migration serves: a declared length
/// the delivered bytes do not carry, then EOF.
const TRUNCATED: &[u8] =
    b"HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: 100\r\n\r\n<p>abc";

fn addr(server: &RawHttpServer) -> SocketAddr {
    server.url().trim_start_matches("http://").parse().unwrap()
}

/// One step's report from the client thread: each step of one exchange
/// takes what remains of the exchange's one deadline (`docs/testing.md`,
/// "Waits and timeouts").
#[track_caller]
fn wait(
    rx: &mpsc::Receiver<Result<Vec<u8>, io::Error>>,
    step: &str,
    deadline: &Deadline,
) -> Vec<u8> {
    match deadline.recv(rx) {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => panic!("{step}: {e}"),
        Err(_) => panic!("{step}: not done within {DEADLINE:?}"),
    }
}

/// Sends `request` as written and reads the reply to the end of the
/// connection. A client thread does the I/O and reports each step, so
/// each wait has one overall deadline and a stall names its step. Every
/// socket takes a timeout and the thread is joined through the
/// exchange's completion channel, so a failure path never leaves a
/// blocked client behind.
#[track_caller]
fn exchange(server: &RawHttpServer, request: &[u8], close_write: bool) -> Vec<u8> {
    let (addr, request) = (addr(server), request.to_vec());
    let (tx, rx) = mpsc::channel();
    let client = thread::spawn(move || {
        let steps = || -> io::Result<()> {
            let mut stream = TcpStream::connect_timeout(&addr, DEADLINE)?;
            stream.set_read_timeout(Some(DEADLINE))?;
            stream.set_write_timeout(Some(DEADLINE))?;
            tx.send(Ok(Vec::new())).unwrap();
            stream.write_all(&request)?;
            if close_write {
                stream.shutdown(Shutdown::Write)?;
            }
            tx.send(Ok(Vec::new())).unwrap();
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw)?;
            tx.send(Ok(raw)).unwrap();
            Ok(())
        };
        if let Err(e) = steps() {
            tx.send(Err(e)).unwrap();
        }
    });
    let step_wait = Deadline::after(DEADLINE);
    wait(&rx, "connecting to the raw fake", &step_wait);
    wait(&rx, "writing the request to the raw fake", &step_wait);
    let raw = wait(&rx, "reading the raw fake's whole reply", &step_wait);
    match client.join() {
        Ok(()) | Err(_) => {}
    }
    raw
}

/// Spins until at least `count` requests are recorded, at most
/// `DEADLINE`: the record lands on the serving thread just before the
/// reply, so a test that must observe the record before closing the peer
/// waits for it here, failing naming the wait instead of hanging.
#[track_caller]
fn wait_recorded(server: &RawHttpServer, count: usize, what: &str) -> Vec<Request> {
    let bound = Deadline::after(DEADLINE);
    loop {
        let requests = server.requests();
        if requests.len() >= count {
            return requests;
        }
        if bound.left().is_zero() {
            panic!("waited until the deadline for {what}");
        }
        thread::yield_now();
    }
}

#[test]
fn the_url_carries_the_bound_port() {
    let server = RawHttpServer::start(LENGTHLESS).unwrap();
    assert_eq!(
        addr(&server).port(),
        server.port(),
        "the URL carries the bound port"
    );
}

#[test]
fn raw_replies_preserve_the_three_migrated_wire_fixtures() {
    for (name, fixture) in [
        ("refusal", REFUSED),
        ("lengthless", LENGTHLESS),
        ("truncated", TRUNCATED),
    ] {
        let server = RawHttpServer::start(fixture).unwrap();
        let raw = exchange(
            &server,
            b"GET / HTTP/1.1\r\nHost: fake\r\nConnection: close\r\n\r\n",
            true,
        );
        assert_eq!(
            raw, fixture,
            "{name}: the reply is the fixture bytes through EOF"
        );
        assert!(
            server.await_replies(1, Deadline::after(DEADLINE)),
            "{name}: the reply completed"
        );
    }
}

#[test]
fn a_raw_reply_is_repeated_for_later_requests_and_records_each() {
    let server = RawHttpServer::start(LENGTHLESS).unwrap();
    let first = exchange(&server, b"GET /first HTTP/1.1\r\nHost: fake\r\n\r\n", true);
    let second = exchange(
        &server,
        b"POST /second HTTP/1.1\r\nHost: fake\r\nContent-Length: 3\r\n\r\nabc",
        true,
    );
    assert_eq!(first, LENGTHLESS, "the first reply is the fixed bytes");
    assert_eq!(
        second, LENGTHLESS,
        "the second reply repeats the fixed bytes"
    );
    let requests = wait_recorded(&server, 2, "both requests to be recorded");
    assert_eq!(requests.len(), 2, "both requests are kept");
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/first");
    assert!(requests[0].body.is_empty(), "a bodyless request keeps none");
    assert_eq!(requests[0].body_len, 0);
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].path, "/second");
    assert_eq!(requests[1].body, b"abc");
    assert_eq!(requests[1].body_len, 3);
}

#[test]
fn raw_request_credentials_are_fingerprinted() {
    let server = RawHttpServer::start(LENGTHLESS).unwrap();
    exchange(
        &server,
        b"GET /x?key=hunter2&other=1 HTTP/1.1\r\nHost: fake\r\nAuthorization: Bearer secret-1\r\nX-Custom: plain\r\n\r\n",
        true,
    );
    let requests = wait_recorded(&server, 1, "the credentialed request to be recorded");
    let secret = fingerprint("Bearer secret-1");
    assert_eq!(
        requests[0].header("authorization"),
        Some(secret.as_str()),
        "a credential header records only its fingerprint"
    );
    let key = fingerprint("hunter2");
    assert_eq!(
        requests[0].path,
        format!("/x?key={key}&other=1"),
        "a key query records only its fingerprint"
    );
    assert_eq!(
        requests[0].header("x-custom"),
        Some("plain"),
        "any other header records as sent"
    );
}

#[test]
fn raw_reply_completion_is_observed_after_the_send() {
    let server = RawHttpServer::start(LENGTHLESS).unwrap();
    assert!(
        server.await_replies(0, Deadline::after(Duration::ZERO)),
        "zero replies admit at once"
    );
    assert!(
        !server.await_replies(1, Deadline::after(Duration::ZERO)),
        "no reply completed before any request"
    );
    exchange(&server, b"GET /one HTTP/1.1\r\nHost: fake\r\n\r\n", true);
    assert!(
        server.await_replies(1, Deadline::after(DEADLINE)),
        "one reply completed after one exchange"
    );
    assert!(
        !server.await_replies(2, Deadline::after(Duration::ZERO)),
        "one reply does not satisfy two"
    );
    exchange(&server, b"GET /two HTTP/1.1\r\nHost: fake\r\n\r\n", true);
    assert!(
        server.await_replies(2, Deadline::after(DEADLINE)),
        "two replies completed after two exchanges"
    );
}

#[test]
fn an_interrupted_reply_send_publishes_no_completion() {
    // More bytes than any socket buffer can hold past a dead peer, so
    // the send cannot fully flush and the reply never counts.
    let mut huge = b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n".to_vec();
    huge.extend(std::iter::repeat_n(b'x', 8 * 1024 * 1024));
    let server = RawHttpServer::start(huge).unwrap();
    let address = addr(&server);
    let mut stream = TcpStream::connect_timeout(&address, DEADLINE).unwrap();
    stream.set_write_timeout(Some(DEADLINE)).unwrap();
    stream
        .write_all(b"GET /cut HTTP/1.1\r\nHost: fake\r\n\r\n")
        .unwrap();
    let recorded = wait_recorded(&server, 1, "the interrupted request to be recorded");
    assert_eq!(
        recorded[0].method, "GET",
        "the interrupted request is recorded before its reply"
    );
    // Close the peer: the record above proves the read finished, so any
    // close after it meets a post-read server, and the 8 MiB reply cannot
    // flush into a non-reading peer, so the send ends stopped, whether
    // the close lands as a reset or a graceful end.
    drop(stream);
    assert!(
        server.await_idle(Deadline::after(DEADLINE)),
        "the send attempt ended"
    );
    assert!(
        !server.await_replies(1, Deadline::after(Duration::ZERO)),
        "a reply cut short publishes no completion"
    );
}

#[test]
fn a_truncated_chunked_request_is_recorded_with_its_decoded_prefix_and_gets_the_fixed_reply() {
    let server = RawHttpServer::start(LENGTHLESS).unwrap();
    let raw = exchange(
        &server,
        b"POST /cut HTTP/1.1\r\nHost: fake\r\nTransfer-Encoding: chunked\r\n\r\n3\r\na",
        true,
    );
    assert_eq!(
        raw, LENGTHLESS,
        "a truncated chunked request still gets the fixed reply"
    );
    let requests = wait_recorded(&server, 1, "the truncated request to be recorded");
    assert_eq!(
        requests[0].body, b"a",
        "the record keeps what decoded before the close"
    );
    assert_eq!(requests[0].body_len, 1);
}

#[test]
fn dropping_a_server_with_a_blocked_reply_returns_promptly() {
    // More bytes than any socket buffer can hold past a peer that never
    // reads, so the send blocks and only the shutdown ends it; dropping
    // the server still returns promptly through the core's bounded join.
    let mut huge = b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n".to_vec();
    huge.extend(std::iter::repeat_n(b'x', 8 * 1024 * 1024));
    let server = RawHttpServer::start(huge).unwrap();
    let address = addr(&server);
    let (held_tx, held_rx) = mpsc::channel::<TcpStream>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect_timeout(&address, DEADLINE).unwrap();
        stream.set_write_timeout(Some(DEADLINE)).unwrap();
        // A handle the test owns: it drops this before joining this
        // worker, so no path leaves a blocked client behind.
        held_tx.send(stream.try_clone().unwrap()).unwrap();
        stream
            .write_all(b"GET /blocked-drop HTTP/1.1\r\nHost: fake\r\n\r\n")
            .unwrap();
        // Never reads: the reply outgrows every socket buffer, so the
        // server's send blocks until the shutdown ends it.
        let _ignored = release_rx.recv();
    });
    let peer_wait = Deadline::after(DEADLINE);
    let peer = peer_wait.recv_or_fail(&held_rx, "connecting the blocked-drop client");
    let recorded = wait_recorded(&server, 1, "the blocked request to be recorded");
    assert_eq!(
        recorded[0].method, "GET",
        "the blocked request is recorded before the drop"
    );
    assert_eq!(recorded[0].path, "/blocked-drop");
    let probe = server.drop_probe();
    let (drop_tx, drop_rx) = mpsc::channel();
    let _dropper = thread::spawn(move || {
        drop(server);
        let _sent = drop_tx.send(());
    });
    let drop_wait = Deadline::after(DEADLINE);
    drop_wait.recv_or_fail(&drop_rx, "dropping the server with a blocked reply");
    // Dropping this handle closes nothing while the worker still holds
    // its own; releasing and joining the worker then leaves no path
    // with a client blocked behind.
    drop(peer);
    let _sent = release_tx.send(());
    match client.join() {
        Ok(()) | Err(_) => {}
    }
    assert!(
        probe.listener_closed(),
        "the dropped server closed its listener"
    );
    assert!(
        probe.accept_joined(),
        "the dropped server joined its accept thread"
    );
}

#[test]
fn dropping_the_raw_server_closes_its_listener_and_incomplete_connection() {
    let server = RawHttpServer::start(LENGTHLESS).unwrap();
    let address = addr(&server);
    let mut stream = TcpStream::connect_timeout(&address, DEADLINE).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: fake\r\n")
        .unwrap();
    let probe = server.drop_probe();
    drop(server);
    let mut byte = [0u8; 1];
    match stream.read(&mut byte) {
        Ok(0) => {}
        Ok(read) => panic!("the dropped server answered nothing, yet {read} byte arrived"),
        Err(_) => {}
    }
    assert!(
        probe.listener_closed(),
        "the dropped server closed its listener"
    );
    assert!(
        probe.accept_joined(),
        "the dropped server joined its accept thread"
    );
}
