//! Tests of the shared HTTP/1.1 core's ownership, budgets and waits,
//! through its own seam (`http_server.rs`).

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, PoisonError, mpsc};
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
    thread::scope(|scope| {
        scope.spawn(|| {
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
    });

    // One tick past the bound expires too: a single advance past it.
    let clock = fake_clock();
    let server = HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds())
        .expect("the core binds");
    let acks = server.test_acknowledgments();
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
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
        let read = loop {
            let ack = next_acknowledgment(&acks, "the read's clock wait");
            if ack.phase == "to read the silent request" {
                break ack;
            }
        };
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
        scope.spawn(move || {
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
    answer: impl FnOnce(&mut Connection) + Send + 'static,
) -> (HttpServer, SocketAddr) {
    let clock = FakeClock::new();
    let server = HttpServer::bind(clock as Arc<dyn Clock>, ten_seconds()).expect("the core binds");
    let addr = server.addr();
    let serving = server.clone();
    thread::spawn(move || {
        let mut conn = serving
            .accept_expected("the test request")
            .expect("a client connects");
        let request = match request::read_request(&mut conn) {
            request::RequestOutcome::Complete(request) => request,
            other => panic!("the request completes, not {other:?}"),
        };
        match recorded.send(request) {
            Ok(()) | Err(_) => {}
        }
        answer(&mut conn);
    });
    (server, addr)
}

#[test]
fn full_reply_preserves_scripted_headers_and_bytes() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let head = b"HTTP/1.1 200 Fake\r\ncontent-type: text/event-stream\r\ncontent-length: 2\r\nconnection: close\r\n\r\n";
    let (server, addr) = serve_once(recorded_tx, move |conn| {
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

#[test]
fn chunked_pieces_have_exact_framing() {
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let head = b"HTTP/1.1 200 Fake\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
    let (server, addr) = serve_once(recorded_tx, move |conn| {
        assert_eq!(
            conn.reply_stream(head, &[b"ok".to_vec(), b"bye".to_vec()], Framing::Chunked, None),
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
    let (server, addr) = serve_once(recorded_tx, move |conn| {
        assert_eq!(
            conn.reply_stream(head, &[events.to_vec()], Framing::Raw, None),
            ReplyEnd::Done,
            "the stream sends"
        );
    });
    let raw = exchange(addr, b"POST /mcp HTTP/1.1\r\nHost: fake\r\nContent-Length: 0\r\n\r\n");
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
    let (server, addr) = serve_once(recorded_tx, move |conn| {
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
    let (server, addr) = serve_once(recorded_tx, move |conn| {
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
    let raw = exchange(addr, b"POST /v1 HTTP/1.1\r\nHost: fake\r\nContent-Length: 0\r\n\r\n");
    assert!(raw.is_empty(), "a dropped reply sends nothing");
    let request = Deadline::after(OP).recv_or_fail(&recorded_rx, "the recorded request");
    assert_eq!(request.target, "/v1");
    assert!(server.shutdown(), "shutdown reaps the serving threads");
}

/// A writer with a finite capacity: past it, every write would block.
struct TestWriter {
    capacity: usize,
    written: Vec<u8>,
}

impl TestWriter {
    fn finished(capacity: usize) -> Self {
        Self {
            capacity,
            written: Vec::new(),
        }
    }
}

impl std::io::Write for TestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
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
    let server = HttpServer::bind(clock.clone() as Arc<dyn Clock>, ten_seconds())
        .expect("the core binds");
    let acks = server.test_acknowledgments();
    let addr = server.addr();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut conn = server
                .accept_expected("the held request")
                .expect("a client connects");
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut writer = TestWriter::finished(10);
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
        let ack = loop {
            let ack = Deadline::after(OP).recv_or_fail(&acks, "the write's clock wait");
            if ack.phase == "to write the held response" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, OP),
            "the write parks on the clock before it moves"
        );
        clock.advance(Duration::from_secs(10) - Duration::from_nanos(1));
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
                    other => panic!("the CONNECT head completes, not {other:?}"),
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
                _ => return,
            };
            drop(request);
        if conn.pause(&pause, &conn.op_budget(), "to pause the test response")
            == PauseEnd::Stopped
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
