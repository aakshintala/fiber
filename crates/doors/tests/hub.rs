//! The hub connect path through its public API: every scenario reaches
//! each entry point it names (`docs/testing.md`, "Levels").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{self, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use doors::hub::{self, Hub};
use fakes::Deadline;
use fakes::clock::FakeClock;
use support::{DEADLINE, Temp, hello_line};

/// One public entry point to the hub connect path.
#[derive(Clone, Copy)]
enum Entry {
    /// [`hub::connect`]: the hello is due within the connect deadline.
    Connect,
    /// [`hub::connect_until`]: the bind wait and the hello share `total`.
    Until(Duration),
    /// [`hub::connect_within`]: the hello is due this long after accept.
    Within(Duration),
}

impl Entry {
    fn name(&self) -> String {
        match self {
            Entry::Connect => "connect".to_owned(),
            Entry::Until(total) => format!("connect_until({total:?})"),
            Entry::Within(within) => format!("connect_within({within:?})"),
        }
    }
}

/// The entries a scenario that needs no timeout reaches.
fn live() -> [Entry; 3] {
    [
        Entry::Connect,
        Entry::Until(DEADLINE),
        Entry::Within(DEADLINE),
    ]
}

/// Runs `entry` on a thread: calling code that blocks is a wait, so the
/// test receives its result with a wall-clock deadline.
#[track_caller]
fn run(
    entry: Entry,
    home: PathBuf,
    mut start: impl FnMut() -> io::Result<()> + Send + 'static,
    clock: Arc<FakeClock>,
) -> io::Result<Hub> {
    let name = entry.name();
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-connect".to_owned())
        .spawn(move || {
            let hub = match entry {
                Entry::Connect => hub::connect(&home, &mut start, &*clock),
                Entry::Until(total) => {
                    let deadline = clock.now() + total;
                    hub::connect_until(&home, &mut start, &*clock, deadline, &mut || {})
                }
                Entry::Within(within) => hub::connect_within(&home, &mut start, &*clock, within),
            };
            done_tx.send(hub).unwrap_or(());
        })
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .unwrap_or_else(|_| panic!("{name} answers before {DEADLINE:?}"))
}

/// A home with a `run/` directory and no hub yet.
fn run_home(temp: &Temp) -> PathBuf {
    let home = temp.0.clone();
    fs::create_dir_all(home.join("run")).unwrap();
    home
}

/// Accepts one connection, writes `line`, and closes.
fn serve_once(listener: UnixListener, line: &[u8]) {
    let (mut stream, _) = listener.accept().unwrap();
    stream.write_all(line).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();
}

/// Accepts two connections, drops both silent, then reports.
fn drop_twice(listener: UnixListener, dropped_tx: mpsc::Sender<()>) {
    for _ in 0..2 {
        let (closed, _) = listener.accept().unwrap();
        drop(closed);
    }
    dropped_tx.send(()).unwrap_or(());
}

/// A `start` that binds the old hub on its first call and a new hub on
/// its second, counting calls in `calls` and reporting the old hub's
/// exit on `exited_tx`. The old hub accepts one connection, drops it
/// silent, then exits, unlinking its socket before the client sees EOF,
/// so the retry finds no hub. The new hub speaks `hub_hello`.
fn restarting_start(
    socket: PathBuf,
    calls: Arc<AtomicUsize>,
    exited_tx: mpsc::Sender<()>,
) -> impl FnMut() -> io::Result<()> {
    move || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let socket = socket.clone();
            let exited_tx = exited_tx.clone();
            let listener = UnixListener::bind(&socket)?;
            thread::spawn(move || {
                let (closed, _) = listener.accept().unwrap();
                fs::remove_file(&socket).unwrap();
                drop(listener);
                drop(closed);
                exited_tx.send(()).unwrap_or(());
            });
            return Ok(());
        }
        let listener = UnixListener::bind(&socket)?;
        thread::spawn(move || serve_once(listener, &hello_line()));
        Ok(())
    }
}

/// A `start` that must never run: the hub already accepts.
fn never_start() -> impl FnMut() -> io::Result<()> + Send + 'static {
    || panic!("a hub that accepts is not started")
}

#[test]
fn an_existing_hub_is_used_and_the_starter_rests() {
    for entry in live() {
        let temp = Temp::new();
        let home = run_home(&temp);
        let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
        thread::spawn(move || serve_once(listener, &hello_line()));
        let calls = Arc::new(AtomicUsize::new(0));
        let hub = run(
            entry,
            home,
            {
                let calls = Arc::clone(&calls);
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            FakeClock::new(),
        )
        .unwrap();
        assert_eq!(hub.1.kind, "hub_hello", "{}", entry.name());
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{}", entry.name());
    }
}

#[test]
fn without_a_hub_the_starter_runs_once() {
    for entry in live() {
        let temp = Temp::new();
        let home = run_home(&temp);
        let socket = home.join("run").join("hub");
        let calls = Arc::new(AtomicUsize::new(0));
        let hub = run(
            entry,
            home,
            {
                let calls = Arc::clone(&calls);
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let listener = UnixListener::bind(&socket)?;
                    thread::spawn(move || serve_once(listener, &hello_line()));
                    Ok(())
                }
            },
            FakeClock::new(),
        )
        .unwrap();
        assert_eq!(hub.1.kind, "hub_hello", "{}", entry.name());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{}", entry.name());
    }
}

