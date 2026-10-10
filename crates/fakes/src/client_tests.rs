use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;

use serde_json::Value;

use super::*;
use crate::TempDir;
use crate::clock::FakeClock;
use crate::deadline::Deadline;
use crate::within;

/// A wait that must succeed.
const MUST: Duration = crate::MUST_SUCCEED_WITHIN;

/// A wait that must end with nothing: short, since it always runs out.
const DEADLINE: Duration = Duration::from_secs(2);

/// `accept` on a thread, so the test's wait is the deadline below.
#[track_caller]
fn accept_within(listener: &UnixListener) -> UnixStream {
    let listener = listener.try_clone().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || if let Ok(()) = tx.send(listener.accept()) {});
    let (stream, _) = Deadline::after(MUST)
        .recv(&rx)
        .expect("a client is accepted")
        .unwrap();
    stream
}

#[test]
fn slow_false_starts_the_reader_and_slow_true_does_not() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let _server = accept_within(&listener);
    client.slow(true);
    assert!(
        super::lock(&client.reader).is_none(),
        "slow(true) does not start the reader"
    );
    client.slow(false);
    assert!(
        super::lock(&client.reader).is_some(),
        "slow(false) starts the reader"
    );
}

#[test]
fn a_paused_client_reads_nothing_until_told() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    client.slow(true);
    let mut server = accept_within(&listener);
    server.write_all(b"{\"ok\":true}\n").unwrap();
    assert!(
        client.recv(DEADLINE).is_none(),
        "a paused client reads nothing"
    );
    client.slow(false);
    let line = client.recv(MUST).expect("slow(false) starts the reader");
    assert_eq!(line["ok"], true);
}

#[test]
fn send_writes_the_line_and_a_newline() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let server = accept_within(&listener);
    client.send(r#"{"id":1}"#).unwrap();
    server.set_read_timeout(Some(MUST)).unwrap();
    let mut buf = Vec::new();
    BufReader::new(server)
        .read_until(b'\n', &mut buf)
        .expect("send writes the line and a newline");
    assert_eq!(buf, b"{\"id\":1}\n");
}

#[test]
fn recv_returns_a_buffered_line_without_waiting_for_the_socket_to_close() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let mut server = accept_within(&listener);
    server.write_all(b"{\"ok\":true}\n").unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(
        move || {
            if let Ok(()) = tx.send(client.recv(Duration::from_secs(60))) {}
        },
    );
    let line = Deadline::after(MUST)
        .recv(&rx)
        .expect("recv returns when the line is buffered")
        .expect("recv returns the line");
    assert_eq!(line["ok"], true);
}

#[test]
fn a_line_keeps_no_trailing_cr_or_lf() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let mut server = accept_within(&listener);
    server.write_all(b"hello\r\n").unwrap();
    let line = client.recv(MUST).expect("a line arrives");
    assert_eq!(line, Value::String("hello".into()));
}

#[test]
fn a_running_client_stops_reading_once_paused() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let mut server = accept_within(&listener);
    server.write_all(b"{\"n\":0}\n").unwrap();
    let first = client.recv(MUST).expect("the first line arrives");
    assert_eq!(first["n"], 0);
    client.slow(true);
    for n in 1..100 {
        server
            .write_all(format!("{{\"n\":{n}}}\n").as_bytes())
            .unwrap();
    }
    let mut got = 0;
    while client.recv(DEADLINE).is_some() {
        got += 1;
    }
    assert!(
        got < 99,
        "a paused client stops reading, got {got} more lines"
    );
}

#[test]
fn drop_closes_the_socket() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let mut server = accept_within(&listener);
    server.write_all(b"{\"ok\":true}\n").unwrap();
    server.set_read_timeout(Some(MUST)).unwrap();
    client.recv(MUST).expect("the line arrives");
    drop(client);
    let mut buf = [0u8; 8];
    let n = server.read(&mut buf).expect("drop closes the socket");
    assert_eq!(n, 0, "drop closes the socket");
}

