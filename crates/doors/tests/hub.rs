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
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// Runs `entry` on a worker thread, handing back its result and the
/// thread's id, so the test steps the fake clock through
/// `advance_when_parked` naming that thread and its expected park
/// instant. Calling code that blocks is a wait, so the test receives the
/// result with a wall-clock deadline.
fn spawn(
    entry: Entry,
    home: PathBuf,
    start: impl FnMut() -> io::Result<()> + Send + 'static,
    clock: Arc<FakeClock>,
) -> (mpsc::Receiver<io::Result<Hub>>, thread::ThreadId) {
    let name = entry.name();
    let (id_tx, id_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-connect".to_owned())
        .spawn(move || {
            id_tx.send(thread::current().id()).unwrap_or(());
            let mut start = start;
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
    let id = Deadline::after(DEADLINE)
        .recv(&id_rx)
        .unwrap_or_else(|_| panic!("{name} worker reports its thread before {DEADLINE:?}"));
    (done_rx, id)
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
    start: impl FnMut() -> io::Result<()> + Send + 'static,
    clock: Arc<FakeClock>,
) -> io::Result<Hub> {
    let name = entry.name();
    let (done_rx, _) = spawn(entry, home, start, clock);
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

/// Accepts one connection within the test deadline, yielding the thread
/// while none waits. `None` is the stand-in ending: `stop` set or the
/// deadline with no time left. The deadline is the hang guard of the
/// fake's poll loop, not a timing assertion.
fn accept_within(
    listener: &UnixListener,
    deadline: Deadline,
    stop: &AtomicBool,
) -> Option<UnixStream> {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if deadline.left().is_zero() {
                    return None;
                }
                thread::yield_now();
            }
            Err(_) => return None,
        }
    }
    None
}

/// Sets `stop` when dropped, so a stand-in thread ends on every path,
/// including a test panic.
struct StopGuard(Arc<AtomicBool>);

impl Drop for StopGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// An exiting hub: drops the first connection unanswered, holds the
/// second unanswered, then removes the socket and drops the held stream
/// and the listener, as the hub's cleanup closing its socket ends a
/// queued connection. Sends `ready` once bound and listening, `released`
/// once the socket is gone.
fn exiting_hub(
    listener: UnixListener,
    socket: PathBuf,
    ready_tx: mpsc::Sender<()>,
    released_tx: mpsc::Sender<()>,
    stop: &AtomicBool,
    deadline: Deadline,
) {
    ready_tx.send(()).unwrap_or(());
    let Some(first) = accept_within(&listener, deadline, stop) else {
        drop(listener);
        fs::remove_file(&socket).unwrap_or(());
        return;
    };
    drop(first);
    let Some(held) = accept_within(&listener, deadline, stop) else {
        drop(listener);
        fs::remove_file(&socket).unwrap_or(());
        return;
    };
    fs::remove_file(&socket).unwrap_or(());
    drop(held);
    drop(listener);
    released_tx.send(()).unwrap_or(());
}

/// A peer that drops every handshake: counts the drops and stops on
/// `stop`. Sends the final count once ended.
fn dropping_hub(
    listener: UnixListener,
    count: Arc<AtomicUsize>,
    done_tx: mpsc::Sender<usize>,
    stop: &AtomicBool,
    deadline: Deadline,
) {
    loop {
        let Some(dropped) = accept_within(&listener, deadline, stop) else {
            break;
        };
        drop(dropped);
        count.fetch_add(1, Ordering::SeqCst);
    }
    done_tx.send(count.load(Ordering::SeqCst)).unwrap_or(());
}

#[test]
fn a_hub_exiting_past_one_poll_is_started_again_once_its_socket_is_gone() {
    for entry in live() {
        let temp = Temp::new();
        let home = run_home(&temp);
        let socket = home.join("run").join("hub");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let _guard = StopGuard(Arc::clone(&stop));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (released_tx, released_rx) = mpsc::channel();
        let thread_stop = Arc::clone(&stop);
        thread::spawn(move || {
            exiting_hub(
                listener,
                socket,
                ready_tx,
                released_tx,
                &thread_stop,
                Deadline::after(DEADLINE),
            )
        });
        Deadline::after(DEADLINE)
            .recv(&ready_rx)
            .expect("the exiting hub is ready");
        let calls = Arc::new(AtomicUsize::new(0));
        let clock = FakeClock::new();
        let origin = clock.origin();
        let new_socket = home.join("run").join("hub");
        let hub = run(
            entry,
            home,
            {
                let calls = Arc::clone(&calls);
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let listener = UnixListener::bind(&new_socket)?;
                    thread::spawn(move || serve_once(listener, &hello_line()));
                    Ok(())
                }
            },
            Arc::clone(&clock),
        )
        .unwrap_or_else(|error| panic!("{}: the fresh hub answers: {error}", entry.name()));
        assert_eq!(hub.1.kind, "hub_hello", "{}", entry.name());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{}", entry.name());
        assert!(
            Deadline::after(DEADLINE).recv(&released_rx).is_ok(),
            "{}: the exiting hub released its socket",
            entry.name()
        );
        // Two race pauses plus one bind poll after `start`, as `attempt`
        // sleeps once after starting before it connects again.
        assert_eq!(
            clock.now(),
            origin + Duration::from_millis(30),
            "{}",
            entry.name()
        );
    }
}

