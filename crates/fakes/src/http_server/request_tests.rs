//! Tests of the bounded HTTP request reader through its own seam
//! (`http_server/request.rs`).

use std::io::Write;
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use super::super::*;
use super::*;
use crate::clock::FakeClock;
use crate::deadline::Deadline;

/// How long one test operation may take on the wall clock.
const OP: Duration = Duration::from_secs(10);

/// Small bounds with roomy caps: each cap test narrows one.
fn bounds() -> Limits {
    Limits {
        request: Duration::from_secs(10),
        ..Limits::default()
    }
}

fn fake_clock() -> Arc<FakeClock> {
    FakeClock::new()
}

fn bind(clock: &Arc<FakeClock>, limits: Limits) -> HttpServer {
    HttpServer::bind(clock.clone(), limits).expect("the core binds")
}

/// Reads one connection's request on a fresh server: the caller reads
/// on its own thread while a scoped client delivers the bytes, so no
/// thread outlives the read and no clone's drop can stop the server.
/// Every wait has the operation deadline, never a bare join.
fn read_once(limits: Limits, bytes: &[u8]) -> RequestOutcome {
    let clock = fake_clock();
    let server = bind(&clock, limits);
    let addr = server.addr();
    thread::scope(|scope| {
        scope.spawn(move || {
            let mut client = TcpStream::connect(addr).expect("the test connects");
            client.write_all(bytes).expect("the request writes");
            // The write side closes behind the request, so a short
            // framing ends the read at once instead of outliving the bound.
            match client.shutdown(Shutdown::Write) {
                Ok(()) | Err(_) => {}
            }
        });
        let mut conn = server
            .accept_expected("the test request")
            .expect("a client connects");
        read_request(&mut conn)
    })
}

/// A complete request's parts, failing the test on any other outcome.
fn complete(outcome: RequestOutcome) -> ParsedRequest {
    match outcome {
        RequestOutcome::Complete(request) => {
            assert_eq!(request.malformed, None);
            request
        }
        RequestOutcome::Eof => panic!("the peer closed instead of completing the request"),
        RequestOutcome::Stopped => panic!("shutdown ended the request read"),
        RequestOutcome::Invalid(why) => panic!("the request was invalid: {why}"),
    }
}

#[test]
fn fixed_length_and_missing_length_record_exact_bytes() {
    let request = complete(read_once(
        bounds(),
        b"POST /v1/messages HTTP/1.1\r\nHost: fake\r\nContent-Length: 3\r\n\r\nabc",
    ));
    assert_eq!(request.method, "POST");
    assert_eq!(request.target, "/v1/messages");
    assert_eq!(
        request.headers,
        [
            ("host".to_owned(), "fake".to_owned()),
            ("content-length".to_owned(), "3".to_owned()),
        ]
    );
    assert_eq!(request.body, b"abc");

    let request = complete(read_once(
        bounds(),
        b"GET /health HTTP/1.1\r\nHost: fake\r\n\r\n",
    ));
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, "/health");
    assert!(request.body.is_empty(), "no length means no body");
}

#[test]
fn chunked_extensions_and_trailers_decode_in_order() {
    let request = complete(read_once(
        bounds(),
        b"POST /v1 HTTP/1.1\r\nHost: fake\r\nTransfer-Encoding: chunked\r\n\r\n\
          5;ext=1\r\nhello\r\n3\r\nbye\r\n0\r\nx-first: 1\r\nx-second: 2\r\n\r\n",
    ));
    assert_eq!(request.body, b"hellobye");
}