#[test]
fn recv_until_skips_non_matching_lines_and_returns_the_match() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let mut server = accept_within(&listener);
    server
        .write_all(b"{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n{\"n\":4}\n")
        .unwrap();
    let got = client
        .recv_until(MUST, |line| {
            line.get("n").and_then(Value::as_u64) == Some(3)
        })
        .expect("the matching line arrives");
    assert_eq!(got["n"], 3);
    let next = client.recv(MUST).expect("the line after the match");
    assert_eq!(
        next["n"], 4,
        "lines before the match are dropped, later ones kept"
    );
}

#[test]
fn recv_until_without_a_match_consumes_nothing() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let mut server = accept_within(&listener);
    server.write_all(b"{\"n\":1}\n{\"n\":2}\n").unwrap();
    let got = client.recv_until(Duration::from_millis(100), |_| false);
    assert!(got.is_none(), "no match returns none");
    let first = client.recv(MUST).expect("the first queued line");
    assert_eq!(first["n"], 1, "a miss consumes nothing");
}

#[test]
fn recv_until_returns_none_at_once_when_the_socket_closes() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let server = accept_within(&listener);
    drop(server);
    let got = within("a closed socket", Duration::from_secs(10), move || {
        client.recv_until(Duration::from_secs(60), |_| false)
    });
    assert!(got.is_none(), "a close with no match returns none");
}

/// What remains until `end` on `clock`, zero once it has passed: the `left`
/// a caller hands `send_by`.
fn left_until(clock: &FakeClock, end: Instant) -> Duration {
    end.saturating_duration_since(clock.now())
}

/// Reads `server` until end-of-file, 4 KiB at a time, calling `after_read`
/// after each read. Sends the bytes read, so the test's wait is a bounded
/// receive.
fn drain(
    mut server: UnixStream,
    after_read: impl Fn() + Send + 'static,
) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match server.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    after_read();
                }
            }
        }
        if let Ok(()) = tx.send(got) {}
    });
    rx
}

/// A connected client and its server end.
fn pair() -> (TempDir, Client, UnixStream) {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let server = accept_within(&listener);
    (dir, client, server)
}