#[test]
fn an_unexpected_connect_error_fails_without_starting() {
    for entry in live() {
        let temp = Temp::new();
        let home = temp.0.clone();
        // `run/` is a regular file, so connecting to `run/hub` fails
        // NotADirectory on every platform: on Linux a symlink to a missing
        // target gives NotFound and would start a hub, so no symlink here.
        // NotADirectory is unexpected, so the client starts nothing.
        fs::write(home.join("run"), b"x").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let failed = run(
            entry,
            home,
            {
                let calls = Arc::clone(&calls);
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            FakeClock::new(),
        );
        assert!(failed.is_err(), "{}", entry.name());
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{}", entry.name());
    }
}

#[test]
fn without_a_hub_that_binds_the_connect_fails_past_its_bind_deadline() {
    for (entry, bound) in [
        (Entry::Connect, hub::CONNECT_DEADLINE),
        (Entry::Until(Duration::from_secs(5)), Duration::from_secs(5)),
    ] {
        let temp = Temp::new();
        let home = run_home(&temp);
        let clock = FakeClock::new();
        let origin = clock.origin();
        let calls = Arc::new(AtomicUsize::new(0));
        let failed = run(
            entry,
            home,
            {
                let calls = Arc::clone(&calls);
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            Arc::clone(&clock),
        );
        let error = failed.expect_err("nothing binding times out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{}", entry.name());
        assert!(
            error.to_string().contains("did not bind"),
            "{}: {error}",
            entry.name()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{}", entry.name());
        assert!(clock.now() >= origin + bound, "{}", entry.name());
    }
}

#[test]
fn eof_before_hello_retries_the_whole_connect_once() {
    for entry in live() {
        let temp = Temp::new();
        let home = run_home(&temp);
        let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
        let (served_tx, served_rx) = mpsc::channel();
        thread::spawn(move || {
            // The first connection dies silent: the idle-exit race. The
            // second gets its hello.
            let (closed, _) = listener.accept().unwrap();
            drop(closed);
            serve_once(listener, &hello_line());
            served_tx.send(()).unwrap();
        });
        let hub = run(entry, home, || Ok(()), FakeClock::new()).unwrap();
        assert_eq!(hub.1.kind, "hub_hello", "{}", entry.name());
        // The connect returns once the hello is read, which can be before
        // the server finishes its second connection.
        assert!(
            Deadline::after(DEADLINE).recv(&served_rx).is_ok(),
            "{}: waited {DEADLINE:?} for the server to serve both connections",
            entry.name()
        );
    }
}

#[test]
fn a_hub_that_exited_before_the_retry_is_started_again() {
    for entry in live() {
        let temp = Temp::new();
        let home = run_home(&temp);
        let socket = home.join("run").join("hub");
        let (exited_tx, exited_rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let hub = run(
            entry,
            home,
            restarting_start(socket, Arc::clone(&calls), exited_tx),
            FakeClock::new(),
        )
        .unwrap();
        assert_eq!(hub.1.kind, "hub_hello", "{}", entry.name());
        assert_eq!(calls.load(Ordering::SeqCst), 2, "{}", entry.name());
        assert!(
            Deadline::after(DEADLINE).recv(&exited_rx).is_ok(),
            "{}: waited {DEADLINE:?} for the old hub to exit",
            entry.name()
        );
    }
}

#[test]
fn the_retry_after_a_dropped_handshake_waits_one_poll() {
    for entry in live() {
        let temp = Temp::new();
        let home = run_home(&temp);
        let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        thread::spawn(move || drop_twice(listener, dropped_tx));
        let clock = FakeClock::new();
        let origin = clock.origin();
        let error = run(entry, home, || Ok(()), Arc::clone(&clock))
            .expect_err("two silent connections fail the connect");
        assert_eq!(
            error.kind(),
            io::ErrorKind::UnexpectedEof,
            "{}",
            entry.name()
        );
        // One poll between the attempts, and none after the final failure.
        // The 10 ms mirrors the private `CONNECT_POLL` the skeleton sleeps.
        assert_eq!(
            clock.now(),
            origin + Duration::from_millis(10),
            "{}",
            entry.name()
        );
        assert!(
            Deadline::after(DEADLINE).recv(&dropped_rx).is_ok(),
            "{}: waited {DEADLINE:?} for the server to drop both connections",
            entry.name()
        );
    }
}

#[test]
fn a_first_line_that_is_not_hello_is_an_error() {
    for entry in live() {
        for line in [
            br#"{"kind":"command_accepted","ts":1,"schema_version":1,"payload":{}}"#.to_vec(),
            b"not json".to_vec(),
        ] {
            let temp = Temp::new();
            let home = run_home(&temp);
            let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
            let served = line.clone();
            thread::spawn(move || serve_once(listener, &served));
            let failed = run(entry, home, never_start(), FakeClock::new());
            assert!(failed.is_err(), "{}: {line:?}", entry.name());
        }
    }
}

#[test]
fn a_hello_on_another_schema_version_is_refused_naming_both() {
    for entry in live() {
        let temp = Temp::new();
        let home = run_home(&temp);
        let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
        let line = format!(
            r#"{{"kind":"hub_hello","ts":1,"schema_version":{},"payload":{{"fiber_version":"0.0.0"}}}}"#,
            contract::SCHEMA_VERSION + 1,
        );
        thread::spawn(move || serve_once(listener, line.as_bytes()));
        let error = run(entry, home, never_start(), FakeClock::new())
            .expect_err("another schema is refused");
        assert_eq!(
            error.to_string(),
            format!(
                "the hub runs schema version {}, this Fiber runs schema version {}; \
                 update Fiber or restart the hub, then reconnect",
                contract::SCHEMA_VERSION + 1,
                contract::SCHEMA_VERSION,
            ),
            "{}",
            entry.name()
        );
    }
}

#[test]
fn a_cr_terminated_hello_is_read() {
    for entry in live() {
        let temp = Temp::new();
        let home = run_home(&temp);
        let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&hello_line()).unwrap();
            stream.write_all(b"\r\n").unwrap();
            stream.flush().unwrap();
        });
        let hub = run(entry, home, never_start(), FakeClock::new()).unwrap();
        assert_eq!(hub.1.kind, "hub_hello", "{}", entry.name());
    }
}

#[test]
fn a_silent_hub_times_out_naming_its_hello_wait() {
    let temp = Temp::new();
    let home = run_home(&temp);
    let socket = home.join("run").join("hub");
    let listener = UnixListener::bind(&socket).unwrap();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let (silent, _) = listener.accept().unwrap();
        // Held open, silent, until the test is done.
        release_rx.recv().unwrap_or(());
        drop(silent);
    });
    let error = run(
        Entry::Within(Duration::from_millis(10)),
        home,
        never_start(),
        FakeClock::new(),
    )
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

    let temp = Temp::new();
    let home = run_home(&temp);
    let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let (silent, _) = listener.accept().unwrap();
        // Held open, silent, until the test is done.
        release_rx.recv().unwrap_or(());
        drop(silent);
    });
    let error = run(
        Entry::Until(Duration::from_millis(50)),
        home,
        never_start(),
        FakeClock::new(),
    )
    .expect_err("a silent hub times out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        error.to_string().contains("0.05 s"),
        "the message names the shared deadline: {error}"
    );
    release_tx.send(()).unwrap_or(());
}