#[test]
fn malformed_chunks_keep_the_decoded_prefix() {
    // A chunk size that is not hex: the prefix decodes, the rest stops.
    let request = match read_once(
        bounds(),
        b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\nzz\r\n",
    ) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => panic!("a bad hex size is malformed ended at EOF"),
        RequestOutcome::Stopped => panic!("a bad hex size is malformed stopped"),
        RequestOutcome::Invalid(why) => panic!("a bad hex size is malformed invalid: {why}"),
    };
    assert_eq!(request.body, b"abc");
    assert_eq!(request.malformed, Some("a chunk size is not a hex number"));

    // Data not followed by CRLF: the chunk's bytes are kept.
    let request = match read_once(
        bounds(),
        b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabcX\r\n0\r\n\r\n",
    ) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => panic!("a missing CRLF is malformed ended at EOF"),
        RequestOutcome::Stopped => panic!("a missing CRLF is malformed stopped"),
        RequestOutcome::Invalid(why) => panic!("a missing CRLF is malformed invalid: {why}"),
    };
    assert_eq!(request.body, b"abc");
    assert_eq!(
        request.malformed,
        Some("a chunk's data is not followed by CRLF")
    );

    // A close mid-body truncates the framing, keeping what decoded.
    let request = match read_once(
        bounds(),
        b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab",
    ) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => panic!("a truncated body is malformed ended at EOF"),
        RequestOutcome::Stopped => panic!("a truncated body is malformed stopped"),
        RequestOutcome::Invalid(why) => panic!("a truncated body is malformed invalid: {why}"),
    };
    assert_eq!(request.body, b"ab");
    assert_eq!(
        request.malformed,
        Some("the chunked body ends before its framing does")
    );

    // A bare LF is not chunk framing.
    let request = match read_once(
        bounds(),
        b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\nabc\r\n0\r\n\r\n",
    ) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => panic!("a bare LF is malformed ended at EOF"),
        RequestOutcome::Stopped => panic!("a bare LF is malformed stopped"),
        RequestOutcome::Invalid(why) => panic!("a bare LF is malformed invalid: {why}"),
    };
    assert_eq!(request.body, b"");
    assert_eq!(
        request.malformed,
        Some("a chunked framing line does not end in CRLF")
    );
}

/// One cap's exact boundary under `limits`: `under` and `at` complete,
/// `over` refuses.
fn check_cap(limits: Limits, under: &[u8], at: &[u8], over: &[u8]) {
    assert!(
        matches!(read_once(limits, under), RequestOutcome::Complete(_)),
        "one under the cap is admitted"
    );
    assert!(
        matches!(read_once(limits, at), RequestOutcome::Complete(_)),
        "exactly the cap is admitted"
    );
    assert!(
        matches!(read_once(limits, over), RequestOutcome::Invalid(_)),
        "one over the cap is refused"
    );
}