#[test]
fn a_hub_that_never_speaks_fails_eof_at_the_race_bound() {
    for (entry, bound) in [
        (Entry::Connect, hub::CONNECT_DEADLINE),
        (Entry::Within(DEADLINE), hub::CONNECT_DEADLINE),
        (
            Entry::Until(Duration::from_millis(50)),
            Duration::from_millis(50),
        ),
    ] {
        let temp = Temp::new();
        let home = run_home(&temp);
        let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let _guard = StopGuard(Arc::clone(&stop));
        let count = Arc::new(AtomicUsize::new(0));
        let (done_tx, done_rx) = mpsc::channel();
        let thread_stop = Arc::clone(&stop);
        let thread_count = Arc::clone(&count);
        thread::spawn(move || {
            dropping_hub(
                listener,
                thread_count,
                done_tx,
                &thread_stop,
                Deadline::after(DEADLINE),
            )
        });
        let clock = FakeClock::new();
        let origin = clock.origin();
        let error = run(entry, home, || Ok(()), Arc::clone(&clock))
            .expect_err("a peer that never speaks fails");
        assert_eq!(
            error.kind(),
            io::ErrorKind::UnexpectedEof,
            "{}",
            entry.name()
        );
        assert_eq!(
            error.to_string(),
            "the hub closed the connection before `hub_hello`",
            "{}",
            entry.name()
        );
        assert_eq!(
            clock.now(),
            (origin + bound)
                .checked_sub(Duration::from_millis(10))
                .unwrap(),
            "{}",
            entry.name()
        );
        stop.store(true, Ordering::SeqCst);
        let dropped = Deadline::after(DEADLINE)
            .recv(&done_rx)
            .expect("the dropping hub reports its count");
        assert_eq!(
            dropped,
            (bound.as_millis() / 10) as usize,
            "{}",
            entry.name()
        );
    }
}