#[test]
fn a_hello_in_two_parts_inside_the_deadline_connects() {
    for entry in live() {
        let temp = Temp::new();
        let home = run_home(&temp);
        let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let line = hello_line();
            let (first, second) = line.split_at(10);
            stream.write_all(first).unwrap();
            stream.flush().unwrap();
            stream.write_all(second).unwrap();
            stream.write_all(b"\n").unwrap();
        });
        let hub = run(entry, home, never_start(), FakeClock::new()).unwrap();
        assert_eq!(hub.1.kind, "hub_hello", "{}", entry.name());
    }
}

#[test]
fn a_hello_then_close_still_connects() {
    for entry in live() {
        let temp = Temp::new();
        let home = run_home(&temp);
        let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
        thread::spawn(move || serve_once(listener, &hello_line()));
        let hub = run(entry, home, never_start(), FakeClock::new()).unwrap();
        assert_eq!(hub.1.kind, "hub_hello", "{}", entry.name());
    }
}

#[test]
fn the_connected_stream_has_no_read_timeout() {
    for entry in live() {
        let temp = Temp::new();
        let home = run_home(&temp);
        let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&hello_line()).unwrap();
            stream.write_all(b"\n").unwrap();
            release_rx.recv().unwrap_or(());
        });
        let (stream, hello) = run(entry, home, never_start(), FakeClock::new()).unwrap();
        assert_eq!(hello.kind, "hub_hello", "{}", entry.name());
        assert_eq!(stream.read_timeout().unwrap(), None, "{}", entry.name());
        release_tx.send(()).unwrap_or(());
    }
}
