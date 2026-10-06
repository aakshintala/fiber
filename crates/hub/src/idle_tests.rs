//! Tests for idle exit and signal shutdown: the hub parks with no client
//! connected, leaves while one is, resets on arrival, and stops on signal.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
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

/// Serves the hub on a thread with its handle: the caller waits on
/// activity before each advance, so every advance follows a park that began
/// after the event under test.
fn serve_with_hub(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
) -> (Arc<Hub>, mpsc::Receiver<i32>) {
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn contract::clock::Clock> = timed;
    let hub = Arc::new(Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(FakeStarter::hang(&temp.dir)),
        timed,
    ));
    hub.diag.info("hub_started", "The hub started.");
    let held = crate::listen::listen(&temp.dir).unwrap().unwrap();
    let got = Arc::new(AtomicI32::new(0));
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-run".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let timed = Arc::clone(&clock);
            let timed: Arc<dyn contract::clock::Clock> = timed;
            move || {
                let exit = run(&hub, &held, idle, &timed, &got);
                held.stop();
                done_tx.send(exit.code()).unwrap_or(());
            }
        })
        .unwrap();
    (hub, done_rx)
}

/// Waits, at most one deadline, for the hub's activity to reach `expected`.
fn until_activity(hub: &Arc<Hub>, expected: u64, what: &str) {
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-activity".to_owned())
        .spawn({
            let hub = Arc::clone(hub);
            move || {
                while hub.activity() != expected {
                    thread::yield_now();
                }
                done_tx.send(()).unwrap_or(());
            }
        })
        .unwrap();
    done_rx
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("the hub reaches activity {expected} {what}"));
}

/// A client on the hub's socket: connected once its `hub_hello` arrives.
struct Client {
    _stream: UnixStream,
}

/// The instant the hub currently waits on: advancing past it is observed.
/// Panics past the deadline naming the wait. Each use follows proof that
/// the hub parked after the event under test (activity), so the returned
/// park began after it: for a quiet hub the deadline is unchanged, for a
/// connected one it moved, but either way the park is current.
fn parked_until(clock: &Arc<fakes::clock::FakeClock>) -> std::time::Instant {
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-parked".to_owned())
        .spawn({
            let clock = Arc::clone(clock);
            move || {
                loop {
                    if let [Some(until)] = clock.parked().as_slice() {
                        done_tx.send(*until).unwrap_or(());
                        return;
                    }
                    thread::yield_now();
                }
            }
        })
        .unwrap();
    done_rx
        .recv_timeout(WITHIN)
        .expect("the hub parks before its deadline")
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
    let (hub, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    assert!(clock.await_parked(clock.origin() + IDLE, WITHIN));
    let (client, hello) = connect(&temp);
    assert!(hello.contains("\"kind\":\"hub_hello\""));
    // The arrival is counted before any advance: otherwise the advance
    // could spend the idle period before the count lands and exit spuriously.
    until_activity(&hub, 1, "after the arrival");
    // The re-park has the same deadline while the clock is frozen, so force
    // a new one: prove the old park, move a millisecond, then prove the
    // current park that began after the arrival before spending idle time.
    assert!(clock.await_parked(clock.now() + IDLE, WITHIN));
    clock.advance(Duration::from_millis(1));
    parked_until(&clock);
    // The client is connected: a whole idle period passes without exit.
    clock.advance(IDLE + Duration::from_millis(1));
    assert!(
        done.recv_timeout(Duration::from_millis(200)).is_err(),
        "a connected client keeps the hub"
    );
    drop(client);
    // The departure is counted before any advance. A quiet hub keeps its
    // deadline across the millisecond, so prove the current park after it
    // rather than a computed one, then spend one period past it.
    until_activity(&hub, 2, "after the departure");
    assert!(clock.await_parked(clock.now() + IDLE, WITHIN));
    clock.advance(Duration::from_millis(1));
    parked_until(&clock);
    clock.advance(IDLE + Duration::from_millis(1));
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub exits idle"), 0);
    assert!(!temp.socket().exists());
}

#[test]
fn a_reconnect_resets_the_idle_timer() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (hub, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    assert!(clock.await_parked(clock.origin() + IDLE, WITHIN));
    let (first, _) = connect(&temp);
    until_activity(&hub, 1, "after the first arrival");
    assert!(clock.await_parked(clock.now() + IDLE, WITHIN));
    clock.advance(Duration::from_millis(1));
    parked_until(&clock);
    clock.advance(IDLE + Duration::from_millis(1));
    assert!(
        done.recv_timeout(Duration::from_millis(200)).is_err(),
        "the first client keeps the hub"
    );
    drop(first);
    until_activity(&hub, 2, "after the first departure");
    let (second, _) = connect(&temp);
    until_activity(&hub, 3, "after the second arrival");
    // The second arrival began after the first departure: force a new park
    // after it before spending another idle period.
    assert!(clock.await_parked(clock.now() + IDLE, WITHIN));
    clock.advance(Duration::from_millis(1));
    parked_until(&clock);
    // The second client arrived after the first left: passing another
    // idle period still exits nothing.
    clock.advance(IDLE + Duration::from_millis(1));
    assert!(
        done.recv_timeout(Duration::from_millis(200)).is_err(),
        "a reconnect resets the timer"
    );
    drop(second);
    until_activity(&hub, 4, "after the second departure");
    assert!(clock.await_parked(clock.now() + IDLE, WITHIN));
    clock.advance(Duration::from_millis(1));
    parked_until(&clock);
    clock.advance(IDLE + Duration::from_millis(1));
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub exits idle"), 0);
    assert!(!temp.socket().exists());
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
fn a_signal_shuts_down_connected_clients() {
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
    let stream = UnixStream::connect(temp.socket()).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut read = BufReader::new(stream.try_clone().unwrap());
    let mut hello = String::new();
    read.read_line(&mut hello).expect("the hub speaks first");
    assert!(hello.contains("\"kind\":\"hub_hello\""));
    // The hello proves the arrival was counted: force a new park after it
    // before waking for the signal, so the advance is observed.
    until_activity(&hub, 1, "after the arrival");
    assert!(clock.await_parked(clock.now() + IDLE, WITHIN));
    clock.advance(Duration::from_millis(1));
    parked_until(&clock);
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    clock.advance(Duration::from_millis(1));
    assert_eq!(done_rx.recv_timeout(DEADLINE).expect("the hub stops"), 143);
    // The shutdown closed the hub's end: EOF, and nothing after the hello.
    let mut tail = String::new();
    assert_eq!(
        read.read_line(&mut tail)
            .expect("the shutdown closes the socket"),
        0,
        "the client sees EOF after the hello"
    );
}

#[test]
fn a_connection_accepted_after_the_exit_decision_gets_eof_without_hello() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let timed: Arc<dyn contract::clock::Clock> = clock;
    let hub = Arc::new(Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(FakeStarter::hang(&temp.dir)),
        timed,
    ));
    // The exit decision already ran: the accept loop drops the stream
    // unanswered, and the client retries.
    let stop = AtomicBool::new(true);
    let (served, mut client) = UnixStream::pair().unwrap();
    client.set_read_timeout(Some(DEADLINE)).unwrap();
    assert!(matches!(
        hub.poll_accept(&served, &stop),
        crate::connection::Accept::Exiting
    ));
    drop(served);
    let mut got = Vec::new();
    assert_eq!(client.read_to_end(&mut got).expect("EOF, not a hello"), 0);
    assert!(got.is_empty());
    assert_eq!(hub.clients(), 0);
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