#[test]
fn connect_within_max_with_a_dropping_peer_ends_timed_out() {
    let temp = Temp::new();
    let home = run_home(&temp);
    let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let _guard = StopGuard(Arc::clone(&stop));
    let count = Arc::new(AtomicUsize::new(0));
    let (done_tx, done_rx) = mpsc::channel();
    let thread_stop = Arc::clone(&stop);
    let thread_count = Arc::clone(&count);
    thread::spawn(move || {
        dropping_hub(
            listener,
            thread_count,
            done_tx,
            &thread_stop,
            Deadline::after(DEADLINE),
        )
    });
    let clock = FakeClock::new();
    let origin = clock.origin();
    let error = run(
        Entry::Within(Duration::MAX),
        home,
        || Ok(()),
        Arc::clone(&clock),
    )
    .expect_err("a peer that drops every handshake with MAX hello wait ends timed out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    // The overflow falls back to one connect deadline of races, one poll
    // apart, with no poll after the final failure.
    assert_eq!(
        clock.now(),
        (origin + hub::CONNECT_DEADLINE)
            .checked_sub(Duration::from_millis(10))
            .unwrap()
    );
    stop.store(true, Ordering::SeqCst);
    let dropped = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the dropping hub reports its count");
    assert_eq!(dropped, (hub::CONNECT_DEADLINE.as_millis() / 10) as usize);
}

#[test]
fn a_probe_that_meets_an_exiting_hub_fails_with_its_starter_error() {
    for entry in [Entry::Until(DEADLINE), Entry::Within(DEADLINE)] {
        let temp = Temp::new();
        let home = run_home(&temp);
        let socket = home.join("run").join("hub");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let _guard = StopGuard(Arc::clone(&stop));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (released_tx, released_rx) = mpsc::channel();
        let thread_stop = Arc::clone(&stop);
        thread::spawn(move || {
            exiting_hub(
                listener,
                socket,
                ready_tx,
                released_tx,
                &thread_stop,
                Deadline::after(DEADLINE),
            )
        });
        Deadline::after(DEADLINE)
            .recv(&ready_rx)
            .expect("the exiting hub is ready");
        let calls = Arc::new(AtomicUsize::new(0));
        let error = run(
            entry,
            home,
            {
                let calls = Arc::clone(&calls);
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(io::Error::new(io::ErrorKind::NotFound, "no hub runs"))
                }
            },
            FakeClock::new(),
        )
        .expect_err("the probe fails with its starter error");
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{}", entry.name());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{}", entry.name());
        assert!(
            Deadline::after(DEADLINE).recv(&released_rx).is_ok(),
            "{}: the exiting hub released its socket",
            entry.name()
        );
    }
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
    for (entry, within, named) in [
        (
            Entry::Within(Duration::from_millis(10)),
            Duration::from_millis(10),
            "0.01 s",
        ),
        (
            Entry::Until(Duration::from_millis(50)),
            Duration::from_millis(50),
            "0.05 s",
        ),
    ] {
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
        let clock = Arc::new(FakeClock::new());
        let origin = clock.origin();
        let (done_rx, id) = spawn(entry, home, never_start(), Arc::clone(&clock));
        // The silent peer parks the hello wait at its deadline; one
        // parked advance to exactly that instant expires it.
        let due = origin + within;
        clock.advance_when_parked(Some(due), Some(id), within, &Deadline::after(DEADLINE));
        let error = Deadline::after(DEADLINE)
            .recv(&done_rx)
            .unwrap_or_else(|_| panic!("{} answers before {DEADLINE:?}", entry.name()))
            .expect_err("a silent hub times out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{}", entry.name());
        assert_eq!(
            error.to_string(),
            format!(
                "the hub at {} accepted but did not say hub_hello in {named}",
                socket.display()
            ),
            "{}",
            entry.name()
        );
        assert_eq!(clock.now(), due, "{}", entry.name());
        release_tx.send(()).unwrap_or(());
    }
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

/// A listener with no backlog the test saturates with held connects, so a
/// further connect retries within the bind wait on Linux (`EAGAIN`) or
/// waits for completion elsewhere: either way it never starts a hub.
fn saturated_backlog(socket: &std::path::Path) -> (UnixListener, Vec<UnixStream>) {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketType};
    let fd = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None).expect("a socket");
    rustix::net::bind(&fd, &SocketAddrUnix::new(socket).expect("a short path"))
        .expect("the test binds run/hub");
    rustix::net::listen(&fd, 0).expect("no backlog");
    let mut held = Vec::new();
    for _ in 0..32 {
        // Non-blocking: once the queue is full a blocking connect waits
        // for an accept that never comes (Linux), hanging setup. A queued
        // socket holds its slot; EAGAIN means the backlog is saturated.
        let queued = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None)
            .expect("a queued socket");
        rustix::fs::fcntl_setfl(
            &queued,
            rustix::fs::fcntl_getfl(&queued).expect("a queued socket's flags")
                | rustix::fs::OFlags::NONBLOCK,
        )
        .expect("a non-blocking queue");
        match rustix::net::connect(&queued, &SocketAddrUnix::new(socket).expect("a short path")) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::INPROGRESS => {}
            Err(error) if error == rustix::io::Errno::AGAIN => {
                drop(queued);
                break;
            }
            Err(error) => panic!("a queued connect: {error}"),
        }
        held.push(UnixStream::from(queued));
    }
    assert!(
        !held.is_empty(),
        "at least one queued connect holds the backlog"
    );
    let listener: UnixListener = fd.into();
    listener.set_nonblocking(true).expect("a polled stand-in");
    (listener, held)
}