#[test]
fn each_request_cap_admits_the_boundary_and_rejects_one_more() {
    // The request line at 15, 16 and 17 bytes with its terminator.
    let limits = Limits {
        line: 16,
        ..bounds()
    };
    let line = |pad: usize| format!("G /{} \r\n\r\n", "x".repeat(pad)).into_bytes();
    check_cap(limits, &line(9), &line(10), &line(11));

    // One header line at 15, 16 and 17 bytes with its terminator.
    let limits = Limits {
        frame: 16,
        ..bounds()
    };
    let header = |pad: usize| format!("G / H\r\na: {}\r\n\r\n", "y".repeat(pad)).into_bytes();
    check_cap(limits, &header(10), &header(11), &header(12));

    // A fixed body at 15, 16 and 17 declared bytes.
    let limits = Limits {
        body: 16,
        ..bounds()
    };
    let fixed = |len: usize| {
        format!(
            "POST /v1 HTTP/1.1\r\nContent-Length: {len}\r\n\r\n{}",
            "b".repeat(len)
        )
        .into_bytes()
    };
    check_cap(limits, &fixed(15), &fixed(16), &fixed(17));
    // Zero bytes are a valid empty body.
    assert!(
        matches!(
            read_once(limits, b"POST /v1 HTTP/1.1\r\nContent-Length: 0\r\n\r\n"),
            RequestOutcome::Complete(_)
        ),
        "zero bytes are admitted"
    );

    // A chunk-size line at 31, 32 and 33 bytes: overlong framing is
    // malformed, keeping the decoded prefix, not invalid.
    let limits = Limits {
        frame: 32,
        ..bounds()
    };
    let size = |pad: usize| {
        format!(
            "POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n1;{}\r\n1\r\n0\r\n\r\n",
            "x".repeat(pad)
        )
        .into_bytes()
    };
    assert!(
        matches!(read_once(limits, &size(27)), RequestOutcome::Complete(_)),
        "a 31-byte size line is admitted"
    );
    assert!(
        matches!(read_once(limits, &size(28)), RequestOutcome::Complete(_)),
        "a 32-byte size line is admitted"
    );
    let crossed = match read_once(limits, &size(29)) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => panic!("a 33-byte size line is malformed ended at EOF"),
        RequestOutcome::Stopped => panic!("a 33-byte size line is malformed stopped"),
        RequestOutcome::Invalid(why) => panic!("a 33-byte size line is malformed invalid: {why}"),
    };
    assert_eq!(crossed.malformed, Some("a chunk line exceeds its cap"));

    // Header count: one, two, then three headers with a cap of two.
    let limits = Limits {
        header_count: 2,
        ..bounds()
    };
    let heads = |count: usize| {
        let mut request = b"G / H\r\n".to_vec();
        for n in 0..count {
            request.extend_from_slice(format!("a{n}: b\r\n").as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        request
    };
    assert!(
        matches!(read_once(limits, &heads(1)), RequestOutcome::Complete(_)),
        "one header is admitted"
    );
    assert!(
        matches!(read_once(limits, &heads(2)), RequestOutcome::Complete(_)),
        "exactly the count is admitted"
    );
    assert!(
        matches!(read_once(limits, &heads(3)), RequestOutcome::Invalid(_)),
        "one header past the count is refused"
    );
    // No headers at all is admitted.
    assert!(
        matches!(
            read_once(limits, b"G / H\r\n\r\n"),
            RequestOutcome::Complete(_)
        ),
        "zero headers are admitted"
    );
}

#[test]
fn aggregate_headers_chunks_and_trailers_share_their_caps() {
    // Many small headers whose sum crosses 32 aggregate bytes: five
    // 6-byte lines admit 30, the sixth crosses.
    let limits = Limits {
        headers: 32,
        ..bounds()
    };
    let heads = |count: usize| {
        let mut request = b"G / H\r\n".to_vec();
        for n in 0..count {
            request.extend_from_slice(format!("h{n}:v\r\n").as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        request
    };
    assert!(
        matches!(read_once(limits, &heads(5)), RequestOutcome::Complete(_)),
        "30 aggregate bytes are admitted"
    );
    assert!(
        matches!(read_once(limits, &heads(6)), RequestOutcome::Invalid(_)),
        "36 aggregate bytes cross the 32-byte cap"
    );

    // Many small chunks whose decoded sum crosses 16 body bytes.
    let limits = Limits {
        body: 16,
        ..bounds()
    };
    let chunks = |count: usize| {
        let mut request = b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for _ in 0..count {
            request.extend_from_slice(b"4\r\n1234\r\n");
        }
        request.extend_from_slice(b"0\r\n\r\n");
        request
    };
    assert!(
        matches!(read_once(limits, &chunks(4)), RequestOutcome::Complete(_)),
        "16 decoded bytes are admitted"
    );
    let crossed = match read_once(limits, &chunks(5)) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => panic!("a crossed body cap is malformed ended at EOF"),
        RequestOutcome::Stopped => panic!("a crossed body cap is malformed stopped"),
        RequestOutcome::Invalid(why) => panic!("a crossed body cap is malformed invalid: {why}"),
    };
    assert_eq!(crossed.body, b"1234123412341234");
    assert_eq!(crossed.malformed, Some("the body exceeds its cap"));

    // Many small trailers whose sum crosses 32 aggregate bytes.
    let limits = Limits {
        trailers: 32,
        ..bounds()
    };
    let trailers = |count: usize| {
        let mut request = b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n".to_vec();
        for n in 0..count {
            request.extend_from_slice(format!("t{n}:v\r\n").as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        request
    };
    assert!(
        matches!(read_once(limits, &trailers(5)), RequestOutcome::Complete(_)),
        "30 trailer bytes are admitted"
    );
    let crossed = match read_once(limits, &trailers(6)) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => panic!("crossed trailer bytes are malformed ended at EOF"),
        RequestOutcome::Stopped => panic!("crossed trailer bytes are malformed stopped"),
        RequestOutcome::Invalid(why) => {
            panic!("crossed trailer bytes are malformed invalid: {why}")
        }
    };
    assert_eq!(crossed.malformed, Some("the trailers exceed their cap"));
}

#[test]
fn huge_declared_lengths_fail_before_allocation() {
    // A decimal length past `usize`: no number, no allocation.
    assert!(
        matches!(
            read_once(
                bounds(),
                b"POST /v1 HTTP/1.1\r\nContent-Length: 99999999999999999999999\r\n\r\n",
            ),
            RequestOutcome::Invalid(_)
        ),
        "an overflowing decimal length is invalid"
    );
    // A valid but enormous decimal length: refused before its allocation.
    // The client sends nothing more, so reading for it would hang the
    // test past its deadline instead of refusing at once.
    assert!(
        matches!(
            read_once(
                bounds(),
                b"POST /v1 HTTP/1.1\r\nContent-Length: 18446744073709551615\r\n\r\n",
            ),
            RequestOutcome::Invalid(_)
        ),
        "an enormous declared length is refused before allocation"
    );
    // A hex chunk size past `u64`: not a number.
    let crossed = match read_once(
        bounds(),
        b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFFFFFFFFFFFFFFF\r\n",
    ) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => panic!("an overflowing hex size is malformed ended at EOF"),
        RequestOutcome::Stopped => panic!("an overflowing hex size is malformed stopped"),
        RequestOutcome::Invalid(why) => {
            panic!("an overflowing hex size is malformed invalid: {why}")
        }
    };
    assert_eq!(crossed.malformed, Some("a chunk size is not a hex number"));
    // A valid but enormous hex chunk: refused before a byte is read.
    let crossed = match read_once(
        bounds(),
        b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFFFFFFFFFFFF\r\n",
    ) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => panic!("an enormous chunk is malformed ended at EOF"),
        RequestOutcome::Stopped => panic!("an enormous chunk is malformed stopped"),
        RequestOutcome::Invalid(why) => panic!("an enormous chunk is malformed invalid: {why}"),
    };
    assert!(crossed.body.is_empty(), "no byte is read past the cap");
    assert_eq!(crossed.malformed, Some("the body exceeds its cap"));
}

/// The next clock-wait acknowledgment naming `phase`, skipping the waits
/// around it: an accept the client beat gets no acknowledgment at all.
fn wait_phase(acks: &mpsc::Receiver<WaitAck>, phase: &'static str) -> WaitAck {
    let deadline = Deadline::after(OP);
    loop {
        let ack = deadline.recv_or_fail(acks, "the next clock wait");
        if ack.phase == phase {
            return ack;
        }
    }
}

/// Drives one phase to its three ends on a fresh server: a peer close,
/// one byte of progress, then silence to exact expiry. Progress never
/// renews the budget: the failure lands at the original bound whatever
/// arrived before it.
fn check_phase(prefix: &[u8], phase: &'static str, progress: &[u8]) {
    let head_phase = !phase.contains("chunk") && !phase.contains("trailer");

    // The peer that closes mid-phase: its end, not the bound, ends
    // the read. The caller reads while a scoped client delivers and
    // closes, so no thread outlives the read.
    let clock = fake_clock();
    let server = bind(&clock, bounds());
    let addr = server.addr();
    let closed = thread::scope(|scope| {
        scope.spawn(move || {
            let mut closing = TcpStream::connect(addr).expect("the test connects");
            closing.write_all(prefix).expect("the prefix writes");
        });
        let mut conn = server
            .accept_expected("the test request")
            .expect("a client connects");
        read_request(&mut conn)
    });
    if head_phase {
        assert_eq!(closed, RequestOutcome::Eof, "a close ends the {phase}");
    } else {
        match closed {
            RequestOutcome::Complete(request) => assert_eq!(
                request.malformed,
                Some("the chunked body ends before its framing does"),
                "a close truncates the {phase}"
            ),
            RequestOutcome::Eof => panic!("a close truncates the {phase} ended at EOF"),
            RequestOutcome::Stopped => panic!("a close truncates the {phase} stopped"),
            RequestOutcome::Invalid(why) => panic!("a close truncates the {phase} invalid: {why}"),
        }
    }

    // One byte of progress, then silence to exact expiry. The prefix is
    // queued before the reader starts, so its first park is the phase
    // under test: that phase's acknowledgment is the one the test waits
    // for. An early park would spend the wait's one acknowledgment for
    // this wake generation on an earlier phase, and the phase under test
    // would park again without announcing itself. The reader borrows its
    // server under a scope, so no clone's drop can end the read early
    // and no thread outlives the phase.
    let clock = fake_clock();
    let server = bind(&clock, bounds());
    let acks = server.test_acknowledgments();
    let addr = server.addr();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        // Inside the scope so a failure drops the client before the
        // join: the drop ends the reader's wait instead of hanging it.
        let mut client = TcpStream::connect(addr).expect("the test connects");
        client.write_all(prefix).expect("the prefix writes");
        scope.spawn(|| {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut conn = server
                    .accept_expected("the test request")
                    .expect("a client connects");
                read_request(&mut conn)
            }));
            match tx.send(outcome) {
                Ok(()) | Err(_) => {}
            }
        });
        let ack = wait_phase(&acks, phase);
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the {phase} parks on the clock before it moves"
        );
        // The park above, marked before the progress byte lands: the
        // wait past the next advance is a later park of this waiter,
        // observed through its acknowledgment and its clock park rather
        // than through wall timing.
        let until = ack.until.expect("the phase waits on the request budget");
        let mark = clock
            .mark_parked(until, Deadline::after(OP))
            .expect("the phase parks on the clock under its budget");
        client.write_all(progress).expect("one byte writes");
        clock.advance(Duration::from_secs(10) - Duration::from_nanos(1));
        assert!(
            clock.await_parked_since(&mark, ack.until, Deadline::after(OP)),
            "one tick before the bound the {phase} parks again instead of ending"
        );
        assert!(
            rx.try_recv().is_err(),
            "one tick before the bound the {phase} is still waiting"
        );
        clock.advance(Duration::from_nanos(1));
        let caught: thread::Result<RequestOutcome> =
            Deadline::after(OP).recv_or_fail(&rx, "the expired read");
        let message = match caught {
            Ok(_) => panic!("the expired {phase} read instead of failing"),
            Err(payload) => payload
                .downcast_ref::<String>()
                .cloned()
                .expect("the failure names its request"),
        };
        assert!(
            message.contains("the test request"),
            "the failure names its request, got: {message}"
        );
        assert!(
            message.contains(phase),
            "the failure names its phase, got: {message}"
        );
    });
}

