//! Tests of the shared HTTP/1.1 core's ownership, budgets and waits,
//! through its own seam (`http_server.rs`).

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc, PoisonError,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;

use super::request::ParsedRequest;
use super::*;
use crate::clock::FakeClock;
use crate::deadline::Deadline;

/// How long one test operation may take on the wall clock: far above a
/// loopback exchange under load, far below any real-guard bound.
const OP: Duration = Duration::from_secs(10);

/// How long a check that must observe nothing waits: the core polls at
/// most every millisecond, so a still-waiting operation stays silent well
/// past this.
const QUIET: Duration = Duration::from_millis(200);

/// Waits until at least `count` persistent windows expired idle.
fn await_idle(server: &HttpServer, count: usize, within: Duration) -> bool {
    let guard = lock(&server.shared.idle);
    let (guard, _) = server
        .shared
        .idle_changed
        .wait_timeout_while(guard, within, |idle| *idle < count)
        .unwrap_or_else(PoisonError::into_inner);
    *guard >= count
}

/// Waits until at least `count` persistent windows started.
fn await_windows(server: &HttpServer, count: usize, within: Duration) -> bool {
    let guard = lock(&server.shared.windows);
    let (guard, _) = server
        .shared
        .windows_changed
        .wait_timeout_while(guard, within, |windows| *windows < count)
        .unwrap_or_else(PoisonError::into_inner);
    *guard >= count
}

/// Whether shutdown stopped the server.
fn is_stopped(server: &HttpServer) -> bool {
    use std::sync::atomic::Ordering;
    server.shared.stopping.load(Ordering::SeqCst)
}

/// Releases an armed check-to-park pause and stops the server when the
/// test unwinds past it: a waiter held between its check and its park
/// would otherwise outlive the failed test and hang the scope's join,
/// turning a red assertion into a hung suite. After a green run the
/// release and shutdown repeat what the test already did.
struct Rescue {
    gate: Arc<CheckGate>,
    server: HttpServer,
}

impl Drop for Rescue {
    fn drop(&mut self) {
        self.gate.release();
        self.server.shutdown();
    }
}

/// A request bound of ten fake seconds, so advances stay small numbers.
fn ten_seconds() -> Limits {
    Limits {
        request: Duration::from_secs(10),
        ..Limits::default()
    }
}

/// A clock the test owns, as the core's injected clock.
fn fake_clock() -> Arc<FakeClock> {
    FakeClock::new()
}

/// Binds the core on `clock` with ten-second request bounds.
fn bind(clock: &Arc<FakeClock>) -> HttpServer {
    HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds()).expect("the core binds")
}

/// Connects to the core: the test's client end.
fn connect(server: &HttpServer) -> TcpStream {
    TcpStream::connect(server.addr()).expect("the test connects")
}

/// The waiter's clock-wait acknowledgment, failing the test when none
/// arrives within the operation bound.
fn next_acknowledgment(deadline: &Deadline, rx: &mpsc::Receiver<WaitAck>, what: &str) -> WaitAck {
    deadline.recv_or_fail(rx, what)
}

/// The text of a waiter that failed naming its request: the test fails
/// when the waiter read instead.
fn failure_text<T>(caught: thread::Result<T>, what: &str) -> String {
    match caught {
        Ok(_) => panic!("{what} read instead of failing"),
        Err(payload) => payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|line| (*line).to_owned())
            })
            .expect("the failure names its request"),
    }
}

/// An expected accept with no client expires at its bound, naming the
/// expected request: one tick before it is still waiting, at it and one
/// tick past it the wait names the request.
#[test]
fn expected_accept_expires_without_a_client() {
    // One tick before the bound the accept is still waiting.
    let clock = fake_clock();
    let server =
        HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let acks = server.test_acknowledgments();
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                server.accept_expected("the missing request")
            }));
            done_tx.send(caught).expect("the test waits");
        });
        let deadline = Deadline::after(OP);
        let ack = next_acknowledgment(&deadline, &acks, "the accept's clock wait");
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the accept parks on the clock before it moves"
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(10) - Duration::from_nanos(1));
        assert!(
            Deadline::after(QUIET).recv(&done_rx).is_err(),
            "one tick before the bound the accept is still waiting"
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_nanos(1));
        let caught = Deadline::after(OP)
            .recv(&done_rx)
            .expect("the bound ends the accept");
        let message = failure_text(caught, "the missing request");
        assert!(
            message.contains("the missing request"),
            "the failure names its request, got: {message}"
        );
    });

    // One tick past the bound expires too: a single advance past it.
    let clock = fake_clock();
    let server =
        HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let acks = server.test_acknowledgments();
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                server.accept_expected("the late request")
            }));
            done_tx.send(caught).expect("the test waits");
        });
        let deadline = Deadline::after(OP);
        let ack = next_acknowledgment(&deadline, &acks, "the accept's clock wait");
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the accept parks on the clock before it moves"
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(10) + Duration::from_nanos(1));
        let caught = Deadline::after(OP)
            .recv(&done_rx)
            .expect("past the bound the accept fails");
        let message = failure_text(caught, "the late request");
        assert!(
            message.contains("the late request"),
            "the failure names its request, got: {message}"
        );
    });
}

/// Idle persistent windows renew without expiring any request, and the
/// next window serves the request that arrives: two renewals, then bytes
/// go both ways.
#[test]
fn persistent_accept_renews_idle_windows_and_serves_the_next_request() {
    let clock = fake_clock();
    let server = HttpServer::bind(
        clock.clone() as Arc<dyn Clock>,
        Limits {
            request: Duration::from_secs(5),
            ..Limits::default()
        },
    )
    .expect("the core binds");
    server.serve_persistent(|mut conn| {
        if conn.read_byte("to read the test request") == ReadOutcome::Byte(b'q') {
            conn.write_all(b"a", "to answer the test request");
        }
    });
    // Two idle windows expire with no request to fail: each advance
    // lands past its window's start, so no window misses its budget.
    assert!(await_windows(&server, 1, OP), "the first window started");
    #[allow(
        clippy::disallowed_methods,
        reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
    )]
    clock.advance(Duration::from_secs(5));
    assert!(await_idle(&server, 1, OP), "the first idle window renewed");
    assert!(await_windows(&server, 2, OP), "the second window started");
    #[allow(
        clippy::disallowed_methods,
        reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
    )]
    clock.advance(Duration::from_secs(5));
    assert!(await_idle(&server, 2, OP), "the second idle window renewed");

    let mut client = connect(&server);
    client.write_all(b"q").expect("the question writes");
    let mut answer = [0u8; 1];
    use std::io::Read;
    client
        .set_read_timeout(Some(OP))
        .expect("the test bounds its read");
    client.read_exact(&mut answer).expect("the answer reads");
    assert_eq!(answer, [b'a']);
    assert!(server.shutdown(), "shutdown reaps the serving loop");
}

/// Time in the accept window counts against the request's bound: three
/// fake seconds of waiting leave seven, so a read at one tick before the
/// bound succeeds and the next read names the request instead of renewing
/// the bound.
#[test]
fn accept_time_is_charged_to_the_request_budget() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let (conn_tx, conn_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let conn = server
                .accept_expected("the charged request")
                .expect("a client connects");
            conn_tx.send(conn).expect("the test waits");
        });
        let deadline = Deadline::after(OP);
        let ack = next_acknowledgment(&deadline, &acks, "the accept's clock wait");
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the accept parks on the clock before it moves"
        );
        // Three seconds pass with no client: the budget is running already.
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(3));
        let mut client = connect(&server);
        client.write_all(b"ab").expect("the bytes write");
        let mut conn = Deadline::after(OP)
            .recv(&conn_rx)
            .expect("the accept takes the client");
        assert_eq!(
            conn.read_byte("to read the first byte"),
            ReadOutcome::Byte(b'a'),
            "three seconds in, the first byte reads"
        );
        // One tick before the ten-second bound the second byte still reads:
        // progress never renewed the bound.
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(7) - Duration::from_nanos(1));
        assert_eq!(
            conn.read_byte("to read the second byte"),
            ReadOutcome::Byte(b'b'),
            "one tick before the bound the second byte reads"
        );
        // Past the bound the next read names the request.
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_nanos(1));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            conn.read_byte("to read past the bound");
        }));
        let message = failure_text(caught, "the read past the bound");
        assert!(
            message.contains("the charged request"),
            "the failure names its request, got: {message}"
        );
    });
}

/// A silent connection wakes on fake time alone: the client sends no byte
/// after connecting, yet the bound names the request at its expiry.
#[test]
fn fake_time_wakes_a_silent_connection() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the silent request")
                .expect("a client connects");
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                conn.read_byte("to read the silent request");
            }));
            done_tx.send(caught).expect("the test waits");
        });
        // The client connects and sends nothing: no byte wakes the read.
        // The accept itself may complete without waiting when the client
        // beats it, so only the read's acknowledgment is required.
        let _client = connect(&server);
        let deadline = Deadline::after(OP);
        let read = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the read's clock wait");
            if ack.phase == "to read the silent request" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(read.thread, read.until, Deadline::after(OP)),
            "the read parks on the clock before it moves"
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(10));
        let caught = Deadline::after(OP)
            .recv(&done_rx)
            .expect("fake time ends the silent read");
        let message = failure_text(caught, "the silent read");
        assert!(
            message.contains("the silent request"),
            "the failure names its request, got: {message}"
        );
        assert!(
            message.contains("to read the silent request"),
            "the failure names its phase, got: {message}"
        );
    });
}

/// Shutdown ends an idle accept window at once: the serving loop stops
/// instead of waiting out its bound.
#[test]
fn shutdown_wakes_idle_accept() {
    let clock = fake_clock();
    let server = bind(&clock);
    server.serve_persistent(|mut conn| {
        while conn.read_byte("to drain the test request") == ReadOutcome::Byte(b'x') {}
    });
    assert!(
        !is_stopped(&server),
        "the serving loop runs before shutdown"
    );
    assert!(server.shutdown(), "shutdown ends the idle accept");
    assert!(is_stopped(&server), "the serving loop stopped");
}

/// Shutdown reaps connections blocked in core waits: a read waiting on
/// the clock ends stopped, and shutdown joins it.
#[test]
fn shutdown_reaps_core_blocked_connections() {
    let clock = fake_clock();
    let server = bind(&clock);
    // The handler reports its first byte: the held read below is proven
    // waiting by the report, not by an accept acknowledgment the accept
    // may never send when the client is already pending.
    let (held_tx, held_rx) = mpsc::channel();
    server.serve_persistent(move |mut conn| {
        // Blocked in a core wait after this byte: shutdown must end it.
        if conn.read_byte("to read the held request") == ReadOutcome::Byte(b'x') {
            match held_tx.send(()) {
                Ok(()) | Err(_) => {}
            }
            while conn.read_byte("to read the held request") == ReadOutcome::Byte(b'x') {}
        }
    });
    let mut client = connect(&server);
    client.write_all(b"x").expect("the byte writes");
    Deadline::after(OP).recv_or_fail(&held_rx, "the held read's first byte");
    assert!(server.shutdown(), "shutdown reaps the blocked read");
}

/// Each move of the injected clock bumps the wake generation by one:
/// no waiter is parked, so a broken bump fails at once instead of
/// hanging a waiter past the mutant timeout.
#[test]
fn each_clock_move_bumps_the_wake_generation() {
    let clock = fake_clock();
    let server = bind(&clock);
    assert_eq!(server.test_generation(), 0, "the generation starts at zero");
    #[allow(
        clippy::disallowed_methods,
        reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
    )]
    clock.advance(Duration::from_secs(1));
    assert_eq!(
        server.test_generation(),
        1,
        "one clock move bumps the generation once"
    );
    #[allow(
        clippy::disallowed_methods,
        reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
    )]
    clock.advance(Duration::from_secs(1));
    assert_eq!(server.test_generation(), 2, "the next move bumps it again");
}