#[test]
fn a_full_backlog_retries_the_connect_without_starting_until_release() {
    let temp = Temp::new();
    let home = run_home(&temp);
    let socket = home.join("run").join("hub");
    let (listener, held) = saturated_backlog(&socket);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let stop = Arc::new(AtomicBool::new(false));
    let _guard = StopGuard(Arc::clone(&stop));
    let thread_stop = Arc::clone(&stop);
    thread::spawn(move || {
        // Held: nothing accepts until the test releases.
        release_rx.recv().unwrap_or(());
        // Then every queued connection gets its hello until stopped.
        while !thread_stop.load(Ordering::SeqCst) {
            let Some(mut stream) =
                accept_within(&listener, Deadline::after(DEADLINE), &thread_stop)
            else {
                break;
            };
            stream.write_all(&hello_line()).unwrap_or(());
            stream.write_all(b"\n").unwrap_or(());
        }
    });
    let clock = Arc::new(FakeClock::new());
    let origin = clock.origin();
    let (done_rx, id) = spawn(
        Entry::Until(DEADLINE),
        home,
        never_start(),
        Arc::clone(&clock),
    );
    // Five parked steps with nothing accepted: still trying, never
    // starting, and the bind wait's instant never moves.
    let bind = origin + DEADLINE;
    for _ in 0..5 {
        assert!(
            done_rx.try_recv().is_err(),
            "no answer while the backlog holds"
        );
        clock.advance_when_parked(
            Some(bind),
            Some(id),
            Duration::from_secs(1),
            &Deadline::after(DEADLINE),
        );
    }
    release_tx.send(()).unwrap_or(());
    // The release is the completing event: the capped real poll delivers
    // the hello with no further advance.
    let (stream, hello) = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .unwrap_or_else(|_| panic!("the released backlog answers before {DEADLINE:?}"))
        .expect("the released backlog connects");
    assert_eq!(hello.kind, "hub_hello");
    assert_eq!(stream.read_timeout().unwrap(), None);
    drop(held);
}