#[test]
fn a_phase_change_during_the_same_clock_wait_is_announced() {
    let clock = fake_clock();
    let server = bind(&clock, bounds());
    let acks = server.test_acknowledgments();
    let addr = server.addr();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        let mut client = TcpStream::connect(addr).expect("the test connects");
        scope.spawn(|| {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut conn = server
                    .accept_expected("the phase change")
                    .expect("a client connects");
                read_request(&mut conn)
            }));
            match tx.send(outcome) {
                Ok(()) | Err(_) => {}
            }
        });

        // Hold the request line incomplete until the core has acknowledged
        // and parked on it. The client then completes that line, so the
        // same waiter reaches the header phase without a clock advance.
        let line = wait_phase(&acks, "to read the request line");
        assert!(
            clock.await_thread_parked(line.thread, line.until, Deadline::after(OP)),
            "the request-line read parks before the bytes arrive"
        );
        let mark = clock
            .mark_parked(
                line.until.expect("the read has a bound"),
                Deadline::after(OP),
            )
            .expect("the request-line read is parked");
        client
            .write_all(b"G / H\r\n")
            .expect("the request line writes");

        // A phase transition is a distinct clock wait even when no clock
        // wake occurred. Without that announcement, the test cannot
        // distinguish the header wait from the earlier request-line wait.
        let headers = wait_phase(&acks, "to read the headers");
        assert_eq!(headers.thread, line.thread, "the same reader advances");
        assert_eq!(headers.seq, line.seq + 1, "the phase change is sequenced");
        assert!(
            clock.await_parked_since(&mark, headers.until, Deadline::after(OP)),
            "the same reader parks again for the headers"
        );

        client
            .write_all(b"\r\n")
            .expect("the empty header block writes");
        let outcome = Deadline::after(OP).recv_or_fail(&rx, "the completed request");
        let request = complete(outcome.expect("the reader returns instead of panicking"));
        assert_eq!(request.method, "G");
        assert_eq!(request.target, "/");
        assert!(request.headers.is_empty());
    });
}