/// An advance between the wait's clock check and its park is not lost:
/// the waiter fails naming its request instead of sleeping past the
/// advance. The check-to-park pause holds the waiter after its check, the
/// advance lands while it is held, and the waiter's next park sees it at
/// once.
#[test]
fn advance_between_check_and_park_is_not_lost() {
    let clock = fake_clock();
    let server = HttpServer::bind(
        clock.clone() as Arc<dyn Clock>,
        Limits {
            request: Duration::from_secs(10),
            // A long poll: only the clock's wake may end the park, so a
            // lost wake sleeps past the test's bound instead of passing on
            // a millisecond poll.
            poll: Duration::from_secs(60),
            ..Limits::default()
        },
    )
    .expect("the core binds");
    let acks = server.test_acknowledgments();
    let gate = server.test_gate();
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                server.accept_expected("the raced request")
            }));
            done_tx.send(caught).expect("the test waits");
        });
        let deadline = Deadline::after(OP);
        let first = next_acknowledgment(&deadline, &acks, "the accept's clock wait");
        assert!(
            clock.await_thread_parked(first.thread, first.until, Deadline::after(OP)),
            "the accept parks on the clock before it moves"
        );
        // Arm the check-to-park pause, then wake the waiter once: it
        // re-checks past the advance and pauses before its next park.
        gate.arm();
        // Dropped when an assertion below fails: the held waiter would
        // otherwise hang the scope's join, so a red assertion fails the
        // test instead of hanging it.
        let _rescue = Rescue {
            gate: Arc::clone(&gate),
            server: server.clone(),
        };
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(3));
        let deadline = Deadline::after(OP);
        let second = next_acknowledgment(&deadline, &acks, "the accept's re-check");
        assert_eq!(
            second.seq,
            first.seq + 1,
            "only the announced sequence advances the clock"
        );
        // The deciding advance lands while the waiter is held between its
        // check and its park.
        let (advanced_tx, advanced_rx) = mpsc::channel();
        let advancing = clock.clone();
        scope.spawn(move || {
            #[allow(
                clippy::disallowed_methods,
                reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
            )]
            advancing.advance(Duration::from_secs(7));
            advanced_tx.send(()).expect("the test waits");
        });
        // The advance landed while the waiter was held: the waiter never ran
        // between the advance and the release.
        gate.release();
        Deadline::after(OP).recv_or_fail(&advanced_rx, "the deciding advance");
        let caught = Deadline::after(OP).recv_or_fail(&done_rx, "the raced accept");
        let message = failure_text(caught, "the raced accept");
        assert!(
            message.contains("the raced request"),
            "the failure names its request, got: {message}"
        );
    });
}

/// A client over blocking I/O: connects, sends one request, reads the
/// whole reply to the connection's end.
fn exchange(addr: SocketAddr, request: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut stream = TcpStream::connect(addr).expect("the test connects");
    stream
        .set_read_timeout(Some(OP))
        .expect("the test bounds its reads");
    stream.write_all(request).expect("the request writes");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("the reply reads");
    raw
}

/// Serves one connection: reads its request, records it, and answers
/// with `answer`. The recording precedes any admission: the test owns
/// both sides of that order.
fn serve_once(
    recorded: mpsc::Sender<ParsedRequest>,
    answer: impl FnOnce(Connection) + Send + 'static,
) -> (HttpServer, SocketAddr) {
    serve_once_on(FakeClock::new(), recorded, answer)
}

/// `serve_once`, on the test's clock so it can drive a drain deadline.
fn serve_once_on(
    clock: Arc<FakeClock>,
    recorded: mpsc::Sender<ParsedRequest>,
    answer: impl FnOnce(Connection) + Send + 'static,
) -> (HttpServer, SocketAddr) {
    let server = HttpServer::bind(clock as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let addr = server.addr();
    let serving = server.clone();
    thread::spawn(move || {
        let mut conn = serving
            .accept_expected("the test request")
            .expect("a client connects");
        let request = match request::read_request(&mut conn) {
            request::RequestOutcome::Complete(request) => request,
            request::RequestOutcome::Eof => panic!("the peer closed before completing the request"),
            request::RequestOutcome::Stopped => panic!("shutdown ended the request read"),
            request::RequestOutcome::Invalid(why) => panic!("the request was invalid: {why}"),
        };
        match recorded.send(request) {
            Ok(()) | Err(_) => {}
        }
        answer(conn);
    });
    (server, addr)
}

#[test]
fn full_reply_preserves_scripted_headers_and_bytes() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let head = b"HTTP/1.1 200 Fake\r\ncontent-type: text/event-stream\r\ncontent-length: 2\r\nconnection: close\r\n\r\n";
    let (server, addr) = serve_once(recorded_tx, move |mut conn| {
        assert_eq!(
            conn.reply_full(head, b"ok", None),
            ReplyEnd::Done,
            "the full reply sends"
        );
    });
    let raw = exchange(addr, b"GET /v1 HTTP/1.1\r\nHost: fake\r\n\r\n");
    let mut expected = head.to_vec();
    expected.extend_from_slice(b"ok");
    assert_eq!(raw, expected, "the wire carries the scripted bytes");
    let request = Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
    assert_eq!(request.target, "/v1");
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

/// Starts a reply drain while the peer keeps its write side open. The
/// acknowledgment proves the drain is waiting on its injected deadline.
fn start_reply_drain(
    within: Duration,
) -> (
    Arc<FakeClock>,
    HttpServer,
    TcpStream,
    mpsc::Receiver<ParsedRequest>,
    mpsc::Receiver<()>,
    WaitAck,
) {
    use std::io::{Read, Write};

    let clock = fake_clock();
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let (drained_tx, drained_rx) = mpsc::channel();
    let (server, addr) = serve_once_on(clock.clone(), recorded_tx, move |mut conn| {
        assert!(conn.write_all(b"reply", "to write the test reply"));
        conn.shutdown_write_and_drain(within);
        drained_tx.send(()).expect("the test waits for the drain");
    });
    let acks = server.test_acknowledgments();
    let mut client = TcpStream::connect(addr).expect("the test connects");
    client
        .set_read_timeout(Some(OP))
        .expect("the test bounds reads");
    client
        .write_all(b"GET /v1 HTTP/1.1\r\nHost: fake\r\n\r\n")
        .expect("the request writes");
    let mut reply = [0; 5];
    client.read_exact(&mut reply).expect("the reply reads");
    assert_eq!(reply, *b"reply", "the reply arrives before the drain");
    let mut end = [0; 1];
    assert_eq!(client.read(&mut end).expect("the write half closes"), 0);

    let deadline = Deadline::after(OP);
    let ack = loop {
        let ack = deadline.recv_or_fail(&acks, "the reply drain's clock wait");
        if ack.phase == "to drain the full reply" {
            break ack;
        }
    };
    assert!(
        clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
        "the drain waits on its injected-clock bound"
    );
    (clock, server, client, recorded_rx, drained_rx, ack)
}

/// The injected drain bound expires below, at and above its exact edge.
/// The production value is pinned, and each edge is driven only after the
/// drain announces its clock wait.
#[test]
fn reply_drain_bound_expires_at_the_exact_edge() {
    let bound = Duration::from_millis(100);
    assert_eq!(
        crate::http_server::connection::REPLY_DRAIN_WITHIN,
        bound,
        "the production drain bound is pinned"
    );

    let (clock, server, _client, requests, drained, ack) = start_reply_drain(bound);
    let before = bound
        .checked_sub(Duration::from_nanos(1))
        .expect("the bound is at least one nanosecond");
    #[allow(
        clippy::disallowed_methods,
        reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
    )]
    let mark = clock.advance_marked(before);
    assert!(
        clock.await_parked_since(&mark, ack.until, Deadline::after(OP)),
        "one tick below the bound the drain is still waiting"
    );
    assert!(
        drained.try_recv().is_err(),
        "one tick below the bound the reply drain has not ended"
    );
    #[allow(
        clippy::disallowed_methods,
        reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
    )]
    clock.advance(Duration::from_nanos(1));
    Deadline::after(OP).recv_or_fail(&drained, "the drain at its bound");
    let request = Deadline::after(OP).recv_or_fail(&requests, "the recorded request");
    assert_eq!(request.target, "/v1");
    assert!(server.shutdown(), "shutdown reaps the serving threads");

    let (clock, server, _client, requests, drained, _ack) = start_reply_drain(bound);
    #[allow(
        clippy::disallowed_methods,
        reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
    )]
    clock.advance(bound);
    Deadline::after(OP).recv_or_fail(&drained, "the drain at its bound");
    let request = Deadline::after(OP).recv_or_fail(&requests, "the recorded request");
    assert_eq!(request.target, "/v1");
    assert!(server.shutdown(), "shutdown reaps the serving threads");

    let (clock, server, _client, requests, drained, _ack) = start_reply_drain(bound);
    #[allow(
        clippy::disallowed_methods,
        reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
    )]
    clock.advance(bound + Duration::from_nanos(1));
    Deadline::after(OP).recv_or_fail(&drained, "the drain past its bound");
    let request = Deadline::after(OP).recv_or_fail(&requests, "the recorded request");
    assert_eq!(request.target, "/v1");
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

/// Bytes arriving during the drain do not renew its one operation budget.
#[test]
fn reply_drain_progress_does_not_restart_its_bound() {
    const SHORT: Duration = Duration::from_millis(50);
    let (clock, server, mut client, requests, drained, ack) = start_reply_drain(SHORT);
    client.write_all(b"x").expect("the pipelined byte writes");
    #[allow(
        clippy::disallowed_methods,
        reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
    )]
    let mark = clock.advance_marked(Duration::from_millis(1));
    assert!(
        clock.await_parked_since(&mark, ack.until, Deadline::after(OP)),
        "after consuming the byte, the drain waits again on the same bound"
    );
    let rest = SHORT
        .checked_sub(Duration::from_millis(1))
        .expect("the short bound exceeds one millisecond");
    #[allow(
        clippy::disallowed_methods,
        reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
    )]
    clock.advance(rest);
    Deadline::after(OP).recv_or_fail(&drained, "the unchanged drain bound");
    let request = Deadline::after(OP).recv_or_fail(&requests, "the recorded request");
    assert_eq!(request.target, "/v1");
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

/// Server shutdown wakes a drain parked on a much longer injected bound.
#[test]
fn shutdown_ends_a_reply_drain_with_an_open_peer() {
    let (clock, server, _client, requests, drained, ack) =
        start_reply_drain(Duration::from_secs(30));
    assert!(
        clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
        "the reply drain is waiting before shutdown"
    );
    assert!(
        server.shutdown(),
        "shutdown ends and reaps the active drain"
    );
    Deadline::after(OP).recv_or_fail(&drained, "the shutdown-stopped drain");
    let request = Deadline::after(OP).recv_or_fail(&requests, "the recorded request");
    assert_eq!(request.target, "/v1");
}

/// One raw loopback pair for the close-path unit tests: the server side
/// (nonblocking, owned by the test's connection) and the client side,
/// plus a probe clone of the server side that peeks without consuming,
/// so arrival is proven by the kernel instead of assumed.
fn raw_pair(server: &HttpServer, name: &str) -> (Connection, TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("the raw pair binds");
    let addr = listener.local_addr().expect("the raw pair has an address");
    let client = TcpStream::connect(addr).expect("the test connects");
    let (server_stream, _) = listener.accept().expect("the test accepts");
    server_stream
        .set_nonblocking(true)
        .expect("the drain reads nonblocking");
    let probe = server_stream
        .try_clone()
        .expect("the test clones its probe");
    probe
        .set_nonblocking(true)
        .expect("the probe peeks nonblocking");
    let budget = RequestBudget::start_on(
        &server.shared.clock,
        Duration::from_secs(10),
        server.shared.limits.guard,
    );
    let conn = Connection::new(
        server_stream,
        budget,
        name.to_owned(),
        Arc::clone(&server.shared),
    );
    (conn, client, probe)
}

/// Spins until `probe` peeks all `want` bytes, at most `OP`: the kernel
/// proves they arrived while leaving them unread for the drain under
/// test. The bound fails naming the awaited bytes instead of spinning
/// past a partial arrival or EOF.
fn await_peeked(probe: &TcpStream, want: usize, what: &str) {
    let deadline = Deadline::after(OP);
    let mut peeked = vec![0u8; want];
    loop {
        match probe.peek(&mut peeked) {
            Ok(n) if n == want => return,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("{what} peeks within {OP:?}: {error}"),
        }
        if deadline.left().is_zero() {
            panic!("waited {OP:?} for {what} ({want} bytes): fewer arrived");
        }
    }
}

