//! Tests for idle exit and signal shutdown: the hub parks with no client
//! connected, leaves while one is, resets on arrival, and stops on signal.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use crate::connection::Hub;
use crate::fake::FakeStarter;

/// One named deadline per wait: the hub answers before it.
const DEADLINE: Duration = Duration::from_secs(10);

/// How long `await_parked` waits for the hub to park, in wall time.
const WITHIN: Duration = Duration::from_secs(10);

/// Thirty idle minutes, the documented default.
const IDLE: Duration = Duration::from_millis(1_800_000);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hi");
        let dir = held.path().join("h");
        fs::create_dir_all(&dir).unwrap();
        Self { dir, held }
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("run").join("hub")
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("logs").join("hub.log")).unwrap_or_default()
    }
}

/// Serves the hub on a thread; the sender receives its exit code.
fn serve_in(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
) -> mpsc::Receiver<i32> {
    let home = temp.dir.clone();
    let timed: Arc<dyn contract::clock::Clock> = clock;
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-serve".to_owned())
        .spawn(move || {
            let code = crate::serve(
                &home,
                idle,
                "0.0.0",
                Arc::new(FakeStarter::hang(&home)),
                timed,
            );
            done_tx.send(code).unwrap_or(());
        })
        .unwrap();
    done_rx
}

/// A client on the hub's socket: connected once its `hub_hello` arrives.
struct Client {
    _stream: UnixStream,
}

fn connect(temp: &Temp) -> (Client, String) {
    let stream = UnixStream::connect(temp.socket()).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut read = BufReader::new(stream.try_clone().unwrap());
    let mut hello = String::new();
    read.read_line(&mut hello).expect("the hub speaks first");
    (Client { _stream: stream }, hello)
}

#[test]
fn with_no_clients_the_hub_parks_then_exits_idle() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let done = serve_in(&temp, IDLE, Arc::clone(&clock));
    let deadline = clock.origin() + IDLE;
    assert!(clock.await_parked(deadline, WITHIN));
    clock.advance(IDLE + Duration::from_millis(1));
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub exits idle"), 0);
    assert!(!temp.socket().exists());
    let log = temp.log();
    assert!(log.contains("\"code\":\"hub_started\""));
    assert!(log.contains("\"code\":\"hub_stopped\""));
    assert!(log.contains("The hub stopped: idle."));
}

#[test]
fn a_connected_client_keeps_the_hub_and_leaving_exits_it() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let done = serve_in(&temp, IDLE, Arc::clone(&clock));
    assert!(clock.await_parked(clock.origin() + IDLE, WITHIN));
    let (client, hello) = connect(&temp);
    assert!(hello.contains("\"kind\":\"hub_hello\""));
    // The client is connected: a whole idle period passes without exit.
    clock.advance(IDLE + Duration::from_millis(1));
    assert!(
        done.recv_timeout(Duration::from_millis(200)).is_err(),
        "a connected client keeps the hub"
    );
    drop(client);
    // Leaving exits it: advances wake the hub until it has seen a whole
    // idle period with no client connected.
    let mut code = None;
    for _ in 0..10 {
        clock.advance(IDLE);
        if let Ok(exit) = done.recv_timeout(Duration::from_millis(200)) {
            code = Some(exit);
            break;
        }
    }
    assert_eq!(code, Some(0));
    assert!(!temp.socket().exists());
}

#[test]
fn a_reconnect_resets_the_idle_timer() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let done = serve_in(&temp, IDLE, Arc::clone(&clock));
    assert!(clock.await_parked(clock.origin() + IDLE, WITHIN));
    let (first, _) = connect(&temp);
    clock.advance(IDLE + Duration::from_millis(1));
    assert!(
        done.recv_timeout(Duration::from_millis(200)).is_err(),
        "the first client keeps the hub"
    );
    drop(first);
    let (second, _) = connect(&temp);
    // The second client arrived after the first left: passing another
    // idle period still exits nothing.
    clock.advance(IDLE + Duration::from_millis(1));
    assert!(
        done.recv_timeout(Duration::from_millis(200)).is_err(),
        "a reconnect resets the timer"
    );
    drop(second);
    let mut code = None;
    for _ in 0..10 {
        clock.advance(IDLE);
        if let Ok(exit) = done.recv_timeout(Duration::from_millis(200)) {
            code = Some(exit);
            break;
        }
    }
    assert_eq!(code, Some(0));
}

#[test]
fn a_signal_stops_the_hub_with_its_code() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn contract::clock::Clock> = timed;
    let hub = Arc::new(Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(FakeStarter::hang(&temp.dir)),
        timed,
    ));
    let held = crate::listen::listen(&temp.dir).unwrap().unwrap();
    let got = Arc::new(AtomicI32::new(0));
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-run".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let got = Arc::clone(&got);
            let timed: Arc<dyn contract::clock::Clock> = clock.clone();
            move || {
                let exit = run(&hub, &held, IDLE, &timed, &got);
                held.stop();
                done_tx.send(exit.code()).unwrap_or(());
            }
        })
        .unwrap();
    assert!(clock.await_parked(clock.origin() + IDLE, WITHIN));
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    clock.advance(Duration::from_millis(1));
    assert_eq!(done_rx.recv_timeout(DEADLINE).expect("the hub stops"), 143);
    assert!(!temp.socket().exists());
    assert!(temp.log().contains("The hub stopped: signal."));
}

#[test]
fn zero_idle_exit_stops_at_the_first_empty_wait() {
    let temp = Temp::new();
    let done = serve_in(
        &temp,
        Duration::ZERO,
        Arc::clone(&fakes::clock::FakeClock::new()),
    );
    assert_eq!(
        done.recv_timeout(DEADLINE).expect("the hub exits at once"),
        0
    );
}