#[test]
fn send_by_delivers_the_line_with_a_newline() {
    let (_dir, client, server) = pair();
    let clock = FakeClock::new();
    let end = clock.origin() + Duration::from_secs(60);
    let read = drain(server, || {});
    let left_clock = Arc::clone(&clock);
    let client = within("send_by", MUST, move || {
        client
            .send_by(r#"{"id":1}"#, &|| left_until(&left_clock, end))
            .unwrap();
        client
    });
    drop(client);
    let got = Deadline::after(MUST)
        .recv(&read)
        .expect("the peer reads to end-of-file");
    assert_eq!(got, b"{\"id\":1}\n");
}

#[test]
fn send_by_keeps_a_newline_the_line_already_has() {
    let (_dir, client, server) = pair();
    let read = drain(server, || {});
    let client = within("send_by", MUST, move || {
        client.send_by("{}\n", &|| MUST).unwrap();
        client
    });
    drop(client);
    let got = Deadline::after(MUST)
        .recv(&read)
        .expect("the peer reads to end-of-file");
    assert_eq!(got, b"{}\n");
}

#[test]
fn send_by_does_not_renew_its_deadline_across_partial_writes() {
    let (_dir, client, server) = pair();
    let clock = FakeClock::new();
    // Fake seconds, so a real write timeout of what is left never ends a
    // write: only the clock the peer advances can.
    let end = clock.origin() + Duration::from_secs(300);
    let reader_clock = Arc::clone(&clock);
    // Each read is the signal that the sender has made progress.
    let read = drain(server, move || {
        reader_clock.advance(Duration::from_secs(100))
    });
    let line = "x".repeat(1 << 20);
    let left_clock = Arc::clone(&clock);
    let (sent, client) = within("send_by", Duration::from_secs(10), move || {
        let sent = client.send_by(&line, &|| left_until(&left_clock, end));
        (sent, client)
    });
    drop(client);
    let got = Deadline::after(MUST)
        .recv(&read)
        .expect("the peer reads to end-of-file");
    assert_eq!(
        sent.expect_err("the deadline passes before the line is written")
            .kind(),
        std::io::ErrorKind::TimedOut,
    );
    assert!(
        got.len() < 1 << 20,
        "a deadline that does not renew stops the line, the peer read {} bytes",
        got.len()
    );
}

#[test]
fn send_by_at_zero_writes_nothing() {
    let (_dir, client, server) = pair();
    let read = drain(server, || {});
    let (sent, client) = within("send_by", MUST, move || {
        (client.send_by("{}", &|| Duration::ZERO), client)
    });
    drop(client);
    let got = Deadline::after(MUST)
        .recv(&read)
        .expect("the peer reads to end-of-file");
    assert_eq!(
        sent.expect_err("no time is left").kind(),
        std::io::ErrorKind::TimedOut
    );
    assert!(got.is_empty(), "nothing is written at zero, got {got:?}");
}

#[test]
fn send_by_reads_what_is_left_once_per_4_kib_chunk() {
    for (len, reads) in [(4095, 1), (4096, 2)] {
        let (_dir, client, server) = pair();
        let read = drain(server, || {});
        let line = "x".repeat(len);
        let (calls, client) = within("send_by", MUST, move || {
            let calls = AtomicUsize::new(0);
            client
                .send_by(&line, &|| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    MUST
                })
                .unwrap();
            (calls.into_inner(), client)
        });
        drop(client);
        let got = Deadline::after(MUST)
            .recv(&read)
            .expect("the peer reads to end-of-file");
        assert_eq!(got.len(), len + 1, "the whole line and its newline arrive");
        assert_eq!(
            calls,
            reads,
            "a {} byte line with its newline is read in {reads} chunks",
            len + 1
        );
    }
}

#[test]
fn send_by_leaves_the_socket_without_a_write_timeout() {
    let (_dir, client, _server) = pair();
    client.send_by("{}", &|| MUST).unwrap();
    assert_eq!(
        super::lock(&client.write).write_timeout().unwrap(),
        None,
        "a later send is not bounded by send_by's last timeout"
    );
}

/// A reader thread that finishes only once the returned sender drops.
fn held_reader() -> (JoinHandle<()>, mpsc::Sender<()>) {
    let (release, released) = mpsc::channel::<()>();
    let handle = thread::spawn(move || if let Err(mpsc::RecvError) = released.recv() {});
    (handle, release)
}

#[test]
fn stop_reader_outside_an_unwind_fails_naming_the_wait() {
    let (handle, release) = held_reader();
    let missed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        stop_reader(handle, Duration::from_millis(100));
    }));
    let message = *missed
        .expect_err("a reader that misses its deadline fails the test")
        .downcast::<String>()
        .unwrap();
    assert!(
        message.contains("the fake client's reader to stop"),
        "{message}"
    );
    drop(release);
}

#[test]
fn a_client_dropped_during_an_unwind_keeps_the_first_panic() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let _server = accept_within(&listener);
    // The reader signals past its stop check, so the drop below lands
    // while it is blocked in `read`: without the wait the drop could set
    // `stop` first and the reader would exit cleanly without the fix.
    let (blocked, is_blocked) = mpsc::channel();
    client.notify_when_blocked(blocked);
    client.slow(false);
    Deadline::after(MUST)
        .recv(&is_blocked)
        .expect("the reader to block in read");
    // Without its shutdown stream, `Drop` cannot wake the reader, which
    // stays blocked in `read` and would miss `READER_STOP`.
    let wake = super::lock(&client.shutdown).take().unwrap();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _client = client;
        panic!("the first failure");
    }));
    let message = *unwound
        .expect_err("the closure panics")
        .downcast::<&str>()
        .unwrap();
    assert_eq!(message, "the first failure");
    wake.shutdown(std::net::Shutdown::Both).unwrap();
}