/// The pre-reply discard consumes what already arrived: a no-op drain
/// would leave the bytes for the close to reset on, so the peek below
/// would still see them.
#[test]
fn drain_socket_discards_only_already_arrived_bytes() {
    let clock = fake_clock();
    let server = bind(&clock);
    let (mut conn, mut client, probe) = raw_pair(&server, "the drained request");
    client.write_all(b"held").expect("the held bytes write");
    await_peeked(&probe, 4, "the held bytes");
    conn.drain_socket();
    match probe.peek(&mut [0u8; 1]) {
        Ok(_) => panic!("the drain leaves no byte unread"),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(error) => panic!("the drained socket peeks within {OP:?}: {error}"),
    }
    // A drain on an empty socket ends at once, consuming nothing.
    conn.drain_socket();
    match probe.peek(&mut [0u8; 1]) {
        Ok(_) => panic!("the second drain leaves no byte unread"),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(error) => panic!("the twice-drained socket peeks within {OP:?}: {error}"),
    }
    assert!(server.shutdown(), "shutdown reaps the server");
}

/// Shutdown ends the reply drain before any read: with `&&` the drain
/// would consume the held bytes first, so the peek below would miss them.
#[test]
fn reply_drain_with_shutdown_returns_without_consuming() {
    let clock = fake_clock();
    let server = bind(&clock);
    let (mut conn, mut client, probe) = raw_pair(&server, "the stopping drain");
    client.write_all(b"held").expect("the held bytes write");
    await_peeked(&probe, 4, "the held bytes");
    assert!(server.shutdown(), "shutdown stops the server");
    conn.shutdown_write_and_drain(Duration::from_secs(30));
    let mut peeked = [0u8; 4];
    match probe.peek(&mut peeked) {
        Ok(4) => assert_eq!(peeked, *b"held", "the stopped drain consumes nothing"),
        Ok(n) => panic!("the stopped drain leaves 4 bytes unread, peeked {n}"),
        Err(error) => panic!("the stopped drain leaves the bytes unread: {error}"),
    }
}

/// An expired bound ends the reply drain before any read: with `&&` the
/// drain would consume the held bytes first, so the peek below would
/// miss them.
#[test]
fn reply_drain_when_expired_returns_without_consuming() {
    let clock = fake_clock();
    let server = bind(&clock);
    let (mut conn, mut client, probe) = raw_pair(&server, "the expired drain");
    client.write_all(b"held").expect("the held bytes write");
    await_peeked(&probe, 4, "the held bytes");
    conn.shutdown_write_and_drain(Duration::ZERO);
    let mut peeked = [0u8; 4];
    match probe.peek(&mut peeked) {
        Ok(4) => assert_eq!(peeked, *b"held", "the expired drain consumes nothing"),
        Ok(n) => panic!("the expired drain leaves 4 bytes unread, peeked {n}"),
        Err(error) => panic!("the expired drain leaves the bytes unread: {error}"),
    }
    assert!(server.shutdown(), "shutdown reaps the server");
}

/// A released pause before a full reply still sends every scripted
/// byte: the reply is `Done`, and the wire carries the head and body
/// exactly. A flipped pause check would stop the reply instead, so the
/// client would see an empty close.
#[test]
fn a_released_pause_before_a_full_reply_sends_every_byte() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let (pause, control) = Pause::new();
    let (result_tx, result_rx) = mpsc::channel();
    let (raw_tx, raw_rx) = mpsc::channel();
    let head = b"HTTP/1.1 200 Fake\r\ncontent-length: 2\r\nconnection: close\r\n\r\n";
    let (server, addr) = serve_once(recorded_tx, move |mut conn| {
        let end = conn.reply_full(head, b"ok", Some(&pause));
        match result_tx.send(end) {
            Ok(()) | Err(_) => {}
        }
    });
    thread::scope(|scope| {
        scope.spawn(|| {
            let raw = exchange(addr, b"GET /v1 HTTP/1.1\r\nHost: fake\r\n\r\n");
            match raw_tx.send(raw) {
                Ok(()) | Err(_) => {}
            }
        });
        control.wait_hit(&Deadline::after(OP), "the pause before the response");
        control.release();
        let raw = Deadline::after(OP).recv_or_fail(&raw_rx, "the client's reply");
        let mut expected = head.to_vec();
        expected.extend_from_slice(b"ok");
        assert_eq!(raw, expected, "the wire carries the scripted bytes");
        assert_eq!(
            Deadline::after(OP).recv_or_fail(&result_rx, "the full reply"),
            ReplyEnd::Done,
            "the released reply sends"
        );
        Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
    });
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

/// Shutdown during the pause before a full reply stops it: the reply
/// is `Stopped`, never a full reply from a stopped server. A flipped
/// pause check would send the reply anyway, answering `Done`.
#[test]
fn shutdown_during_the_pause_before_a_full_reply_stops_it() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let (pause, control) = Pause::new();
    let (result_tx, result_rx) = mpsc::channel();
    let head = b"HTTP/1.1 200 Fake\r\ncontent-length: 2\r\nconnection: close\r\n\r\n";
    let (server, addr) = serve_once(recorded_tx, move |mut conn| {
        let end = conn.reply_full(head, b"ok", Some(&pause));
        match result_tx.send(end) {
            Ok(()) | Err(_) => {}
        }
    });
    let mut client = TcpStream::connect(addr).expect("the test connects");
    client
        .set_read_timeout(Some(OP))
        .expect("the test bounds its reads");
    client
        .write_all(b"GET /v1 HTTP/1.1\r\nHost: fake\r\n\r\n")
        .expect("the request writes");
    control.wait_hit(&Deadline::after(OP), "the pause before the response");
    assert!(server.shutdown(), "shutdown ends the paused reply");
    assert_eq!(
        Deadline::after(OP).recv_or_fail(&result_rx, "the stopped reply"),
        ReplyEnd::Stopped,
        "shutdown stops the paused reply"
    );
    Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
}

/// Sends one fixed-length body with `pipelined` bytes behind it, all at
/// once, then half-closes and reads the reply to the connection's end.
/// The body is larger than one socket read, so the head's over-read
/// cannot swallow the suffix into the lookahead: it sits on the socket,
/// unread, when the reply's close runs, and the suffix is queued before
/// the server answers, so no wait decides the outcome.
fn exchange_pipelined(addr: SocketAddr, body: &[u8], pipelined: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut stream = TcpStream::connect(addr).expect("the test connects");
    stream
        .set_read_timeout(Some(OP))
        .expect("the test bounds its reads");
    stream
        .write_all(
            format!(
                "POST /v1 HTTP/1.1\r\nHost: fake\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .expect("the head writes");
    stream.write_all(body).expect("the body writes");
    stream
        .write_all(pipelined)
        .expect("the pipelined bytes write");
    // Half-closes: the reply's drain meets the suffix, then the end.
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("the write side closes");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("the reply reads");
    raw
}

/// A fixed body larger than one socket read, for the pipelined-suffix
/// tests below: small enough to burst at once, large enough that the
/// head's over-read cannot reach past it into the suffix.
fn large_body() -> Vec<u8> {
    vec![b'x'; 20_000]
}

/// A full reply after a half-close with nothing behind the body still
/// delivers: the reply's drain meets the peer's end at once, so the
/// close behind it is clean. The zero-byte edge of the drain's end
/// check: without it the drain would spin on the end below instead of
/// closing.
#[test]
fn full_reply_after_a_half_close_with_no_unread_bytes_delivers() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let head = b"HTTP/1.1 200 Fake\r\ncontent-length: 2\r\nconnection: close\r\n\r\n";
    let (server, addr) = serve_once(recorded_tx, move |mut conn| {
        assert_eq!(
            conn.reply_full(head, b"ok", None),
            ReplyEnd::Done,
            "the full reply sends"
        );
    });
    let raw = exchange_pipelined(addr, &large_body(), b"");
    let mut expected = head.to_vec();
    expected.extend_from_slice(b"ok");
    assert_eq!(raw, expected, "the wire carries the scripted bytes");
    let request = Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
    assert_eq!(request.target, "/v1");
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

/// A full reply past one unread byte still delivers: the reply's drain
/// consumes what arrived behind the body before the close behind it, so
/// the close carries no reset to the client. The one-byte edge past the
/// drain's end check: without the drain the byte sits unread at close
/// and the read below resets instead.
#[test]
fn full_reply_delivers_past_one_unread_byte() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let head = b"HTTP/1.1 200 Fake\r\ncontent-length: 2\r\nconnection: close\r\n\r\n";
    let (server, addr) = serve_once(recorded_tx, move |mut conn| {
        assert_eq!(
            conn.reply_full(head, b"ok", None),
            ReplyEnd::Done,
            "the full reply sends"
        );
    });
    let raw = exchange_pipelined(addr, &large_body(), b"y");
    let mut expected = head.to_vec();
    expected.extend_from_slice(b"ok");
    assert_eq!(raw, expected, "the wire carries the scripted bytes");
    let request = Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
    assert_eq!(request.target, "/v1");
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

/// A full reply past more than one read of unread bytes still delivers:
/// the reply's drain keeps consuming until the socket runs dry, so no
/// tail past its first read sits unread at close. Well past the drain's
/// end check and past one socket read: stopping after one read would
/// leave most of the suffix unread and reset the read below instead.
#[test]
fn full_reply_delivers_past_more_than_one_read_of_unread_bytes() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let head = b"HTTP/1.1 200 Fake\r\ncontent-length: 2\r\nconnection: close\r\n\r\n";
    let (server, addr) = serve_once(recorded_tx, move |mut conn| {
        assert_eq!(
            conn.reply_full(head, b"ok", None),
            ReplyEnd::Done,
            "the full reply sends"
        );
    });
    let raw = exchange_pipelined(addr, &large_body(), &vec![b'y'; 20_000]);
    let mut expected = head.to_vec();
    expected.extend_from_slice(b"ok");
    assert_eq!(raw, expected, "the wire carries the scripted bytes");
    let request = Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
    assert_eq!(request.target, "/v1");
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

#[test]
fn chunked_pieces_have_exact_framing() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let head = b"HTTP/1.1 200 Fake\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
    let (server, addr) = serve_once(recorded_tx, move |mut conn| {
        assert_eq!(
            conn.reply_stream(
                head,
                &[b"ok".to_vec(), b"bye".to_vec()],
                Framing::Chunked,
                None
            ),
            ReplyEnd::Done,
            "the pieces send"
        );
    });
    let raw = exchange(addr, b"GET /v1 HTTP/1.1\r\nHost: fake\r\n\r\n");
    let mut expected = head.to_vec();
    expected.extend_from_slice(b"2\r\nok\r\n3\r\nbye\r\n0\r\n\r\n");
    assert_eq!(raw, expected, "each piece carries its exact framing");
    Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

#[test]
fn sse_pieces_have_no_inserted_length() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let head = b"HTTP/1.1 200 Fake\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
    let events = b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":\"el-1\"}\n\n";
    let (server, addr) = serve_once(recorded_tx, move |mut conn| {
        assert_eq!(
            conn.reply_stream(head, &[events.to_vec()], Framing::Raw, None),
            ReplyEnd::Done,
            "the stream sends"
        );
    });
    let raw = exchange(
        addr,
        b"POST /mcp HTTP/1.1\r\nHost: fake\r\nContent-Length: 0\r\n\r\n",
    );
    let mut expected = head.to_vec();
    expected.extend_from_slice(events);
    assert_eq!(raw, expected, "stream pieces carry no inserted length");
    Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

#[test]
fn pause_before_reply_and_between_pieces_releases_on_sender_drop() {
    // Two pieces: one pause before the reply, one between the pieces.
    // The second pause releases when its sender drops, not by an
    // explicit send.
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let (pause, control) = Pause::new();
    let head = b"HTTP/1.1 200 Fake\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
    let (server, addr) = serve_once(recorded_tx, move |mut conn| {
        assert_eq!(
            conn.reply_stream(
                head,
                &[b"one".to_vec(), b"two".to_vec()],
                Framing::Chunked,
                Some(&pause),
            ),
            ReplyEnd::Done,
            "the paused stream sends"
        );
    });
    let mut reader = {
        let stream = TcpStream::connect(addr).expect("the test connects");
        stream
            .set_read_timeout(Some(OP))
            .expect("the test bounds its reads");
        let mut reader = std::io::BufReader::new(stream);
        reader
            .get_mut()
            .write_all(b"GET /v1 HTTP/1.1\r\nHost: fake\r\n\r\n")
            .expect("the request writes");
        reader
    };
    let deadline = Deadline::after(OP);
    control.wait_hit(&deadline, "the pause before the reply");
    control.release();
    control.wait_hit(&deadline, "the pause between the pieces");
    drop(control);
    use std::io::Read;
    let mut raw = Vec::new();
    reader.read_to_end(&mut raw).expect("the stream reads");
    let mut expected = head.to_vec();
    expected.extend_from_slice(b"3\r\none\r\n3\r\ntwo\r\n0\r\n\r\n");
    assert_eq!(raw, expected, "both pieces arrive after their pauses");
    Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

#[test]
fn stall_records_prefix_then_peer_close() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let (sent_tx, sent_rx) = mpsc::channel();
    let head = b"HTTP/1.1 200 Fake\r\ncontent-length: 100\r\nconnection: close\r\n\r\n";
    let (server, addr) = serve_once(recorded_tx, move |mut conn| {
        let end = conn.stall(head, b"partial", None);
        match sent_tx.send(end) {
            Ok(()) | Err(_) => {}
        }
    });
    let mut stream = TcpStream::connect(addr).expect("the test connects");
    stream
        .set_read_timeout(Some(OP))
        .expect("the test bounds its reads");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: fake\r\n\r\n")
        .expect("the request writes");
    let mut reader = std::io::BufReader::new(stream.try_clone().expect("the test clones"));
    let mut status = String::new();
    use std::io::BufRead;
    reader
        .read_line(&mut status)
        .expect("the stall's status reads");
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("the stall's head reads");
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
    }
    let mut prefix = vec![0u8; 7];
    use std::io::Read;
    reader.read_exact(&mut prefix).expect("the prefix reads");
    assert_eq!(prefix, b"partial");
    Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
    // Nothing more arrives while the client holds the connection.
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("the test bounds its reads");
    let mut one = [0u8; 1];
    assert!(
        reader.read_exact(&mut one).is_err(),
        "the connection holds past its prefix"
    );
    drop(reader);
    drop(stream);
    assert_eq!(
        Deadline::after(OP).recv_or_fail(&sent_rx, "the stalled peer's close"),
        StallEnd::Closed,
        "the client's close ends the stall"
    );
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

#[test]
fn dropped_reply_records_request_and_sends_nothing() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let (server, addr) = serve_once(recorded_tx, |conn| {
        drop(conn);
    });
    let raw = exchange(
        addr,
        b"POST /v1 HTTP/1.1\r\nHost: fake\r\nContent-Length: 0\r\n\r\n",
    );
    assert!(raw.is_empty(), "a dropped reply sends nothing");
    let request = Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
    assert_eq!(request.target, "/v1");
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

/// A writer with a finite capacity: past it, every write would block.
/// The test stops it, so a mutant that spins past every wait still ends.
struct TestWriter {
    capacity: usize,
    written: Vec<u8>,
    stop: Arc<AtomicBool>,
}

impl TestWriter {
    fn finished(capacity: usize, stop: Arc<AtomicBool>) -> Self {
        Self {
            capacity,
            written: Vec::new(),
            stop,
        }
    }
}

impl std::io::Write for TestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.stop.load(Ordering::SeqCst) {
            self.written.extend_from_slice(bytes);
            return Ok(bytes.len());
        }
        let room = self.capacity.saturating_sub(self.written.len());
        if room == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "the test writer is full",
            ));
        }
        let take = room.min(bytes.len());
        self.written.extend_from_slice(&bytes[..take]);
        Ok(take)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn blocked_write_expires_on_the_injected_clock() {
    let clock = FakeClock::new();
    let server =
        HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let acks = server.test_acknowledgments();
    let addr = server.addr();
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the held request")
                .expect("a client connects");
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut writer = TestWriter::finished(10, Arc::clone(&stop));
                let op = conn.op_budget();
                let (_stream, mut ctx) = conn.parts(&op);
                pump(
                    &mut writer,
                    &[b'x'; 100],
                    "to write the held response",
                    &mut ctx,
                )
            }));
            match tx.send(caught) {
                Ok(()) | Err(_) => {}
            }
        });
        let _client = TcpStream::connect(addr).expect("the test connects");
        let acknowledgment_deadline = Deadline::after(OP);
        let ack = loop {
            match acknowledgment_deadline.recv(&acks) {
                Ok(ack) if ack.phase == "to write the held response" => break Some(ack),
                Ok(_) => continue,
                Err(_) => break None,
            }
        };
        let ack = match ack {
            Some(ack) => ack,
            None => {
                // No clock wait: a spinner never parks. Release its
                // writer and take its verdict instead of hanging on it.
                stop.store(true, Ordering::SeqCst);
                let verdict = Deadline::after(OP).recv_or_fail(&rx, "the spinning write");
                let message = failure_text(verdict, "the held write");
                panic!("the held write sent instead of failing: {message}");
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the write parks on the clock before it moves"
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(10) - Duration::from_nanos(1));
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_nanos(1));
        let caught: thread::Result<bool> = Deadline::after(OP).recv_or_fail(&rx, "the held write");
        let message = match caught {
            Ok(_) => panic!("the held write sent instead of failing"),
            Err(payload) => payload
                .downcast_ref::<String>()
                .cloned()
                .expect("the failure names its request"),
        };
        assert!(
            message.contains("the held request"),
            "the failure names its request, got: {message}"
        );
    });
}