#[test]
fn a_backlog_that_never_releases_fails_past_its_bind_deadline() {
    let temp = Temp::new();
    let home = run_home(&temp);
    let socket = home.join("run").join("hub");
    let (_listener, held) = saturated_backlog(&socket);
    let clock = Arc::new(FakeClock::new());
    let origin = clock.origin();
    let (done_rx, id) = spawn(
        Entry::Until(DEADLINE),
        home,
        never_start(),
        Arc::clone(&clock),
    );
    let bind = origin + DEADLINE;
    for _ in 0..10 {
        clock.advance_when_parked(
            Some(bind),
            Some(id),
            Duration::from_secs(1),
            &Deadline::after(DEADLINE),
        );
    }
    let error = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .unwrap_or_else(|_| panic!("the held backlog answers before {DEADLINE:?}"))
        .expect_err("a backlog that never releases times out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    // Where the platform refuses the saturated connect the bind wait
    // expires; where it queues, the unanswered hello wait does. Either
    // way the socket and its ten seconds are named.
    assert!(
        error.to_string().contains(&socket.display().to_string()),
        "the error names the socket: {error}"
    );
    assert!(
        error.to_string().contains("in 10 s"),
        "the error names the bind wait: {error}"
    );
    assert_eq!(clock.now(), bind);
    drop(held);
}

/// Runs the probe on a worker thread, handing back its answer and the
/// thread's id, so the test steps the fake clock naming that thread.
fn spawn_probe(
    home: PathBuf,
    clock: Arc<FakeClock>,
    deadline: std::time::Instant,
) -> (mpsc::Receiver<hub::Probe>, thread::ThreadId) {
    let (id_tx, id_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-probe".to_owned())
        .spawn(move || {
            id_tx.send(thread::current().id()).unwrap_or(());
            done_tx
                .send(hub::probe_until(&home, &*clock, deadline))
                .unwrap_or(());
        })
        .unwrap();
    let id = Deadline::after(DEADLINE)
        .recv(&id_rx)
        .unwrap_or_else(|_| panic!("the probe worker reports its thread before {DEADLINE:?}"));
    (done_rx, id)
}

#[test]
fn a_probe_with_no_socket_is_absent() {
    let temp = Temp::new();
    let home = run_home(&temp);
    let clock = FakeClock::new();
    let deadline = clock.now() + DEADLINE;
    assert!(matches!(
        hub::probe_until(&home, &*clock, deadline),
        hub::Probe::Absent
    ));
}

#[test]
fn a_probe_with_a_stale_socket_is_absent() {
    let temp = Temp::new();
    let home = run_home(&temp);
    let socket = home.join("run").join("hub");
    // Bound, then closed without accepting: the file stays, nobody
    // listens.
    drop(UnixListener::bind(&socket).unwrap());
    let clock = FakeClock::new();
    let deadline = clock.now() + DEADLINE;
    assert!(matches!(
        hub::probe_until(&home, &*clock, deadline),
        hub::Probe::Absent
    ));
}

#[test]
fn a_probe_with_a_regular_file_at_the_socket_fails() {
    let temp = Temp::new();
    let home = run_home(&temp);
    let socket = home.join("run").join("hub");
    fs::write(&socket, b"not a socket").unwrap();
    let clock = FakeClock::new();
    let deadline = clock.now() + DEADLINE;
    assert!(matches!(
        hub::probe_until(&home, &*clock, deadline),
        hub::Probe::Failed(_)
    ));
}

#[test]
fn a_probe_with_a_hello_connects() {
    let temp = Temp::new();
    let home = run_home(&temp);
    let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
    thread::spawn(move || serve_once(listener, &hello_line()));
    let clock = FakeClock::new();
    let deadline = clock.now() + DEADLINE;
    // Served at once: the capped real poll delivers the hello with no
    // parked advance.
    match hub::probe_until(&home, &*clock, deadline) {
        hub::Probe::Connected(Ok((stream, hello))) => {
            assert_eq!(hello.kind, "hub_hello");
            assert_eq!(stream.read_timeout().unwrap(), None);
            let flags = rustix::fs::fcntl_getfl(&stream).unwrap();
            assert!(
                !flags.contains(rustix::fs::OFlags::NONBLOCK),
                "the returned stream blocks again: {flags:?}"
            );
        }
        hub::Probe::Connected(Err(error)) => panic!("a hello connects: {error}"),
        hub::Probe::Absent => panic!("a hello is not absent"),
        hub::Probe::Failed(error) => panic!("a hello connects: {error}"),
    }
}

#[test]
fn a_probe_with_a_silent_hub_connects_err() {
    let temp = Temp::new();
    let home = run_home(&temp);
    let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let (silent, _) = listener.accept().unwrap();
        release_rx.recv().unwrap_or(());
        drop(silent);
    });
    let clock = Arc::new(FakeClock::new());
    let origin = clock.origin();
    let deadline = origin + Duration::from_millis(50);
    let (done_rx, id) = spawn_probe(home, Arc::clone(&clock), deadline);
    clock.advance_when_parked(
        Some(deadline),
        Some(id),
        Duration::from_millis(50),
        &Deadline::after(DEADLINE),
    );
    match Deadline::after(DEADLINE)
        .recv(&done_rx)
        .unwrap_or_else(|_| panic!("the silent hub answers before {DEADLINE:?}"))
    {
        hub::Probe::Connected(Err(error)) => {
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert!(
                error.to_string().contains("did not say hub_hello"),
                "the handshake error names the hello wait: {error}"
            );
        }
        hub::Probe::Connected(Ok(_)) => panic!("a silent hub has no hello"),
        hub::Probe::Absent => panic!("an accepted hub is not absent"),
        hub::Probe::Failed(error) => panic!("an accepted hub is not failed: {error}"),
    }
    assert_eq!(clock.now(), deadline);
    release_tx.send(()).unwrap_or(());
}

