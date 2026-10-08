//! Tests for the hub client: starting one when none runs, the
//! `hub_hello` it must speak first, and the idle-exit race.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;

/// One named deadline per wait: `connect` answers before it.
const DEADLINE: Duration = Duration::from_secs(10);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("dh");
        let dir = held.path().join("h");
        fs::create_dir_all(&dir).unwrap();
        Self { dir, held }
    }

    fn run(&self) -> PathBuf {
        let run = self.dir.join("run");
        fs::create_dir_all(&run).unwrap();
        run
    }
}

/// Runs `connect` on a thread: calling code that blocks is a wait, so the
/// test receives its result with a wall-clock deadline.
fn run_connect(
    home: PathBuf,
    mut start: impl FnMut() -> io::Result<()> + Send + 'static,
    clock: Arc<fakes::clock::FakeClock>,
) -> io::Result<Hub> {
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-connect".to_owned())
        .spawn(move || {
            let hub = connect(&home, &mut start, &*clock);
            done_tx.send(hub).unwrap_or(());
        })
        .unwrap();
    done_rx
        .recv_timeout(DEADLINE)
        .expect("connect answers before its deadline")
}

fn hello_line() -> Vec<u8> {
    br#"{"kind":"hub_hello","ts":1,"schema_version":1,"payload":{"fiber_version":"0.0.0"}}"#
        .to_vec()
}

/// Accepts one connection, writes `line`, and closes.
fn serve_once(listener: UnixListener, line: &[u8]) {
    let (mut stream, _) = listener.accept().unwrap();
    stream.write_all(line).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();
}