#[test]
fn handoff_keeps_bytes_after_the_connect_head() {
    let clock = FakeClock::new();
    let server = HttpServer::bind(clock as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let addr = server.addr();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the CONNECT request")
                .expect("a client connects");
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let request = match request::read_request(&mut conn) {
                    request::RequestOutcome::Complete(request) => request,
                    request::RequestOutcome::Eof => panic!("the peer closed the CONNECT head"),
                    request::RequestOutcome::Stopped => panic!("shutdown ended the CONNECT read"),
                    request::RequestOutcome::Invalid(why) => {
                        panic!("the CONNECT head was invalid: {why}")
                    }
                };
                assert_eq!(request.method, "CONNECT");
                conn.into_handoff().expect("the handoff keeps the stream")
            }));
            match tx.send(outcome) {
                Ok(()) | Err(_) => {}
            }
        });
        let mut client = TcpStream::connect(addr).expect("the test connects");
        client
            .write_all(b"CONNECT 127.0.0.1:80 HTTP/1.1\r\nHost: 127.0.0.1:80\r\n\r\nPING")
            .expect("the CONNECT writes");
        let (stream, ahead) = match Deadline::after(OP).recv_or_fail(&rx, "the handoff") {
            Ok(handoff) => handoff,
            Err(payload) => panic!(
                "the handoff failed: {}",
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| "a non-string panic".to_owned())
            ),
        };
        assert_eq!(ahead, b"PING", "bytes past the head survive the handoff");
        drop(stream);
    });
}

#[test]
fn shutdown_releases_pauses_and_reaps_core_waits() {
    let clock = FakeClock::new();
    let server = HttpServer::bind(clock as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let (pause, control) = Pause::new();
    let (paused_tx, paused_rx) = mpsc::channel();
    server.serve_persistent(move |mut conn| {
        // Held in a core pause, then in a core read: shutdown must
        // release both without the test sending anything.
        let request = match request::read_request(&mut conn) {
            request::RequestOutcome::Complete(request) => request,
            request::RequestOutcome::Eof => return,
            request::RequestOutcome::Stopped => return,
            request::RequestOutcome::Invalid(_) => return,
        };
        drop(request);
        if conn.pause(&pause, &conn.op_budget(), "to pause the test response") == PauseEnd::Stopped
        {
            match paused_tx.send("pause") {
                Ok(()) | Err(_) => {}
            }
            return;
        }
        match paused_tx.send("released") {
            Ok(()) | Err(_) => {}
        }
    });
    let mut client = TcpStream::connect(server.addr()).expect("the test connects");
    client
        .write_all(b"GET /held HTTP/1.1\r\nHost: fake\r\n\r\n")
        .expect("the request writes");
    control.wait_hit(&Deadline::after(OP), "the held pause");
    // The control is never released: shutdown ends the pause instead.
    assert!(server.shutdown(), "shutdown releases the pause");
    assert_eq!(
        Deadline::after(OP).recv_or_fail(&paused_rx, "the released pause"),
        "pause",
        "shutdown ends the pause stopped"
    );
    drop(control);
}

/// The defaults the module doc states: pinning each value kills a mutant
/// that lowers a cap past what any boundary test reaches through small
/// injected limits.
#[test]
fn default_limits_pin_their_documented_values() {
    let limits = Limits::default();
    assert_eq!(
        limits.request,
        Duration::from_secs(30),
        "one request bound covers the whole read"
    );
    assert_eq!(
        limits.guard,
        Duration::from_secs(45),
        "one real hang guard backs the bound"
    );
    assert_eq!(
        limits.poll,
        Duration::from_millis(1),
        "one poll interval bounds every park"
    );
    assert_eq!(limits.line, 64 * 1024, "request-line bytes");
    assert_eq!(limits.frame, 64 * 1024, "header and chunk-line bytes");
    assert_eq!(limits.headers, 1024 * 1024, "aggregate header bytes");
    assert_eq!(limits.header_count, 4096, "header count");
    assert_eq!(limits.body, 64 * 1024 * 1024, "decoded body bytes");
    assert_eq!(limits.trailers, 1024 * 1024, "aggregate trailer bytes");
    assert_eq!(limits.trailer_count, 4096, "trailer count");
}

/// A wait with no wake parks: with a minute poll, a parked waiter enters
/// the core's wait once, while one whose park returns at once re-enters
/// without any wake.
#[test]
fn a_wait_without_a_wake_parks_instead_of_spinning() {
    let clock = fake_clock();
    let server = HttpServer::bind(
        clock.clone() as Arc<dyn Clock>,
        Limits {
            request: Duration::from_secs(10),
            poll: Duration::from_secs(60),
            ..Limits::default()
        },
    )
    .expect("the core binds");
    let acks = server.test_acknowledgments();
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let accepted = server.accept_expected("the parked request").is_some();
            match done_tx.send(accepted) {
                Ok(()) | Err(_) => {}
            }
        });
        let deadline = Deadline::after(OP);
        let ack = next_acknowledgment(&deadline, &acks, "the accept's clock wait");
        assert_eq!(ack.phase, "to accept the connection");
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the accept parks on the clock before it moves"
        );
        let entered = server.test_wait_entries();
        assert!(
            Deadline::after(QUIET).recv(&done_rx).is_err(),
            "the accept waits instead of ending"
        );
        let reentered = server.test_wait_entries();
        server.shutdown();
        assert!(
            reentered - entered <= 3,
            "a parked wait re-enters the core's wait only past a wake, entered {entered}, now {reentered}"
        );
    });
}

/// A silent connection is not closed: the stall waits for the peer, and
/// only its close ends the wait.
#[test]
fn a_silent_connection_is_not_closed() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let addr = server.addr();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the stalled request")
                .expect("a client connects");
            let end = conn.stall_wait();
            match tx.send(end) {
                Ok(()) | Err(_) => {}
            }
        });
        let client = TcpStream::connect(addr).expect("the test connects");
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the stall's clock wait");
            if ack.phase == "to wait for the stalled peer" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the stall parks on the clock before it moves"
        );
        assert!(
            Deadline::after(QUIET).recv(&rx).is_err(),
            "a silent stall waits instead of closing"
        );
        drop(client);
        let end = Deadline::after(OP).recv(&rx);
        server.shutdown();
        assert_eq!(
            end.ok(),
            Some(StallEnd::Closed),
            "the peer's close ends the stall"
        );
    });
}

