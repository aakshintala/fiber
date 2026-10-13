//! Tests of the shared HTTP/1.1 core's ownership, budgets and waits,
//! through its own seam (`http_server.rs`).

use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;

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
fn next_acknowledgment(rx: &mpsc::Receiver<WaitAck>, what: &str) -> WaitAck {
    Deadline::after(OP).recv_or_fail(rx, what)
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
    let server = HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds())
        .expect("the core binds");
    let acks = server.test_acknowledgments();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            server.accept_expected("the missing request")
        }));
        done_tx.send(caught).expect("the test waits");
    });
    let ack = next_acknowledgment(&acks, "the accept's clock wait");
    assert!(
        clock.await_thread_parked(ack.thread, ack.until, OP),
        "the accept parks on the clock before it moves"
    );
    clock.advance(Duration::from_secs(10) - Duration::from_nanos(1));
    assert!(
        Deadline::after(QUIET).recv(&done_rx).is_err(),
        "one tick before the bound the accept is still waiting"
    );
    clock.advance(Duration::from_nanos(1));
    let caught = Deadline::after(OP)
        .recv(&done_rx)
        .expect("the bound ends the accept");
    let message = failure_text(caught, "the missing request");
    assert!(
        message.contains("the missing request"),
        "the failure names its request, got: {message}"
    );

    // One tick past the bound expires too: a single advance past it.
    let clock = fake_clock();
    let server = HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds())
        .expect("the core binds");
    let acks = server.test_acknowledgments();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            server.accept_expected("the late request")
        }));
        done_tx.send(caught).expect("the test waits");
    });
    let ack = next_acknowledgment(&acks, "the accept's clock wait");
    assert!(
        clock.await_thread_parked(ack.thread, ack.until, OP),
        "the accept parks on the clock before it moves"
    );
    clock.advance(Duration::from_secs(10) + Duration::from_nanos(1));
    let caught = Deadline::after(OP)
        .recv(&done_rx)
        .expect("past the bound the accept fails");
    let message = failure_text(caught, "the late request");
    assert!(
        message.contains("the late request"),
        "the failure names its request, got: {message}"
    );
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
    assert!(
        await_windows(&server, 1, OP),
        "the first window started"
    );
    clock.advance(Duration::from_secs(5));
    assert!(
        await_idle(&server, 1, OP),
        "the first idle window renewed"
    );
    assert!(
        await_windows(&server, 2, OP),
        "the second window started"
    );
    clock.advance(Duration::from_secs(5));
    assert!(
        await_idle(&server, 2, OP),
        "the second idle window renewed"
    );

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
        let ack = next_acknowledgment(&acks, "the accept's clock wait");
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, OP),
            "the accept parks on the clock before it moves"
        );
        // Three seconds pass with no client: the budget is running already.
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
        clock.advance(Duration::from_secs(7) - Duration::from_nanos(1));
        assert_eq!(
            conn.read_byte("to read the second byte"),
            ReadOutcome::Byte(b'b'),
            "one tick before the bound the second byte reads"
        );
        // Past the bound the next read names the request.
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
    let accepting = server.clone();
    thread::spawn(move || {
        let mut conn = accepting
            .accept_expected("the silent request")
            .expect("a client connects");
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            conn.read_byte("to read the silent request");
        }));
        done_tx.send(caught).expect("the test waits");
    });
    // The client connects and sends nothing: no byte wakes the read.
    let _client = connect(&server);
    // Two acknowledgments: the accept's wait, then the read's wait after
    // the accept takes the silent client.
    let accept = next_acknowledgment(&acks, "the accept's clock wait");
    assert!(
        clock.await_thread_parked(accept.thread, accept.until, OP),
        "the accept parks on the clock before it moves"
    );
    let read = next_acknowledgment(&acks, "the read's clock wait");
    assert!(
        clock.await_thread_parked(read.thread, read.until, OP),
        "the read parks on the clock before it moves"
    );
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
    thread::spawn(move || {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            server.accept_expected("the raced request")
        }));
        done_tx.send(caught).expect("the test waits");
    });
    let first = next_acknowledgment(&acks, "the accept's clock wait");
    assert!(
        clock.await_thread_parked(first.thread, first.until, OP),
        "the accept parks on the clock before it moves"
    );
    // Arm the check-to-park pause, then wake the waiter once: it
    // re-checks past the advance and pauses before its next park.
    gate.arm();
    clock.advance(Duration::from_secs(3));
    let second = next_acknowledgment(&acks, "the accept's re-check");
    assert_eq!(
        second.seq,
        first.seq + 1,
        "only the announced sequence advances the clock"
    );
    // The deciding advance lands while the waiter is held between its
    // check and its park.
    let (advanced_tx, advanced_rx) = mpsc::channel();
    let advancing = clock.clone();
    thread::spawn(move || {
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
}
