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
use std::os::unix::net::UnixListener;
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
fn eof_before_hello_retries_the_whole_connect_once() {
    let temp = Temp::new();
    let listener = UnixListener::bind(temp.run().join("hub")).unwrap();
    let served = Arc::new(AtomicUsize::new(0));
    thread::spawn({
        let served = Arc::clone(&served);
        move || {
            // The first connection dies silent: the idle-exit race. The
            // second gets its hello.
            let (closed, _) = listener.accept().unwrap();
            drop(closed);
            served.fetch_add(1, Ordering::SeqCst);
            serve_once(listener, &hello_line());
            served.fetch_add(1, Ordering::SeqCst);
        }
    });
    let hub = run_connect(temp.dir.clone(), || Ok(()), fakes::clock::FakeClock::new()).unwrap();
    assert_eq!(hub.1.kind, "hub_hello");
    assert_eq!(served.load(Ordering::SeqCst), 2);
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