/// A reset connection ends a fill at once: anything but a would-block
/// wait or an interrupted retry is the peer's end, not another wait.
/// The client peeks the server's bytes, proving they arrived without
/// consuming them, so its close resets with bytes unread: the unread
/// count is proven, not timed. The error-kind mapping itself is pinned
/// by read_errors_classify_distinctly, which needs no socket.
#[test]
fn a_reset_connection_ends_a_fill_at_once() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let addr = server.addr();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the reset request")
                .expect("a client connects");
            let op = conn.op_budget();
            let (stream, mut ctx) = conn.parts(&op);
            assert!(
                pump(stream, &[b'x'; 8], "to write before the reset", &mut ctx),
                "the bytes the reset leaves unread write"
            );
            let outcome = conn.fill("to read the reset request");
            match tx.send(outcome) {
                Ok(()) | Err(_) => {}
            }
        });
        let client = TcpStream::connect(addr).expect("the test connects");
        client
            .set_read_timeout(Some(OP))
            .expect("the test bounds its reads");
        // Peeking proves all eight arrived while leaving them unread, so
        // the close below resets instead of closing cleanly.
        let mut peeked = [0u8; 8];
        loop {
            match client.peek(&mut peeked) {
                Ok(8) => break,
                Ok(_) => continue,
                Err(error) => panic!("the peek reads within {OP:?}: {error}"),
            }
        }
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the fill's clock wait");
            if ack.phase == "to read the reset request" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the fill parks on the clock before it moves"
        );
        drop(client);
        let outcome = Deadline::after(QUIET).recv(&rx);
        server.shutdown();
        assert_eq!(
            outcome.ok(),
            Some(FillOutcome::Eof),
            "a reset ends the fill"
        );
    });
}

/// A silent fill waits for its bytes: a would-block wait parks, and the
/// first byte ends it held.
#[test]
fn a_silent_fill_waits_for_its_bytes() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let addr = server.addr();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the silent request")
                .expect("a client connects");
            let outcome = conn.fill("to read the silent request");
            match tx.send(outcome) {
                Ok(()) | Err(_) => {}
            }
        });
        let mut client = TcpStream::connect(addr).expect("the test connects");
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the fill's clock wait");
            if ack.phase == "to read the silent request" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the fill parks on the clock before it moves"
        );
        assert!(
            Deadline::after(QUIET).recv(&rx).is_err(),
            "a silent fill waits instead of ending"
        );
        client.write_all(b"x").expect("the byte writes");
        assert_eq!(
            Deadline::after(OP).recv_or_fail(&rx, "the fed fill"),
            FillOutcome::Held(1),
            "the first byte ends the fill held"
        );
        drop(client);
    });
}

/// A reset stall reports the close: the peer is gone, whatever error its
/// reset reads as. The client peeks the stalled bytes, proving they
/// arrived without consuming them, so its close resets with bytes unread.
#[test]
fn a_reset_stall_reports_the_close() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let addr = server.addr();
    let head = b"HTTP/1.1 200 Fake\r\ncontent-length: 100\r\nconnection: close\r\n\r\n";
    let prefix = b"partial";
    let total = head.len() + prefix.len();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the reset stall")
                .expect("a client connects");
            assert!(
                conn.stall_send(head, prefix, None),
                "the stalled head sends"
            );
            let end = conn.stall_wait();
            match tx.send(end) {
                Ok(()) | Err(_) => {}
            }
        });
        let client = TcpStream::connect(addr).expect("the test connects");
        client
            .set_read_timeout(Some(OP))
            .expect("the test bounds its reads");
        // Peeking proves the whole stall arrived while leaving it unread,
        // so the close below resets instead of closing cleanly.
        let mut peeked = vec![0u8; total];
        loop {
            match client.peek(&mut peeked) {
                Ok(n) if n == total => break,
                Ok(_) => continue,
                Err(error) => panic!("the peek reads within {OP:?}: {error}"),
            }
        }
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the stall's clock wait");
            if ack.phase == "to wait for the stalled peer" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the stall parks on the clock before it moves"
        );
        drop(client);
        let end = Deadline::after(QUIET).recv(&rx);
        server.shutdown();
        assert_eq!(
            end.ok(),
            Some(StallEnd::Closed),
            "a reset ends the stall closed"
        );
    });
}

/// A writer whose flush fails a programmed number of times, then succeeds.
/// The test stops it, so a mutant that spins past every wait still ends.
struct FlushFaults {
    written: Vec<u8>,
    kind: std::io::ErrorKind,
    failures: usize,
    stop: Arc<AtomicBool>,
}

impl FlushFaults {
    fn fails(kind: std::io::ErrorKind, failures: usize, stop: Arc<AtomicBool>) -> Self {
        Self {
            written: Vec::new(),
            kind,
            failures,
            stop,
        }
    }
}

impl std::io::Write for FlushFaults {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.written.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        if self.failures > 0 {
            self.failures -= 1;
            return Err(std::io::Error::new(self.kind, "the test flush fails"));
        }
        Ok(())
    }
}

/// An interrupted flush retries: one interruption is not the peer's end.
#[test]
fn an_interrupted_flush_retries() {
    let clock = FakeClock::new();
    let server =
        HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let addr = server.addr();
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the flushed request")
                .expect("a client connects");
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut writer =
                    FlushFaults::fails(std::io::ErrorKind::Interrupted, 1, Arc::clone(&stop));
                let op = conn.op_budget();
                let (_stream, mut ctx) = conn.parts(&op);
                pump(&mut writer, b"ok", "to flush the response", &mut ctx)
            }));
            match tx.send(caught) {
                Ok(()) | Err(_) => {}
            }
        });
        let _client = TcpStream::connect(addr).expect("the test connects");
        match Deadline::after(QUIET).recv(&rx) {
            Ok(caught) => assert_eq!(caught.ok(), Some(true), "an interrupted flush retries"),
            Err(_) => {
                server.shutdown();
                if Deadline::after(OP).recv(&rx).is_err() {
                    // Shutdown released no one: a spinner never parks.
                    // Stop its writer and reap it instead of hanging.
                    stop.store(true, Ordering::SeqCst);
                    match Deadline::after(OP).recv(&rx) {
                        Ok(_) | Err(_) => {}
                    }
                }
                panic!("an interrupted flush retries instead of waiting out the flush");
            }
        }
    });
}

/// A failed flush ends the write: a permanent error is the peer's end,
/// not another wait.
#[test]
fn a_failed_flush_ends_the_write() {
    let clock = FakeClock::new();
    let server =
        HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let addr = server.addr();
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the failed request")
                .expect("a client connects");
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut writer = FlushFaults::fails(
                    std::io::ErrorKind::BrokenPipe,
                    usize::MAX,
                    Arc::clone(&stop),
                );
                let op = conn.op_budget();
                let (_stream, mut ctx) = conn.parts(&op);
                pump(&mut writer, b"ok", "to flush the failed response", &mut ctx)
            }));
            match tx.send(caught) {
                Ok(()) | Err(_) => {}
            }
        });
        let _client = TcpStream::connect(addr).expect("the test connects");
        match Deadline::after(QUIET).recv(&rx) {
            Ok(caught) => assert_eq!(caught.ok(), Some(false), "a failed flush ends the write"),
            Err(_) => {
                server.shutdown();
                if Deadline::after(OP).recv(&rx).is_err() {
                    stop.store(true, Ordering::SeqCst);
                    match Deadline::after(OP).recv(&rx) {
                        Ok(_) | Err(_) => {}
                    }
                }
                panic!("a failed flush ends the write instead of waiting out the flush");
            }
        }
    });
}

/// A writer whose flush would block until the test stops it.
struct GatedFlush {
    written: Vec<u8>,
    stop: Arc<AtomicBool>,
}

impl std::io::Write for GatedFlush {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.written.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "the test flush would block",
        ))
    }
}

/// A flush that would block waits out its bound: expiry names the request
/// past it, instead of spinning past the wait or ending the write.
#[test]
fn a_would_block_flush_waits_out_its_bound() {
    let clock = FakeClock::new();
    let server =
        HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let acks = server.test_acknowledgments();
    let addr = server.addr();
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the held request")
                .expect("a client connects");
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut writer = GatedFlush {
                    written: Vec::new(),
                    stop: Arc::clone(&stop),
                };
                let op = conn.op_budget();
                let (_stream, mut ctx) = conn.parts(&op);
                pump(&mut writer, b"ok", "to flush the held response", &mut ctx)
            }));
            match tx.send(caught) {
                Ok(()) | Err(_) => {}
            }
        });
        let _client = TcpStream::connect(addr).expect("the test connects");
        let acknowledgment_deadline = Deadline::after(OP);
        let ack = loop {
            let ack = acknowledgment_deadline.recv_or_fail(&acks, "the flush's clock wait");
            if ack.phase == "to flush the held response" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the flush parks on the clock before it moves"
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(10) - Duration::from_nanos(1));
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_nanos(1));
        // A spinner never waited out its bound, so no verdict arrives:
        // release its writer and take its verdict instead.
        let verdict = match Deadline::after(OP).recv(&rx) {
            Ok(caught) => caught,
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                Deadline::after(OP).recv_or_fail(&rx, "the spinning flush")
            }
        };
        let message = failure_text(verdict, "the held flush");
        assert!(
            message.contains("the held request"),
            "the failure names its request, got: {message}"
        );
    });
}

/// A writer whose writes fail a programmed number of times, then succeed.
/// The test stops it, so a mutant that spins past every wait still ends.
struct WriteFaults {
    written: Vec<u8>,
    kind: std::io::ErrorKind,
    failures: usize,
    stop: Arc<AtomicBool>,
}

impl WriteFaults {
    fn fails(kind: std::io::ErrorKind, failures: usize, stop: Arc<AtomicBool>) -> Self {
        Self {
            written: Vec::new(),
            kind,
            failures,
            stop,
        }
    }
}

impl std::io::Write for WriteFaults {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.stop.load(Ordering::SeqCst) {
            self.written.extend_from_slice(bytes);
            return Ok(bytes.len());
        }
        if self.failures > 0 {
            self.failures -= 1;
            return Err(std::io::Error::new(self.kind, "the test write fails"));
        }
        self.written.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An interrupted write retries: one interruption is not the peer's end.
#[test]
fn an_interrupted_write_retries() {
    let clock = FakeClock::new();
    let server =
        HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let addr = server.addr();
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the written request")
                .expect("a client connects");
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut writer =
                    WriteFaults::fails(std::io::ErrorKind::Interrupted, 1, Arc::clone(&stop));
                let op = conn.op_budget();
                let (_stream, mut ctx) = conn.parts(&op);
                pump(&mut writer, b"ok", "to write the response", &mut ctx)
            }));
            match tx.send(caught) {
                Ok(()) | Err(_) => {}
            }
        });
        let _client = TcpStream::connect(addr).expect("the test connects");
        match Deadline::after(QUIET).recv(&rx) {
            Ok(caught) => assert_eq!(caught.ok(), Some(true), "an interrupted write retries"),
            Err(_) => {
                server.shutdown();
                if Deadline::after(OP).recv(&rx).is_err() {
                    stop.store(true, Ordering::SeqCst);
                    match Deadline::after(OP).recv(&rx) {
                        Ok(_) | Err(_) => {}
                    }
                }
                panic!("an interrupted write retries instead of waiting out the write");
            }
        }
    });
}

/// A failed write ends at once: a permanent error is the peer's end, not
/// another wait.
#[test]
fn a_failed_write_ends_at_once() {
    let clock = FakeClock::new();
    let server =
        HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let addr = server.addr();
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the broken request")
                .expect("a client connects");
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut writer = WriteFaults::fails(
                    std::io::ErrorKind::BrokenPipe,
                    usize::MAX,
                    Arc::clone(&stop),
                );
                let op = conn.op_budget();
                let (_stream, mut ctx) = conn.parts(&op);
                pump(&mut writer, b"ok", "to write the broken response", &mut ctx)
            }));
            match tx.send(caught) {
                Ok(()) | Err(_) => {}
            }
        });
        let _client = TcpStream::connect(addr).expect("the test connects");
        match Deadline::after(QUIET).recv(&rx) {
            Ok(caught) => assert_eq!(caught.ok(), Some(false), "a failed write ends at once"),
            Err(_) => {
                server.shutdown();
                if Deadline::after(OP).recv(&rx).is_err() {
                    stop.store(true, Ordering::SeqCst);
                    match Deadline::after(OP).recv(&rx) {
                        Ok(_) | Err(_) => {}
                    }
                }
                panic!("a failed write ends at once instead of waiting out the write");
            }
        }
    });
}