#[test]
fn an_existing_hub_is_used_and_the_starter_rests() {
    let temp = Temp::new();
    let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
    thread::spawn(move || serve_once(listener, &hello_line()));
    let calls = Arc::new(AtomicUsize::new(0));
    let hub = run_connect(
        temp.dir.clone(),
        {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(hub.1.kind, "hub_hello");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn without_a_hub_the_starter_runs_once() {
    let temp = Temp::new();
    let home = temp.dir.clone();
    let run = temp.run();
    let calls = Arc::new(AtomicUsize::new(0));
    let hub = run_connect(
        home,
        {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                let listener = UnixListener::bind(run.join("hub")).unwrap();
                thread::spawn(move || serve_once(listener, &hello_line()));
                Ok(())
            }
        },
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(hub.1.kind, "hub_hello");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn without_a_hub_that_binds_connect_fails_past_its_deadline() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let failed = run_connect(
        temp.dir.clone(),
        {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
        Arc::clone(&clock),
    );
    assert!(failed.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(clock.now() >= clock.origin() + CONNECT_DEADLINE);
}

#[test]
fn an_unexpected_connect_error_fails_without_starting() {
    let temp = Temp::new();
    // `run/` is a regular file, so connecting to `run/hub` fails
    // NotADirectory on every platform: on Linux a symlink to a missing
    // target gives NotFound and would start a hub, so no symlink here.
    // NotADirectory is unexpected, so the client starts nothing.
    fs::write(temp.dir.join("run"), b"x").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let failed = run_connect(
        temp.dir.clone(),
        {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
        fakes::clock::FakeClock::new(),
    );
    assert!(failed.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn eof_before_hello_retries_the_whole_connect_once() {
    let temp = Temp::new();
    let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
    let (served_tx, served_rx) = mpsc::channel();
    thread::spawn(move || {
        // The first connection dies silent: the idle-exit race. The
        // second gets its hello.
        let (closed, _) = listener.accept().unwrap();
        drop(closed);
        serve_once(listener, &hello_line());
        served_tx.send(()).unwrap();
    });
    let hub = run_connect(temp.dir.clone(), || Ok(()), fakes::clock::FakeClock::new()).unwrap();
    assert_eq!(hub.1.kind, "hub_hello");
    // `connect` returns once the hello is read, which can be before the
    // server finishes its second connection.
    assert!(
        served_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the server to serve both connections"
    );
}

#[test]
fn a_first_line_that_is_not_hello_is_an_error() {
    for line in [
        br#"{"kind":"command_accepted","ts":1,"schema_version":1,"payload":{}}"#.to_vec(),
        b"not json".to_vec(),
    ] {
        let temp = Temp::new();
        let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
        thread::spawn(move || serve_once(listener, &line));
        let failed = run_connect(temp.dir.clone(), || Ok(()), fakes::clock::FakeClock::new());
        assert!(failed.is_err());
    }
}

#[test]
fn a_hello_on_another_schema_version_is_refused_naming_both() {
    let temp = Temp::new();
    let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
    let line = format!(
        r#"{{"kind":"hub_hello","ts":1,"schema_version":{},"payload":{{"fiber_version":"0.0.0"}}}}"#,
        contract::SCHEMA_VERSION + 1,
    );
    thread::spawn(move || serve_once(listener, line.as_bytes()));
    let error = run_connect(temp.dir.clone(), || Ok(()), fakes::clock::FakeClock::new())
        .expect_err("a hub on another schema version is refused");
    assert_eq!(
        error.to_string(),
        format!(
            "the hub runs schema version {}, this Fiber runs schema version {}; \
             update Fiber or restart the hub, then reconnect",
            contract::SCHEMA_VERSION + 1,
            contract::SCHEMA_VERSION,
        ),
    );
}

fn error(kind: io::ErrorKind) -> io::Error {
    io::Error::new(kind, "injected for the test")
}

#[test]
fn absent_is_not_found_or_connection_refused() {
    assert!(is_absent(io::ErrorKind::NotFound));
    assert!(is_absent(io::ErrorKind::ConnectionRefused));
    for kind in [
        io::ErrorKind::NotADirectory,
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::TimedOut,
        io::ErrorKind::InvalidInput,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::UnexpectedEof,
        io::ErrorKind::InvalidData,
    ] {
        assert!(!is_absent(kind), "{kind:?}");
    }
}

#[test]
fn a_hello_timeout_is_would_block_or_timed_out() {
    assert!(is_hello_timeout(io::ErrorKind::WouldBlock));
    assert!(is_hello_timeout(io::ErrorKind::TimedOut));
    for kind in [
        io::ErrorKind::NotFound,
        io::ErrorKind::ConnectionRefused,
        io::ErrorKind::NotADirectory,
        io::ErrorKind::InvalidInput,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::UnexpectedEof,
        io::ErrorKind::InvalidData,
    ] {
        assert!(!is_hello_timeout(kind), "{kind:?}");
    }
}

#[test]
fn time_is_left_until_the_deadline() {
    assert!(!has_time_left(&Duration::ZERO));
    assert!(has_time_left(&Duration::from_nanos(1)));
    assert!(has_time_left(&Duration::from_secs(5)));
}

#[test]
fn a_carriage_return_is_stripped() {
    assert!(strips_cr(Some(&b'\r')));
    assert!(!strips_cr(Some(&b'\n')));
    assert!(!strips_cr(Some(&b'x')));
    assert!(!strips_cr(None));
}

#[test]
fn only_another_kind_is_rejected() {
    assert!(!rejects_hello("hub_hello"));
    assert!(rejects_hello("command_accepted"));
    assert!(rejects_hello("hub_hello "));
    assert!(rejects_hello(""));
}

#[test]
fn only_another_schema_version_is_rejected() {
    assert!(!rejects_schema(contract::SCHEMA_VERSION));
    assert!(rejects_schema(contract::SCHEMA_VERSION + 1));
    assert!(rejects_schema(contract::SCHEMA_VERSION.wrapping_sub(1)));
}

#[test]
fn only_the_closed_peer_refusal_is_ignored() {
    assert!(is_closed_peer(io::ErrorKind::InvalidInput));
    for kind in [
        io::ErrorKind::NotFound,
        io::ErrorKind::TimedOut,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::PermissionDenied,
    ] {
        assert!(!is_closed_peer(kind), "{kind:?}");
    }
}

#[test]
fn a_handshake_read_is_classified() {
    assert!(matches!(classify_hello_read(Ok(0)), HelloRead::Race));
    assert!(matches!(classify_hello_read(Ok(1)), HelloRead::Byte));
    assert!(matches!(classify_hello_read(Ok(100)), HelloRead::Byte));
    assert!(matches!(
        classify_hello_read(Err(error(io::ErrorKind::WouldBlock))),
        HelloRead::TimedOut
    ));
    assert!(matches!(
        classify_hello_read(Err(error(io::ErrorKind::TimedOut))),
        HelloRead::TimedOut
    ));
    for kind in [
        io::ErrorKind::NotFound,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::InvalidInput,
        io::ErrorKind::UnexpectedEof,
    ] {
        assert!(
            matches!(classify_hello_read(Err(error(kind))), HelloRead::Failed(_)),
            "{kind:?}"
        );
    }
}

#[test]
fn only_the_closed_peer_refusal_is_folded_away() {
    assert!(ignore_closed_peer(Ok(())).is_ok());
    assert!(ignore_closed_peer(Err(error(io::ErrorKind::InvalidInput))).is_ok());
    for kind in [
        io::ErrorKind::TimedOut,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::NotFound,
        io::ErrorKind::ConnectionReset,
    ] {
        let folded = ignore_closed_peer(Err(error(kind)));
        assert_eq!(folded.unwrap_err().kind(), kind, "{kind:?}");
    }
}

/// Runs `connect_until` on a thread with `total` on `clock`: calling code
/// that blocks is a wait, so the test receives its result with a
/// wall-clock deadline.
fn run_connect_until(
    home: PathBuf,
    mut start: impl FnMut() -> io::Result<()> + Send + 'static,
    clock: Arc<fakes::clock::FakeClock>,
    total: Duration,
) -> io::Result<Hub> {
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-connect-until".to_owned())
        .spawn(move || {
            let deadline = clock.now() + total;
            let hub = connect_until(&home, &mut start, &*clock, deadline, &mut || {});
            done_tx.send(hub).unwrap_or(());
        })
        .unwrap();
    done_rx
        .recv_timeout(DEADLINE)
        .expect("connect_until answers before its deadline")
}

#[test]
fn connect_until_uses_an_existing_hub_and_starts_nothing() {
    let temp = Temp::new();
    let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
    thread::spawn(move || serve_once(listener, &hello_line()));
    let calls = Arc::new(AtomicUsize::new(0));
    let hub = run_connect_until(
        temp.dir.clone(),
        {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
        fakes::clock::FakeClock::new(),
        DEADLINE,
    )
    .unwrap();
    assert_eq!(hub.1.kind, "hub_hello");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn connect_until_starts_once_when_absent() {
    let temp = Temp::new();
    let run = temp.run();
    let calls = Arc::new(AtomicUsize::new(0));
    let hub = run_connect_until(
        temp.dir.clone(),
        {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                let listener = UnixListener::bind(run.join("hub")).unwrap();
                thread::spawn(move || serve_once(listener, &hello_line()));
                Ok(())
            }
        },
        fakes::clock::FakeClock::new(),
        DEADLINE,
    )
    .unwrap();
    assert_eq!(hub.1.kind, "hub_hello");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn connect_until_unexpected_error_fails_without_starting() {
    let temp = Temp::new();
    // `run/` is a regular file, so connecting to `run/hub` fails
    // NotADirectory: unexpected, so the client starts nothing.
    fs::write(temp.dir.join("run"), b"x").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let failed = run_connect_until(
        temp.dir.clone(),
        {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
        fakes::clock::FakeClock::new(),
        DEADLINE,
    );
    assert!(failed.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn connect_until_times_out_when_nothing_binds() {
    let temp = Temp::new();
    temp.run();
    let clock = fakes::clock::FakeClock::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let failed = run_connect_until(
        temp.dir.clone(),
        {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
        Arc::clone(&clock),
        Duration::from_secs(5),
    );
    let error = failed.expect_err("nothing binding times out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("did not bind"), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn connect_until_eof_race_retries_once() {
    let temp = Temp::new();
    let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
    let (served_tx, served_rx) = mpsc::channel();
    thread::spawn(move || {
        // The first connection dies silent: the idle-exit race. The
        // second gets its hello.
        let (closed, _) = listener.accept().unwrap();
        drop(closed);
        serve_once(listener, &hello_line());
        served_tx.send(()).unwrap();
    });
    let hub = run_connect_until(
        temp.dir.clone(),
        || Ok(()),
        fakes::clock::FakeClock::new(),
        DEADLINE,
    )
    .unwrap();
    assert_eq!(hub.1.kind, "hub_hello");
    assert!(
        served_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the server to serve both connections"
    );
}

#[test]
fn connect_until_first_line_that_is_not_hello_is_an_error() {
    for line in [
        br#"{"kind":"command_accepted","ts":1,"schema_version":1,"payload":{}}"#.to_vec(),
        b"not json".to_vec(),
    ] {
        let temp = Temp::new();
        let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
        thread::spawn(move || serve_once(listener, &line));
        let failed = run_connect_until(
            temp.dir.clone(),
            || Ok(()),
            fakes::clock::FakeClock::new(),
            DEADLINE,
        );
        assert!(failed.is_err());
    }
}

#[test]
fn connect_until_hello_on_another_schema_version_is_refused() {
    let temp = Temp::new();
    let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
    let line = format!(
        r#"{{"kind":"hub_hello","ts":1,"schema_version":{},"payload":{{"fiber_version":"0.0.0"}}}}"#,
        contract::SCHEMA_VERSION + 1,
    );
    thread::spawn(move || serve_once(listener, line.as_bytes()));
    let error = run_connect_until(
        temp.dir.clone(),
        || Ok(()),
        fakes::clock::FakeClock::new(),
        DEADLINE,
    )
    .expect_err("a hub on another schema version is refused");
    assert!(error.to_string().contains("schema version"), "{error}");
}

#[test]
fn connect_until_reads_a_cr_terminated_hello() {
    let temp = Temp::new();
    let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(&hello_line()).unwrap();
        stream.write_all(b"\r\n").unwrap();
        stream.flush().unwrap();
    });
    let hub = run_connect_until(
        temp.dir.clone(),
        || Ok(()),
        fakes::clock::FakeClock::new(),
        DEADLINE,
    )
    .unwrap();
    assert_eq!(hub.1.kind, "hub_hello");
}

#[test]
fn connect_until_silent_hub_times_out_at_the_shared_deadline() {
    let temp = Temp::new();
    let socket = temp.run().join("hub");
    let listener = UnixListener::bind(&socket).unwrap();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let (silent, _) = listener.accept().unwrap();
        // Held open, silent, until the test is done.
        release_rx.recv_timeout(DEADLINE).unwrap_or(());
        drop(silent);
    });
    let error = run_connect_until(
        temp.dir.clone(),
        || panic!("a hub that accepts is not started"),
        fakes::clock::FakeClock::new(),
        Duration::from_millis(50),
    )
    .expect_err("a silent hub times out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    release_tx.send(()).unwrap_or(());
}

#[test]
fn connect_until_hello_from_a_peer_that_already_closed_is_read() {
    // Setting or clearing the read timeout on a socket whose peer has
    // closed fails on macOS, while the buffered hello stays readable.
    let (mut peer, reader) = UnixStream::pair().unwrap();
    peer.write_all(&hello_line()).unwrap();
    peer.write_all(b"\n").unwrap();
    drop(peer);
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.now() + DEADLINE;
    let got = read_hello_until(
        reader,
        Path::new("run/hub"),
        deadline,
        DEADLINE,
        &*clock,
        &mut || {},
    );
    let Ok((stream, hello)) = got else {
        panic!("the buffered hello is read");
    };
    assert_eq!(hello.kind, "hub_hello");
    assert_eq!(stream.read_timeout().unwrap(), None);
}
fn run_connect_within(home: PathBuf, hello_within: Duration) -> io::Result<Hub> {
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-connect-within".to_owned())
        .spawn(move || {
            let clock = fakes::clock::FakeClock::new();
            let mut start = || -> io::Result<()> { panic!("a hub that accepts is not started") };
            let hub = connect_within(&home, &mut start, &*clock, hello_within);
            done_tx.send(hub).unwrap_or(());
        })
        .unwrap();
    done_rx
        .recv_timeout(DEADLINE)
        .expect("connect_within answers before its deadline")
}

#[test]
fn a_hub_that_accepts_and_never_speaks_times_out_naming_the_socket() {
    let temp = Temp::new();
    let socket = temp.run().join("hub");
    let listener = UnixListener::bind(&socket).unwrap();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let (silent, _) = listener.accept().unwrap();
        // Held open, silent, until the test is done.
        release_rx.recv_timeout(DEADLINE).unwrap_or(());
        drop(silent);
    });
    let error = run_connect_within(temp.dir.clone(), Duration::from_millis(10))
        .expect_err("a silent hub times out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        error.to_string(),
        format!(
            "the hub at {} accepted but did not say hub_hello in 0.01 s",
            socket.display()
        )
    );
    release_tx.send(()).unwrap_or(());
}

#[test]
fn a_hello_in_two_parts_inside_the_deadline_connects() {
    let temp = Temp::new();
    let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let line = hello_line();
        let (first, second) = line.split_at(10);
        stream.write_all(first).unwrap();
        stream.flush().unwrap();
        stream.write_all(second).unwrap();
        stream.write_all(b"\n").unwrap();
    });
    let hub = run_connect_within(temp.dir.clone(), DEADLINE).unwrap();
    assert_eq!(hub.1.kind, "hub_hello");
}

#[test]
fn a_hello_then_close_still_connects() {
    let temp = Temp::new();
    let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
    thread::spawn(move || serve_once(listener, &hello_line()));
    let hub = run_connect_within(temp.dir.clone(), DEADLINE).unwrap();
    assert_eq!(hub.1.kind, "hub_hello");
}

#[test]
fn a_hello_from_a_peer_that_already_closed_is_read() {
    // Setting or clearing the read timeout on a socket whose peer has
    // closed fails on macOS, while the buffered hello stays readable.
    let (mut peer, reader) = UnixStream::pair().unwrap();
    peer.write_all(&hello_line()).unwrap();
    peer.write_all(b"\n").unwrap();
    drop(peer);
    let clock = fakes::clock::FakeClock::new();
    let got = read_hello_with(reader, Path::new("run/hub"), DEADLINE, &*clock, &mut || {});
    let Ok((stream, hello)) = got else {
        panic!("the buffered hello is read");
    };
    assert_eq!(hello.kind, "hub_hello");
    assert_eq!(stream.read_timeout().unwrap(), None);
}

#[test]
fn the_connected_stream_has_no_read_timeout() {
    let temp = Temp::new();
    let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(&hello_line()).unwrap();
        stream.write_all(b"\n").unwrap();
        release_rx.recv_timeout(DEADLINE).unwrap_or(());
    });
    let (stream, hello) = run_connect_within(temp.dir.clone(), DEADLINE).unwrap();
    assert_eq!(hello.kind, "hub_hello");
    assert_eq!(stream.read_timeout().unwrap(), None);
    release_tx.send(()).unwrap_or(());
}

#[test]
fn a_trickling_hub_cannot_extend_the_handshake_deadline() {
    let (mut peer, reader) = UnixStream::pair().unwrap();
    let clock = fakes::clock::FakeClock::new();
    let (read_tx, read_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-trickle".to_owned())
        .spawn({
            let clock = Arc::clone(&clock);
            move || {
                let mut before_read = || read_tx.send(()).unwrap_or(());
                let got = read_hello_with(
                    reader,
                    Path::new("run/hub"),
                    DEADLINE,
                    &*clock,
                    &mut before_read,
                );
                done_tx.send(got).unwrap_or(());
            }
        })
        .unwrap();
    let line = hello_line();
    peer.write_all(&line[..1]).unwrap();
    for n in 1..=2 {
        read_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("the reader is about to read for the {n}th time"));
    }
    // The reader read the first byte and checked the time left for the
    // next: at the deadline now, with no time left, so the next byte ends
    // the handshake.
    clock.advance(DEADLINE);
    peer.write_all(&line[1..2]).unwrap();
    let got = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the reader returns once the deadline has passed");
    let Err(Poll::Failed(error)) = got else {
        panic!("a trickle past the deadline fails");
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        read_rx.try_recv().is_err(),
        "no read after the deadline passed"
    );
}