#[test]
fn a_probe_that_meets_a_dropping_hub_is_failed() {
    let temp = Temp::new();
    let home = run_home(&temp);
    let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let _guard = StopGuard(Arc::clone(&stop));
    let count = Arc::new(AtomicUsize::new(0));
    let (done_tx, done_rx) = mpsc::channel();
    let thread_stop = Arc::clone(&stop);
    let thread_count = Arc::clone(&count);
    thread::spawn(move || {
        dropping_hub(
            listener,
            thread_count,
            done_tx,
            &thread_stop,
            Deadline::after(DEADLINE),
        )
    });
    let clock = FakeClock::new();
    let deadline = clock.now() + DEADLINE;
    // Every handshake ends at once, so the race loop needs no parked
    // advance: the fake sleeps move its own clock.
    match hub::probe_until(&home, &*clock, deadline) {
        hub::Probe::Failed(error) => {
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
            assert_eq!(
                error.to_string(),
                "the hub closed the connection before `hub_hello`"
            );
        }
        hub::Probe::Connected(_) => panic!("a dropping hub never answers"),
        hub::Probe::Absent => panic!("a dropping hub is not absent"),
    }
    stop.store(true, Ordering::SeqCst);
    let dropped = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the dropping hub reports its count");
    assert!(dropped > 0, "the probe retried past at least one race");
}

#[test]
fn a_trickling_hub_cannot_extend_the_handshake_deadline() {
    for entry in [Entry::Within(DEADLINE), Entry::Until(DEADLINE)] {
        let temp = Temp::new();
        let home = run_home(&temp);
        let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
        let (accepted_tx, accepted_rx) = mpsc::channel::<()>();
        let (byte_tx, byte_rx) = mpsc::channel::<u8>();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            accepted_tx.send(()).unwrap_or(());
            // One byte per test step, no newline: the line never completes.
            for byte in byte_rx {
                stream.write_all(&[byte]).unwrap_or(());
                stream.flush().unwrap_or(());
            }
        });
        let clock = Arc::new(FakeClock::new());
        let origin = clock.origin();
        let (done_rx, id) = spawn(entry, home, never_start(), Arc::clone(&clock));
        Deadline::after(DEADLINE)
            .recv(&accepted_rx)
            .expect("the hub accepts");
        let due = origin + DEADLINE;
        let line = hello_line();
        let mut mark = None;
        for (step, byte) in line.iter().take(10).enumerate() {
            byte_tx.send(*byte).unwrap();
            // Short of the deadline: the byte arrives, the wait stays at
            // the original instant.
            let advanced = clock.advance_when_parked(
                Some(due),
                Some(id),
                Duration::from_millis(10),
                &Deadline::after(DEADLINE),
            );
            if step == 0 {
                mark = Some(advanced);
            }
        }
        assert!(
            done_rx.try_recv().is_err(),
            "{}: ten bytes are not a hello",
            entry.name()
        );
        assert!(
            clock.await_parked_since(
                mark.as_ref().expect("a first advance"),
                Some(due),
                fakes::Deadline::after(DEADLINE)
            ),
            "{}: the wait still parks at the original deadline",
            entry.name()
        );
        // Exactly the deadline: the trickle never renewed it.
        let left = due.saturating_duration_since(clock.now());
        clock.advance_when_parked(Some(due), Some(id), left, &Deadline::after(DEADLINE));
        let error = Deadline::after(DEADLINE)
            .recv(&done_rx)
            .unwrap_or_else(|_| panic!("{} answers before {DEADLINE:?}", entry.name()))
            .expect_err("a trickled hub times out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{}", entry.name());
        assert_eq!(clock.now(), due, "{}", entry.name());
    }
}