/// A stalled send holds on its pause: release sends the head and prefix,
/// while shutdown ends the send unsent.
#[test]
fn a_stalled_pause_releases_and_stops() {
    let head = b"HTTP/1.1 200 Fake\r\ncontent-length: 100\r\nconnection: close\r\n\r\n";
    // Release sends the stalled head and prefix.
    let clock = FakeClock::new();
    let server = bind(&clock);
    let addr = server.addr();
    let (pause, control) = Pause::new();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the paused stall")
                .expect("a client connects");
            let sent = conn.stall_send(head, b"partial", Some(&pause));
            match tx.send(sent) {
                Ok(()) | Err(_) => {}
            }
        });
        let client = TcpStream::connect(addr).expect("the test connects");
        let deadline = Deadline::after(OP);
        control.wait_hit(&deadline, "the pause before the stalled response");
        assert!(
            Deadline::after(QUIET).recv(&rx).is_err(),
            "the stalled send waits on its pause"
        );
        control.release();
        assert!(
            Deadline::after(OP).recv_or_fail(&rx, "the released stall"),
            "release sends the stalled head"
        );
        drop(client);
    });
    // Shutdown while paused ends the send unsent.
    let clock = FakeClock::new();
    let server = bind(&clock);
    let addr = server.addr();
    let (pause, control) = Pause::new();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the stopped stall")
                .expect("a client connects");
            let sent = conn.stall_send(head, b"partial", Some(&pause));
            match tx.send(sent) {
                Ok(()) | Err(_) => {}
            }
        });
        let _client = TcpStream::connect(addr).expect("the test connects");
        control.wait_hit(&Deadline::after(OP), "the pause before the stopped stall");
        assert!(server.shutdown(), "shutdown ends the paused send");
        assert!(
            !Deadline::after(OP).recv_or_fail(&rx, "the stopped stall"),
            "shutdown ends the stalled send unsent"
        );
    });
}

/// Dropping the server refuses connections: the drop stops the listener,
/// not just the last handle.
#[test]
fn dropping_the_server_refuses_connections() {
    let clock = fake_clock();
    let server = bind(&clock);
    server.serve_persistent(|_| {});
    let probe = server.drop_probe();
    drop(server);
    assert!(
        probe.listener_closed(),
        "a dropped server closes its listener"
    );
    assert!(
        probe.accept_joined(),
        "a dropped server joins its accept thread"
    );
}

/// The probe reads the live server before the drop and the joined one
/// after: a reused port can no longer flake the verdict, since nothing
/// dials it. False before the drop kills a constant-true accessor, and
/// true after kills a constant-false one.
#[test]
fn drop_probe_reads_live_then_joined() {
    let clock = fake_clock();
    let server = bind(&clock);
    server.serve_persistent(|_| {});
    let probe = server.drop_probe();
    assert!(
        !probe.listener_closed(),
        "a serving server still holds its listener"
    );
    assert!(
        !probe.accept_joined(),
        "a serving server has joined nothing yet"
    );
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

/// A server that never served still reads joined after the drop: the
/// shutdown that found no handle on a never-spawned slot records the
/// join. A condition on the wrong spawn arm leaves this false.
#[test]
fn drop_probe_marks_a_never_served_server_joined() {
    let clock = fake_clock();
    let server = bind(&clock);
    let probe = server.drop_probe();
    assert!(
        !probe.listener_closed(),
        "a bound server still holds its listener"
    );
    assert!(
        !probe.accept_joined(),
        "a bound server has joined nothing yet"
    );
    drop(server);
    assert!(
        probe.listener_closed(),
        "the dropped server closed its listener"
    );
    assert!(
        probe.accept_joined(),
        "a never-served server still reads joined after its shutdown"
    );
}

/// Repeated shutdowns past a clean join stay joined: only the call
/// that took the handle records anything, and later calls that find
/// nothing leave the mark alone.
#[test]
fn repeated_shutdowns_after_a_clean_join_stay_joined() {
    let clock = fake_clock();
    let server = bind(&clock);
    server.serve_persistent(|_| {});
    let probe = server.drop_probe();
    assert!(
        server.shutdown(),
        "the first shutdown joins the accept thread"
    );
    assert!(probe.accept_joined(), "the joined accept stays joined");
    assert!(
        server.shutdown(),
        "a second shutdown past a joined accept still reports joined"
    );
    assert!(
        probe.accept_joined(),
        "the second shutdown leaves the join mark alone"
    );
    drop(server);
    assert!(
        probe.listener_closed(),
        "the dropped server closed its listener"
    );
    assert!(probe.accept_joined(), "the drop leaves the join mark alone");
}

/// Repeated shutdowns past a failed join stay unjoined: the call that
/// timed out records nothing, and no later call turns the failed join
/// into a joined one. Each blocking shutdown runs on a thread and its
/// result arrives through a deadline, so a hung shutdown fails the
/// test instead of hanging it.
#[test]
fn repeated_shutdowns_after_a_failed_join_stay_unjoined() {
    let clock = fake_clock();
    let server = bind(&clock);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    server.serve_persistent(move |_conn| {
        match started_tx.send(()) {
            Ok(()) | Err(_) => {}
        }
        // Stuck past shutdown's bound: shutdown must report it instead
        // of hanging. The test owns the release, so the handler ends
        // when the test is done with it.
        match release_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv()
        {
            Ok(()) | Err(_) => {}
        }
    });
    let mut client = connect(&server);
    client
        .write_all(b"GET /stuck HTTP/1.1\r\nHost: fake\r\n\r\n")
        .expect("the request writes");
    Deadline::after(OP).recv_or_fail(&started_rx, "the stuck handler");
    let probe = server.drop_probe();
    let limit = SHUTDOWN_WITHIN + Duration::from_secs(5);
    let (first_tx, first_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| match first_tx.send(server.shutdown()) {
            Ok(()) | Err(_) => {}
        });
        assert!(
            !Deadline::after(limit)
                .recv_or_fail(&first_rx, "the first shutdown reporting the stuck handler"),
            "shutdown reports the stuck handler instead of hanging"
        );
    });
    assert!(
        !probe.accept_joined(),
        "a timed-out join never reads joined"
    );
    let (second_tx, second_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| match second_tx.send(server.shutdown()) {
            Ok(()) | Err(_) => {}
        });
        Deadline::after(limit).recv_or_fail(
            &second_rx,
            "the second shutdown returning past the failed join",
        );
    });
    assert!(
        !probe.accept_joined(),
        "a later shutdown never turns a failed join into a joined one"
    );
    drop(server);
    assert!(
        !probe.accept_joined(),
        "the drop never turns a failed join into a joined one"
    );
    match release_tx.send(()) {
        Ok(()) | Err(_) => {}
    }
    drop(client);
}

/// A freed port is no proof of a dropped server: something else can
/// bind it at once, and a connect then succeeds. The probe reads the
/// dropped server's own state instead, so a reused port cannot flake
/// the verdict. The rebound listener lives to the test's end, so the
/// connect lands in its backlog; the connect runs on a thread under
/// the test's deadline with the socket's own bound kept as well.
#[test]
fn a_freed_port_is_not_proof_of_a_dropped_server() {
    let clock = fake_clock();
    let server = bind(&clock);
    let addr = server.addr();
    server.serve_persistent(|_| {});
    let probe = server.drop_probe();
    drop(server);
    assert!(
        probe.listener_closed(),
        "the dropped server closed its listener"
    );
    assert!(
        probe.accept_joined(),
        "the dropped server joined its accept thread"
    );
    let rebound = TcpListener::bind(addr).expect("the freed port rebinds after the drop");
    let (conn_tx, conn_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            TcpStream::connect_timeout(&addr, crate::MUST_SUCCEED_WITHIN)
                .expect("the rebound port accepting a connect");
            match conn_tx.send(()) {
                Ok(()) | Err(_) => {}
            }
        });
        Deadline::after(crate::MUST_SUCCEED_WITHIN + Duration::from_secs(5))
            .recv_or_fail(&conn_rx, "the rebound port accepting a connect");
    });
    drop(rebound);
}

/// Serving a stopped server spawns nothing: the slot keeps its
/// never-spawned shape, so the shutdown still reads as never served.
/// serve_persistent returns synchronously, so its return proves any
/// spawn already signalled; the pause's immediate silence then proves
/// none did, with no wall-clock wait. The serve runs on a worker so a
/// missing stopping check blocks in the pause and fails at the
/// completion deadline instead of hanging the test; the release drops
/// before any join.
#[test]
fn serve_after_stop_spawns_nothing() {
    /// Releases the spawn pause when the test unwinds past it: a serve
    /// stuck in the pause would otherwise outlive the failed test and
    /// hang the scope's join, turning a red assertion into a hung suite.
    struct ReleaseOnDrop {
        tx: Option<mpsc::Sender<()>>,
    }
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            if let Some(tx) = self.tx.take() {
                match tx.send(()) {
                    Ok(()) | Err(_) => {}
                }
            }
        }
    }
    let clock = fake_clock();
    let server = bind(&clock);
    let probe = server.drop_probe();
    assert!(server.shutdown(), "shutdown stops the unserved server");
    let (reached_rx, release_tx) = server.test_pause_after_spawn();
    let (serve_tx, serve_rx) = mpsc::channel();
    thread::scope(|scope| {
        // Dropped on unwind past any assert below, before the scope
        // ends: a serve stuck in the pause is released, so a red
        // assertion fails the test instead of hanging it.
        let _release = ReleaseOnDrop {
            tx: Some(release_tx.clone()),
        };
        scope.spawn(|| {
            server.serve_persistent(|_| {});
            match serve_tx.send(()) {
                Ok(()) | Err(_) => {}
            }
        });
        Deadline::after(OP).recv_or_fail(
            &serve_rx,
            "serving a stopped server returning without spawning",
        );
        assert!(
            reached_rx.try_recv().is_err(),
            "serving a stopped server spawns no accept thread"
        );
        assert!(
            probe.listener_closed(),
            "the stopped server closed its listener"
        );
        assert!(
            probe.accept_joined(),
            "serving a stopped server leaves the never-served join mark"
        );
    });
    drop(server);
    assert!(
        probe.accept_joined(),
        "the drop leaves the never-served join mark alone"
    );
}

/// A shutdown racing the spawn joins the thread: the pause holds the
/// serve between its spawn and its slot store, inside the accept lock,
/// so the racing shutdown blocks on the lock instead of seeing a
/// spawned thread as unspawned. A test-only signal immediately before
/// shutdown takes the contested lock proves it reached the acquisition
/// before the pause is released; its silence within a poll bound, with
/// no join published, then proves the wait. The release lets both end.
#[test]
fn shutdown_racing_spawn_joins_the_thread() {
    /// Releases the spawn pause when the test unwinds past it: the
    /// paused serve would otherwise outlive the failed test and hang
    /// the scope's join, turning a red assertion into a hung suite.
    struct ReleaseOnDrop {
        tx: Option<mpsc::Sender<()>>,
    }
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            if let Some(tx) = self.tx.take() {
                match tx.send(()) {
                    Ok(()) | Err(_) => {}
                }
            }
        }
    }
    let clock = fake_clock();
    let server = bind(&clock);
    let probe = server.drop_probe();
    let (reached_rx, release_tx) = server.test_pause_after_spawn();
    let (serve_tx, serve_rx) = mpsc::channel();
    let (down_tx, down_rx) = mpsc::channel();
    thread::scope(|scope| {
        // Dropped on unwind past any assert below, before the scope
        // ends: the paused serve is released, so a red assertion fails
        // the test instead of hanging it.
        let _release = ReleaseOnDrop {
            tx: Some(release_tx.clone()),
        };
        scope.spawn(|| {
            server.serve_persistent(|_| {});
            match serve_tx.send(()) {
                Ok(()) | Err(_) => {}
            }
        });
        Deadline::after(OP).recv_or_fail(&reached_rx, "the accept thread spawning");
        let at_accept_lock = server.test_shutdown_before_accept_lock();
        scope.spawn(|| match down_tx.send(server.shutdown()) {
            Ok(()) | Err(_) => {}
        });
        Deadline::after(OP).recv_or_fail(
            &at_accept_lock,
            "the racing shutdown reaching the contested accept lock",
        );
        assert!(
            Deadline::after(QUIET).recv(&down_rx).is_err(),
            "shutdown waits for the spawning serve instead of publishing a join"
        );
        assert!(
            !probe.accept_joined(),
            "no join is published while the spawn is paused"
        );
        match release_tx.send(()) {
            Ok(()) | Err(_) => {}
        }
        assert!(
            Deadline::after(OP).recv_or_fail(&down_rx, "the racing shutdown"),
            "shutdown joins the raced accept thread"
        );
        assert!(
            probe.accept_joined(),
            "the racing shutdown joined the accept thread"
        );
        Deadline::after(OP).recv_or_fail(&serve_rx, "the serve returning past the pause");
    });
}