#[test]
fn every_read_phase_uses_the_original_budget() {
    // Each phase's prefix and its one byte of progress.
    let phases: &[(&[u8], &'static str, &[u8])] = &[
        (b"", "to read the request line", b"G"),
        (b"G / H\r\n", "to read the headers", b"a"),
        (
            b"POST /v1 HTTP/1.1\r\nContent-Length: 8\r\n\r\n",
            "to read the fixed body",
            b"1",
        ),
        (
            b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            "to read a chunk size",
            b"1",
        ),
        (
            b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n",
            "to read chunk data",
            b"1",
        ),
        (
            b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na",
            "to read chunk framing",
            b"\r",
        ),
        (
            b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n",
            "to read the trailers",
            b"t",
        ),
    ];
    for (prefix, phase, progress) in phases {
        check_phase(prefix, phase, progress);
    }
}

/// Trailer count admits the count and refuses one more: the count is a
/// cap of its own beside the aggregate trailer bytes.
#[test]
fn trailer_count_admits_the_count_and_refuses_one_more() {
    let limits = Limits {
        trailer_count: 2,
        ..bounds()
    };
    let trailers = |count: usize| {
        let mut request = b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n".to_vec();
        for n in 0..count {
            request.extend_from_slice(format!("t{n}: v\r\n").as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        request
    };
    assert!(
        matches!(read_once(limits, &trailers(1)), RequestOutcome::Complete(_)),
        "one trailer is admitted"
    );
    assert!(
        matches!(read_once(limits, &trailers(2)), RequestOutcome::Complete(_)),
        "exactly the count is admitted"
    );
    let crossed = match read_once(limits, &trailers(3)) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => panic!("a third trailer is malformed ended at EOF"),
        RequestOutcome::Stopped => panic!("a third trailer is malformed stopped"),
        RequestOutcome::Invalid(why) => panic!("a third trailer is malformed invalid: {why}"),
    };
    assert_eq!(crossed.malformed, Some("there are too many trailers"));
}

/// `Debug` redacts credentials: an `Authorization` header and a `key`
/// query parameter appear only as fingerprints, never as bytes, through
/// both the request and its outcome. A derived `Debug` would print them
/// raw, so this fails against the derive.
#[test]
fn debug_output_redacts_credentials() {
    let credential = "Bearer secret-token-123";
    let key = "secret-query-key-456";
    let request = complete(read_once(
        bounds(),
        format!(
            "GET /v1/models?key={key} HTTP/1.1\r\nHost: fake\r\nAuthorization: {credential}\r\nX-Custom: plain\r\n\r\n"
        )
        .as_bytes(),
    ));
    let shown = format!("{request:?}");
    assert!(
        !shown.contains(credential),
        "no credential bytes in the request's Debug, got: {shown}"
    );
    assert!(
        !shown.contains(key),
        "no query-key bytes in the request's Debug, got: {shown}"
    );
    assert!(
        shown.contains(&crate::provider_server::fingerprint(credential)),
        "the credential's fingerprint names it, got: {shown}"
    );
    assert!(
        shown.contains(&crate::provider_server::fingerprint(key)),
        "the query key's fingerprint names it, got: {shown}"
    );
    assert!(
        shown.contains("plain"),
        "a non-credential header still shows, got: {shown}"
    );
    let outcome = RequestOutcome::Complete(request);
    let shown = format!("{outcome:?}");
    assert!(
        !shown.contains(credential) && !shown.contains(key),
        "no credential bytes in the outcome's Debug, got: {shown}"
    );
    // Byte-for-byte: an empty output would pass the absence checks
    // above, so the exact redacted form pins the `RequestOutcome`
    // `Debug` body itself.
    let credential_print = crate::provider_server::fingerprint(credential);
    let key_print = crate::provider_server::fingerprint(key);
    let expected_request = format!(
        "ParsedRequest {{ method: \"GET\", target: \"/v1/models?key={key_print}\", headers: [(\"host\", \"fake\"), (\"authorization\", {credential_print:?}), (\"x-custom\", \"plain\")], body: [], malformed: None }}"
    );
    let request = match outcome {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => panic!("the outcome Debug test closed instead of completing"),
        RequestOutcome::Stopped => panic!("shutdown ended the outcome Debug test"),
        RequestOutcome::Invalid(why) => panic!("the outcome Debug test was invalid: {why}"),
    };
    assert_eq!(
        format!("{request:?}"),
        expected_request,
        "the request's Debug is exactly the redacted form"
    );
    assert_eq!(
        shown,
        format!("Complete({expected_request})"),
        "the outcome's Debug is exactly the redacted form"
    );
}