/// Shutdown reports a stuck handler: a serving thread that never ends
/// fails shutdown instead of hanging the drop.
#[test]
fn shutdown_reports_a_stuck_handler() {
    let clock = fake_clock();
    let server = bind(&clock);
    let (started_tx, started_rx) = mpsc::channel();
    server.serve_persistent(move |_conn| {
        match started_tx.send(()) {
            Ok(()) | Err(_) => {}
        }
        // Stuck past shutdown's bound: shutdown must report it instead
        // of hanging. The wait's own deadline never fires inside the
        // test; the test process ends first.
        let (_stuck_tx, stuck_rx) = mpsc::channel::<()>();
        match Deadline::after(Duration::from_secs(60)).recv(&stuck_rx) {
            Ok(_) | Err(_) => {}
        }
    });
    let mut client = connect(&server);
    client
        .write_all(b"GET /stuck HTTP/1.1\r\nHost: fake\r\n\r\n")
        .expect("the request writes");
    Deadline::after(OP).recv_or_fail(&started_rx, "the stuck handler");
    assert!(
        !server.shutdown(),
        "shutdown reports the stuck handler instead of hanging"
    );
}

/// A socket-read error classifies distinctly: a would-block waits, an
/// interruption reads again at once, and anything else ends the read.
/// Each kind is constructed, so no socket flake decides the verdict.
#[test]
fn read_errors_classify_distinctly() {
    use std::io::ErrorKind;
    let error = |kind| std::io::Error::new(kind, "the test error");
    assert_eq!(
        classify(&error(ErrorKind::WouldBlock)),
        ReadErr::Wait,
        "a would-block waits"
    );
    assert_eq!(
        classify(&error(ErrorKind::Interrupted)),
        ReadErr::Retry,
        "an interruption reads again"
    );
    assert_eq!(
        classify(&error(ErrorKind::ConnectionReset)),
        ReadErr::End,
        "a reset ends the read"
    );
    assert_eq!(
        classify(&error(ErrorKind::BrokenPipe)),
        ReadErr::End,
        "a broken pipe ends the read"
    );
}

/// A wake generation pins both sides: the same generation is no wake,
/// any other generation wakes, wrapping included.
#[test]
fn a_wake_generation_pins_both_sides() {
    assert!(!woken(7, 7), "the same generation is no wake");
    assert!(woken(7, 8), "a newer generation wakes");
    assert!(woken(8, 7), "an older generation past a wrap wakes");
}

/// The consume gate pins each budget directly: an expired injected
/// budget panics naming the request and phase, a spent real guard
/// panics naming the guard, shutdown ends stopped, and a live budget
/// reads. No socket timing decides any case: the budgets are built,
/// never waited out.
#[test]
fn consume_checks_gate_buffered_bytes() {
    let clock = fake_clock();
    let server = bind(&clock);
    let client = connect(&server);
    let budget = |request: Duration, guard: Duration| {
        RequestBudget::start_on(&(clock.clone() as Arc<dyn Clock>), request, guard)
    };
    let conn = |request: Duration, guard: Duration| {
        Connection::new(
            client.try_clone().expect("the test clones its stream"),
            budget(request, guard),
            "the test request".to_owned(),
            Arc::clone(&server.shared),
        )
    };
    assert!(
        conn(Duration::from_secs(10), Duration::from_secs(45))
            .consume_check("to read the test request"),
        "a live budget consumes"
    );
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        conn(Duration::from_secs(0), Duration::from_secs(45))
            .consume_check("to read the test request");
    }));
    let message = failure_text(caught, "the expired consume check");
    assert!(
        message.contains("the test request"),
        "expiry names its request, got: {message}"
    );
    assert!(
        message.contains("to read the test request"),
        "expiry names its phase, got: {message}"
    );
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        conn(Duration::from_secs(10), Duration::from_secs(0))
            .consume_check("to read the test request");
    }));
    let message = failure_text(caught, "the spent guard's consume check");
    assert!(
        message.contains("real guard"),
        "a spent guard names itself, got: {message}"
    );
    assert!(
        message.contains("the test request"),
        "a spent guard names its request, got: {message}"
    );
    assert!(server.shutdown(), "shutdown stops the server");
    assert!(
        !conn(Duration::from_secs(10), Duration::from_secs(45))
            .consume_check("to read the test request"),
        "shutdown ends the consume stopped"
    );
}

/// Buffers one connection's whole request on a spawned thread, so the
/// test's deadline bounds the buffering and the request below is proven
/// fully held before the bound moves.
fn buffer_whole_request(server: &HttpServer, name: &str, request: &[u8]) -> Connection {
    let (conn_tx, conn_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server.accept_expected(name).expect("a client connects");
            loop {
                match conn.fill("to buffer the request") {
                    FillOutcome::Held(held) if held == request.len() => break,
                    FillOutcome::Held(_) => continue,
                    FillOutcome::Eof => panic!("the peer closed before buffering the request"),
                    FillOutcome::Stopped => panic!("shutdown ended the buffering"),
                }
            }
            conn_tx
                .send(conn)
                .expect("the test waits for the buffered read");
        });
        let mut client = TcpStream::connect(server.addr()).expect("the test connects");
        client.write_all(request).expect("the request writes");
        Deadline::after(OP)
            .recv(&conn_rx)
            .expect("the whole request buffers")
    })
}

/// An expired budget with a fully-buffered request still fails naming
/// the request and phase: already-held bytes never complete it.
#[test]
fn an_expired_budget_with_a_fully_buffered_request_still_fails_naming_it() {
    let clock = fake_clock();
    let server = bind(&clock);
    let request = b"GET /late HTTP/1.1\r\nHost: fake\r\n\r\n";
    let mut conn = buffer_whole_request(&server, "the buffered request", request);
    // The whole request is held; the bound expires with no further
    // client bytes, so only the consume gate decides the read.
    #[allow(
        clippy::disallowed_methods,
        reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
    )]
    clock.advance(Duration::from_secs(10));
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        request::read_request(&mut conn)
    }));
    let message = failure_text(caught, "the expired buffered read");
    assert!(
        message.contains("the buffered request"),
        "the failure names its request, got: {message}"
    );
    assert!(
        message.contains("to read the request line"),
        "the failure names its phase, got: {message}"
    );
}

/// A shutdown racing buffered bytes ends stopped, never a completed
/// response: the held request is not answered from the buffer.
#[test]
fn shutdown_racing_buffered_bytes_ends_stopped_never_complete() {
    let clock = fake_clock();
    let server = bind(&clock);
    let request = b"GET /held HTTP/1.1\r\nHost: fake\r\n\r\n";
    let mut conn = buffer_whole_request(&server, "the held request", request);
    assert!(server.shutdown(), "shutdown stops the server");
    assert!(
        matches!(
            request::read_request(&mut conn),
            request::RequestOutcome::Stopped
        ),
        "shutdown ends the buffered read stopped, never complete"
    );
}

/// An origin that accepts one connection and holds it until the peer
/// goes away: the tunnel's far end. Its thread ends on the peer's
/// close in every path below, so no join outlives its test.
fn hold_origin() -> (SocketAddr, mpsc::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("the origin binds");
    let addr = listener.local_addr().expect("the origin has an address");
    let (ended_tx, ended_rx) = mpsc::channel();
    thread::Builder::new()
        .name("fakes-http-test-origin".to_owned())
        .spawn(move || {
            let (mut stream, _) = listener.accept().expect("the tunnel reaches the origin");
            let mut rest = Vec::new();
            match std::io::Read::read_to_end(&mut stream, &mut rest) {
                Ok(_) | Err(_) => {}
            }
            match ended_tx.send(()) {
                Ok(()) | Err(_) => {}
            }
        })
        .expect("the origin serves");
    (addr, ended_rx)
}

/// Accepts one connection for a tunnel test: the client connects
/// first, so the accept finds it pending.
fn accept_for_tunnel(server: &HttpServer, name: &str) -> (Connection, TcpStream) {
    let client = TcpStream::connect(server.addr()).expect("the test connects");
    let conn = server.accept_expected(name).expect("a client connects");
    (conn, client)
}

/// A handshake the peer never reads fails within its bound naming the
/// target: the writer is past capacity, so the 200 parks on the injected
/// clock instead of completing. Sockets alone cannot guarantee
/// send-buffer exhaustion, so a finite writer forces the slow peer.
#[test]
fn a_handshake_the_peer_never_reads_fails_within_its_bound() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let target = "127.0.0.1:1".to_owned();
    let expected = target.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel();
    // The request takes the target's name, as the tunnel's own
    // handshake context does: expiry names the target.
    let (mut conn, _client) = accept_for_tunnel(&server, &target);
    let op = conn.op_budget();
    let (_stream, mut ctx) = conn.parts(&op);
    thread::scope(|scope| {
        // Past capacity with the drain held shut: every write parks, as
        // behind a peer that never reads.
        let mut client = TestWriter::finished(0, Arc::clone(&stop));
        let mut origin = TestWriter::finished(0, Arc::clone(&stop));
        scope.spawn(move || {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tunnel_handshake(&mut client, &mut origin, &[], &mut ctx)
            }));
            match done_tx.send(caught) {
                Ok(()) | Err(_) => {}
            }
        });
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the handshake's clock wait");
            if ack.phase == "to write the tunnel handshake" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the handshake parks on the clock before it moves"
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(10));
        let caught: thread::Result<bool> =
            Deadline::after(OP).recv_or_fail(&done_rx, "the stalled handshake");
        let message = failure_text(caught, "the stalled handshake");
        assert!(
            message.contains(&expected),
            "the failure names its target, got: {message}"
        );
        assert!(
            message.contains("to write the tunnel handshake"),
            "the failure names its phase, got: {message}"
        );
    });
}

/// A tunnel direction the peer never feeds fails within its bound
/// naming the target: both sides stay open and silent, so only the
/// copy's deadline decides it.
#[test]
fn a_stalled_tunnel_direction_fails_within_its_bound() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let (origin_addr, origin_ended) = hold_origin();
    let target = format!("{origin_addr}");
    let expected = target.clone();
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        let (conn, _client) = accept_for_tunnel(&server, "the tunnel request");
        let origin = TcpStream::connect(origin_addr).expect("the tunnel reaches the origin");
        scope.spawn(move || {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                conn.tunnel(&target, origin)
            }));
            match done_tx.send(caught) {
                Ok(()) | Err(_) => {}
            }
        });
        // Both sides stay open and silent: the client-to-origin copy
        // parks waiting for bytes that never come.
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the copy's clock wait");
            if ack.phase == "to copy the client to the origin" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the copy parks on the clock before it moves"
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(10));
        let caught: thread::Result<TunnelEnd> =
            Deadline::after(OP).recv_or_fail(&done_rx, "the stalled copy");
        let message = failure_text(caught, "the stalled copy");
        assert!(
            message.contains(&expected),
            "the failure names its target, got: {message}"
        );
        assert!(
            message.contains("to copy the client to the origin"),
            "the failure names its phase, got: {message}"
        );
        // The panic drops the tunnel's sockets and shutdown reaps the
        // other direction: the origin sees the end either way.
        assert!(server.shutdown(), "shutdown reaps the tunnel");
        Deadline::after(OP).recv_or_fail(&origin_ended, "the origin's end");
    });
}

/// A reader that never has bytes: every read would block, so a copy
/// from it parks on the injected clock instead of completing.
struct SilentReader;

impl std::io::Read for SilentReader {
    fn read(&mut self, _bytes: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "the silent origin holds",
        ))
    }
}

/// A stalled origin read fails within its bound naming the target: the
/// origin never sends, so only the copy's deadline decides it. A
/// blocking origin read would hang here with no acknowledgment at all.
#[test]
fn a_stalled_origin_read_fails_within_its_bound() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let target = "127.0.0.1:1".to_owned();
    let expected = target.clone();
    // The request takes the target's name, as the tunnel's own copy
    // context does: expiry names the target.
    let (mut conn, _client) = accept_for_tunnel(&server, &target);
    let op = conn.op_budget();
    let (_stream, mut ctx) = conn.parts(&op);
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        let mut src = SilentReader;
        let mut dst = Vec::new();
        scope.spawn(move || {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tunnel_copy(
                    &mut src,
                    &mut dst,
                    "to copy the origin to the client",
                    &mut ctx,
                );
            }));
            match done_tx.send(caught) {
                Ok(()) | Err(_) => {}
            }
        });
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the origin copy's clock wait");
            if ack.phase == "to copy the origin to the client" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the origin copy parks on the clock before it moves"
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(10));
        let caught: thread::Result<()> =
            Deadline::after(OP).recv_or_fail(&done_rx, "the stalled origin read");
        let message = failure_text(caught, "the stalled origin read");
        assert!(
            message.contains(&expected),
            "the failure names its target, got: {message}"
        );
        assert!(
            message.contains("to copy the origin to the client"),
            "the failure names its phase, got: {message}"
        );
    });
}

/// A stalled origin write fails within its bound naming the target: the
/// origin never reads, so the copy's pump parks on the injected clock
/// instead of completing. A finite writer forces the slow peer, since
/// sockets alone cannot guarantee send-buffer exhaustion.
#[test]
fn a_stalled_origin_write_fails_within_its_bound() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let target = "127.0.0.1:1".to_owned();
    let expected = target.clone();
    let (mut conn, _client) = accept_for_tunnel(&server, &target);
    let op = conn.op_budget();
    let (_stream, mut ctx) = conn.parts(&op);
    let stop = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        // Past capacity with the drain held shut: every write parks, as
        // behind an origin that never reads.
        let mut src = std::io::Cursor::new(vec![b'x'; 8]);
        let mut dst = TestWriter::finished(0, Arc::clone(&stop));
        scope.spawn(move || {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tunnel_copy(
                    &mut src,
                    &mut dst,
                    "to copy the client to the origin",
                    &mut ctx,
                );
            }));
            match done_tx.send(caught) {
                Ok(()) | Err(_) => {}
            }
        });
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the origin write's clock wait");
            if ack.phase == "to copy the client to the origin" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the origin write parks on the clock before it moves"
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(10));
        let caught: thread::Result<()> =
            Deadline::after(OP).recv_or_fail(&done_rx, "the stalled origin write");
        let message = failure_text(caught, "the stalled origin write");
        assert!(
            message.contains(&expected),
            "the failure names its target, got: {message}"
        );
        assert!(
            message.contains("to copy the client to the origin"),
            "the failure names its phase, got: {message}"
        );
    });
}

/// Shutdown during a stalled origin read stops it: the parked copy
/// returns instead of failing past its bound.
#[test]
fn shutdown_during_a_stalled_origin_read_stops_it() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let target = "127.0.0.1:1".to_owned();
    let (mut conn, _client) = accept_for_tunnel(&server, &target);
    let op = conn.op_budget();
    let (_stream, mut ctx) = conn.parts(&op);
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        let mut src = SilentReader;
        let mut dst = Vec::new();
        scope.spawn(move || {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tunnel_copy(
                    &mut src,
                    &mut dst,
                    "to copy the origin to the client",
                    &mut ctx,
                );
            }));
            match done_tx.send(caught.is_ok()) {
                Ok(()) | Err(_) => {}
            }
        });
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the origin copy's clock wait");
            if ack.phase == "to copy the origin to the client" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the origin copy parks on the clock before it moves"
        );
        assert!(server.shutdown(), "shutdown ends the stalled origin read");
        assert!(
            Deadline::after(OP).recv_or_fail(&done_rx, "the stopped origin read"),
            "shutdown stops the stalled origin read instead of failing it"
        );
    });
}

/// Shutdown during a stalled origin write stops it: the parked pump
/// returns instead of failing past its bound.
#[test]
fn shutdown_during_a_stalled_origin_write_stops_it() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let target = "127.0.0.1:1".to_owned();
    let (mut conn, _client) = accept_for_tunnel(&server, &target);
    let op = conn.op_budget();
    let (_stream, mut ctx) = conn.parts(&op);
    let stop = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        let mut src = std::io::Cursor::new(vec![b'x'; 8]);
        let mut dst = TestWriter::finished(0, Arc::clone(&stop));
        scope.spawn(move || {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tunnel_copy(
                    &mut src,
                    &mut dst,
                    "to copy the client to the origin",
                    &mut ctx,
                );
            }));
            match done_tx.send(caught.is_ok()) {
                Ok(()) | Err(_) => {}
            }
        });
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the origin write's clock wait");
            if ack.phase == "to copy the client to the origin" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the origin write parks on the clock before it moves"
        );
        assert!(server.shutdown(), "shutdown ends the stalled origin write");
        assert!(
            Deadline::after(OP).recv_or_fail(&done_rx, "the stopped origin write"),
            "shutdown stops the stalled origin write instead of failing it"
        );
    });
}

/// A silent origin parks on the injected clock through a real tunnel:
/// the origin-to-client copy waits boundedly instead of blocking on
/// the origin socket, and shutdown ends the tunnel stopped. A blocking
/// origin read would hang here with no acknowledgment at all.
#[test]
fn a_silent_origin_parks_on_the_injected_clock() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let (origin_addr, origin_ended) = hold_origin();
    let target = format!("{origin_addr}");
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        let (conn, _client) = accept_for_tunnel(&server, "the tunnel request");
        let origin = TcpStream::connect(origin_addr).expect("the tunnel reaches the origin");
        scope.spawn(move || {
            let end = conn.tunnel(&target, origin);
            match done_tx.send(end) {
                Ok(()) | Err(_) => {}
            }
        });
        // Both sides stay open and silent: the origin-to-client copy
        // parks waiting for bytes that never come.
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the origin copy's clock wait");
            if ack.phase == "to copy the origin to the client" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the origin copy parks on the clock before it moves"
        );
        assert!(server.shutdown(), "shutdown ends the silent tunnel");
        assert_eq!(
            Deadline::after(OP).recv_or_fail(&done_rx, "the stopped tunnel"),
            TunnelEnd::Stopped,
            "shutdown ends the tunnel stopped"
        );
        Deadline::after(OP).recv_or_fail(&origin_ended, "the origin's end");
    });
}

/// Shutdown during the tunnel handshake stops it: the parked 200 never
/// sends, and no copy starts.
#[test]
fn shutdown_during_the_tunnel_handshake_stops_it() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let target = "127.0.0.1:1".to_owned();
    let stop = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel();
    let (mut conn, _client) = accept_for_tunnel(&server, &target);
    let op = conn.op_budget();
    let (_stream, mut ctx) = conn.parts(&op);
    thread::scope(|scope| {
        let mut client = TestWriter::finished(0, Arc::clone(&stop));
        let mut origin = TestWriter::finished(0, Arc::clone(&stop));
        scope.spawn(move || {
            let held = tunnel_handshake(&mut client, &mut origin, &[], &mut ctx);
            match done_tx.send(held) {
                Ok(()) | Err(_) => {}
            }
        });
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the handshake's clock wait");
            if ack.phase == "to write the tunnel handshake" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the handshake parks on the clock before it moves"
        );
        assert!(server.shutdown(), "shutdown stops the handshake");
        assert!(
            !Deadline::after(OP).recv_or_fail(&done_rx, "the stopped handshake"),
            "shutdown ends the handshake stopped"
        );
    });
}

/// Shutdown before tunnel registration stops it without copying: the
/// handshake already succeeded, but the atomic check sees stopping and
/// no copy starts, so nothing is left for the drain to miss.
#[test]
fn shutdown_before_tunnel_registration_stops_it_without_copying() {
    let clock = fake_clock();
    let server = bind(&clock);
    let (origin_addr, origin_ended) = hold_origin();
    let target = format!("{origin_addr}");
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        let (conn, _client) = accept_for_tunnel(&server, "the tunnel request");
        let origin = TcpStream::connect(origin_addr).expect("the tunnel reaches the origin");
        assert!(server.shutdown(), "shutdown stops the server first");
        scope.spawn(move || {
            let end = conn.tunnel(&target, origin);
            match done_tx.send(end) {
                Ok(()) | Err(_) => {}
            }
        });
        assert_eq!(
            Deadline::after(OP).recv_or_fail(&done_rx, "the stopped tunnel"),
            TunnelEnd::Stopped,
            "shutdown ends the tunnel stopped before any copy"
        );
        assert!(
            lock(&server.shared.tunnels).live.is_empty(),
            "no tunnel registered past shutdown"
        );
        Deadline::after(OP).recv_or_fail(&origin_ended, "the origin's end");
    });
}

/// One connected socket pair, held open: break-socket dupes for the
/// registration tests below.
fn socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("the pair binds");
    let addr = listener.local_addr().expect("the pair has an address");
    let probe = TcpStream::connect(addr).expect("the pair connects");
    let (held, _) = listener.accept().expect("the pair accepts");
    (probe, held)
}

/// Registration while live claims an id with its break sockets: the
/// drain below would break a copy parked on them.
#[test]
fn registration_while_live_claims_an_id() {
    let clock = fake_clock();
    let server = bind(&clock);
    let (probe, held) = socket_pair();
    assert_eq!(
        server.shared.register_tunnel(&probe, &held),
        Some(0),
        "the first live registration claims id zero"
    );
    assert_eq!(
        lock(&server.shared.tunnels).live.len(),
        1,
        "the claim registers its break sockets"
    );
}

/// Registration past shutdown claims nothing: the atomic check sees
/// stopping under the same lock as the push, so no copy starts and the
/// drain has nothing to miss. Removing the check registers instead.
#[test]
fn registration_past_shutdown_claims_nothing() {
    let clock = fake_clock();
    let server = bind(&clock);
    let (probe, held) = socket_pair();
    assert!(server.shutdown(), "shutdown stops the server");
    assert_eq!(
        server.shared.register_tunnel(&probe, &held),
        None,
        "no tunnel registers past shutdown"
    );
    assert!(
        lock(&server.shared.tunnels).live.is_empty(),
        "the refused claim registers nothing"
    );
}

/// A finished tunnel removes only its own registry entry: live neighbours
/// below and above its id stay for shutdown's drain. Flipping the retain
/// to `==` keeps only the finished id and drops both neighbours.
#[test]
fn a_finished_tunnel_removes_only_its_own_registry_entry() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    // A live neighbour below the tunnel's id: the drain must still
    // break it after the tunnel ends.
    let (probe_before, held_before) = socket_pair();
    let before = server
        .shared
        .register_tunnel(&probe_before, &held_before)
        .expect("a live neighbour registers");
    let (origin_addr, origin_ended) = hold_origin();
    let target = format!("{origin_addr}");
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        let (conn, client) = accept_for_tunnel(&server, "the tunnel request");
        let origin = TcpStream::connect(origin_addr).expect("the tunnel reaches the origin");
        scope.spawn(move || {
            let end = conn.tunnel(&target, origin);
            match done_tx.send(end) {
                Ok(()) | Err(_) => {}
            }
        });
        // The client-to-origin copy parks only past registration, so
        // its wait proves the tunnel claimed its id.
        let deadline = Deadline::after(OP);
        let ack = loop {
            let ack = next_acknowledgment(&deadline, &acks, "the copy's clock wait");
            if ack.phase == "to copy the client to the origin" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the copy parks on the clock before it moves"
        );
        // A live neighbour above the tunnel's id, claimed while the
        // tunnel runs.
        let (probe_after, held_after) = socket_pair();
        let after = server
            .shared
            .register_tunnel(&probe_after, &held_after)
            .expect("a later neighbour registers");
        // The client's close ends the client-to-origin copy first: the
        // origin never sends nor closes, so the origin copy cannot win.
        drop(client);
        assert_eq!(
            Deadline::after(OP).recv_or_fail(&done_rx, "the finished tunnel"),
            TunnelEnd::ClientClosed,
            "the client's close ends the tunnel first"
        );
        let live: Vec<u64> = lock(&server.shared.tunnels)
            .live
            .iter()
            .map(|(id, _, _)| *id)
            .collect();
        assert_eq!(
            live,
            vec![before, after],
            "only the finished tunnel leaves the registry"
        );
        assert!(server.shutdown(), "shutdown reaps the neighbours");
        Deadline::after(OP).recv_or_fail(&origin_ended, "the origin's end");
    });
}
