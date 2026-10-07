//! Tests for idle exit and signal shutdown. Every fake-clock advance follows
//! proof of the park the hub chose: `[None]` while a client is open, or
//! `[Some(t0 + idle_exit)]` once the count reached 0 at `t0`.

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
use std::time::{Duration, Instant};

use contract::clock::Clock;
use log::diag::Level;

use super::*;
use crate::connection::Hub;
use crate::diag::Diag;
use crate::fake::FakeStarter;
use crate::listen::Held;

/// One named deadline per wait: the hub answers before it.
const DEADLINE: Duration = Duration::from_secs(5);

/// How long `await_parked` waits for the hub to park, in wall time.
const WITHIN: Duration = Duration::from_secs(5);

/// Thirty idle minutes, the documented default.
const IDLE: Duration = Duration::from_millis(1_800_000);

/// The smallest step of fake time.
const TICK: Duration = Duration::from_nanos(1);

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
    serve_at(temp, idle, clock, Level::Info)
}

/// [`serve_in`] at a diagnostic level.
fn serve_at(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
    level: Level,
) -> mpsc::Receiver<i32> {
    let home = temp.dir.clone();
    let timed: Arc<dyn contract::clock::Clock> = clock;
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-serve".to_owned())
        .spawn(move || {
            let code = crate::serve(
                &home,
                move || {
                    Ok(crate::Settings {
                        idle_exit: idle,
                        level,
                    })
                },
                "0.0.0",
                Arc::new(FakeStarter::hang(&home)),
                timed,
            )
            .expect("the hub starts");
            done_tx.send(code).unwrap_or(());
        })
        .unwrap();
    done_rx
}

/// Serves the hub on a thread with its handle and its signal flag.
fn serve_with_hub(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
) -> (Arc<Hub>, Arc<AtomicI32>, mpsc::Receiver<i32>) {
    serve_with_hub_at(temp, idle, clock, Level::Info)
}

/// [`serve_with_hub`] at a diagnostic level.
fn serve_with_hub_at(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
    level: Level,
) -> (Arc<Hub>, Arc<AtomicI32>, mpsc::Receiver<i32>) {
    serve_with_hub_at_peak(temp, idle, clock, level, log::diag::peak_kib)
}

/// [`serve_with_hub_at`] reading the peak memory with `peak`. Tests use
/// it to prove the stop lines hold the log's lock while they read.
fn serve_with_hub_at_peak(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
    level: Level,
    peak: fn() -> Option<u64>,
) -> (Arc<Hub>, Arc<AtomicI32>, mpsc::Receiver<i32>) {
    let timed: Arc<dyn contract::clock::Clock> = clock;
    let hub = Arc::new(Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(FakeStarter::hang(&temp.dir)),
        Arc::clone(&timed),
        Diag::open(&temp.dir, timed)
            .with_level(level)
            .with_peak(peak),
    ));
    hub.diag.info("hub_started", "The hub started.");
    let lock = crate::listen::lock(&temp.dir).unwrap().unwrap();
    let bound = crate::listen::bind(&lock, &temp.dir).unwrap().unwrap();
    let held = Held::new(lock, bound);
    let got = Arc::new(AtomicI32::new(0));
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-run".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let got = Arc::clone(&got);
            move || {
                let exit = run(&hub, &held, idle, &got);
                held.stop();
                done_tx.send(exit.code()).unwrap_or(());
            }
        })
        .unwrap();
    (hub, got, done_rx)
}

/// Waits, at most `WITHIN`, until the hub's only park has no deadline: it
/// saw a client open. Panics past the deadline naming the wait.
fn await_open_park(clock: &Arc<fakes::clock::FakeClock>, what: &str) {
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-parked".to_owned())
        .spawn({
            let clock = Arc::clone(clock);
            move || {
                while clock.parked() != [None] {
                    thread::yield_now();
                }
                done_tx.send(()).unwrap_or(());
            }
        })
        .unwrap();
    done_rx
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the hub parks with no deadline {what}"));
}

/// Waits, at most `WITHIN`, until the hub parks until `zero + IDLE`: it saw
/// the count reach 0 at `zero`. Panics past the deadline naming the wait.
fn await_idle_park(clock: &Arc<fakes::clock::FakeClock>, zero: Instant, what: &str) {
    assert!(
        clock.await_parked(zero + IDLE, WITHIN),
        "the hub parks until idle_exit after the count reached 0 {what}"
    );
}

/// A client on the hub's socket: connected once its `hub_hello` arrives.
struct Client {
    _stream: UnixStream,
    read: BufReader<UnixStream>,
}

/// Connects and reads the first line: `hub_hello` proves the hub counted
/// the connection.
fn connect(temp: &Temp) -> Client {
    let stream = UnixStream::connect(temp.socket()).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut read = BufReader::new(stream.try_clone().unwrap());
    let mut hello = String::new();
    read.read_line(&mut hello).expect("the hub speaks first");
    assert!(
        hello.contains("\"kind\":\"hub_hello\""),
        "the hub answers with hub_hello: {hello:?}"
    );
    Client {
        _stream: stream,
        read,
    }
}

#[test]
fn with_no_clients_the_hub_exits_at_idle_exit() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let done = serve_in(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    // Exactly the deadline: the exit is due at, not after, it.
    clock.advance(IDLE);
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub exits idle"), 0);
    assert!(!temp.socket().exists());
    let log = temp.log();
    assert!(log.contains("\"code\":\"hub_started\""));
    assert!(log.contains("\"code\":\"hub_stopped\""));
    assert!(log.contains("The hub stopped: idle."));
    assert!(!log.contains("\"level\":\"debug\""), "{log}");
}

/// The codes of the last two lines of `log`, and the peak of the first.
fn last_two(log: &str) -> (String, String, u64) {
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let [.., before, last] = lines.as_slice() else {
        panic!("fewer than two lines: {log}");
    };
    (
        before["code"].as_str().unwrap().to_owned(),
        last["code"].as_str().unwrap().to_owned(),
        before["data"]["peak_kib"].as_u64().unwrap_or(0),
    )
}

#[test]
fn a_debug_hub_writes_peak_memory_just_before_its_idle_stop() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let done = serve_at(&temp, IDLE, Arc::clone(&clock), Level::Debug);
    await_idle_park(&clock, clock.origin(), "at start");
    clock.advance(IDLE);
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub exits idle"), 0);
    let (before, last, peak) = last_two(&temp.log());
    assert_eq!(
        (before.as_str(), last.as_str()),
        ("peak_memory", "hub_stopped")
    );
    assert!(peak > 0);
}

#[test]
fn a_hub_whose_socket_was_removed_still_exits_at_idle_exit() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let done = serve_in(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    fs::remove_file(temp.socket()).unwrap();
    clock.advance(IDLE);
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub exits idle"), 0);
    assert!(temp.log().contains("The hub stopped: idle."));
}

#[test]
fn one_tick_before_idle_exit_the_hub_still_answers() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (_hub, _got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    clock.advance(IDLE.checked_sub(TICK).unwrap());
    // A hub that had exited answers EOF or refuses: the hello proves it
    // still serves.
    let client = connect(&temp);
    await_open_park(&clock, "after the arrival");
    drop(client);
    let zero = clock.now();
    await_idle_park(&clock, zero, "after the departure");
    clock.advance(IDLE);
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub exits idle"), 0);
}

#[test]
fn a_connected_client_keeps_the_hub_and_leaving_starts_the_timer() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (_hub, _got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    let client = connect(&temp);
    await_open_park(&clock, "after the arrival");
    // Far past any deadline: an open client has none.
    clock.advance(IDLE * 100);
    drop(client);
    // The timer starts at the departure, so this park also proves the hub
    // outlived the advance.
    let zero = clock.now();
    await_idle_park(&clock, zero, "after the departure");
    clock.advance(IDLE);
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub exits idle"), 0);
    assert!(!temp.socket().exists());
}

#[test]
fn a_reconnect_clears_the_timer_and_a_later_departure_restarts_it() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (_hub, _got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    let first = connect(&temp);
    await_open_park(&clock, "after the first arrival");
    clock.advance(IDLE);
    drop(first);
    let first_zero = clock.now();
    await_idle_park(&clock, first_zero, "after the first departure");
    clock.advance(IDLE.checked_sub(TICK).unwrap());
    // Within the window: the hello proves the hub still serves.
    let second = connect(&temp);
    await_open_park(&clock, "after the second arrival");
    // Past the first departure's deadline: the reconnect cleared it.
    clock.advance(IDLE);
    drop(second);
    let second_zero = clock.now();
    await_idle_park(&clock, second_zero, "after the second departure");
    clock.advance(IDLE);
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub exits idle"), 0);
    assert!(!temp.socket().exists());
}

#[test]
fn a_spawn_failure_rollback_restarts_the_timer() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (hub, _got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    // What the accept loop does: count, then roll back when the serving
    // thread fails to spawn.
    let (counted, _peer) = UnixStream::pair().unwrap();
    let n = hub.register(&counted).expect("the clone succeeds");
    await_open_park(&clock, "after the count");
    clock.advance(IDLE);
    hub.rollback(n);
    let zero = clock.now();
    await_idle_park(&clock, zero, "after the rollback");
    clock.advance(IDLE);
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub exits idle"), 0);
    assert!(!temp.log().contains("client_disconnected"));
}

#[test]
fn an_accept_after_the_exit_claim_gets_eof_without_hello() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let hub = Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(FakeStarter::hang(&temp.dir)),
        Arc::clone(&timed),
        Diag::open(&temp.dir, timed),
    );
    // No thread waits on the clock: the idle wait below runs on this one.
    clock.advance(IDLE);
    let stop = AtomicBool::new(false);
    assert!(matches!(
        hub.idle_wait(IDLE, &stop, &AtomicI32::new(0)),
        crate::connection::Idle::Expired
    ));
    assert!(stop.load(Ordering::SeqCst), "the claim sets stop");
    // The accept loop sees the claim: the stream is dropped unanswered,
    // and the client retries.
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
fn a_signal_stops_the_hub_with_its_code() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (hub, got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    // What the signal arm does: record, then wake.
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub.waker().wake();
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub stops"), 143);
    assert!(!temp.socket().exists());
    assert!(temp.log().contains("The hub stopped: signal."));
    assert!(!temp.log().contains("peak_memory"));
}

#[test]
fn a_debug_hub_writes_peak_memory_just_before_its_signal_stop() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (hub, got, done) = serve_with_hub_at(&temp, IDLE, Arc::clone(&clock), Level::Debug);
    await_idle_park(&clock, clock.origin(), "at start");
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub.waker().wake();
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub stops"), 143);
    let (before, last, peak) = last_two(&temp.log());
    assert_eq!(
        (before.as_str(), last.as_str()),
        ("peak_memory", "hub_stopped")
    );
    assert!(peak > 0);
}

#[test]
fn a_signal_shuts_down_connected_clients() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (hub, got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    let mut client = connect(&temp);
    // An open client parks with no deadline: only the wake ends it.
    await_open_park(&clock, "after the arrival");
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub.waker().wake();
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub stops"), 143);
    // The shutdown closed the hub's end: EOF, and nothing after the hello.
    let mut tail = String::new();
    assert_eq!(
        client
            .read
            .read_line(&mut tail)
            .expect("the shutdown closes the socket"),
        0,
        "the client sees EOF after the hello"
    );
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

/// An acceptor thread that returns after one `accept`, with a flag set as it
/// ends, on a listener at `socket`.
fn one_accept_thread(socket: &std::path::Path) -> (thread::JoinHandle<()>, Arc<AtomicBool>) {
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    let ended = Arc::new(AtomicBool::new(false));
    let handle = thread::Builder::new()
        .name("hub-test-acceptor".to_owned())
        .spawn({
            let ended = Arc::clone(&ended);
            move || {
                listener.accept().map(drop).unwrap_or(());
                ended.store(true, Ordering::SeqCst);
            }
        })
        .unwrap();
    (handle, ended)
}

#[test]
fn join_after_wake_unblocks_and_joins_a_blocked_acceptor() {
    let temp = Temp::new();
    let socket = temp.dir.join("wake");
    let (acceptor, ended) = one_accept_thread(&socket);
    let (done_tx, done_rx) = mpsc::channel();
    let wake_socket = socket.clone();
    thread::Builder::new()
        .name("hub-test-wake".to_owned())
        .spawn(move || {
            join_after_wake(&wake_socket, acceptor);
            done_tx.send(()).unwrap_or(());
        })
        .unwrap();
    done_rx
        .recv_timeout(DEADLINE)
        .expect("the wake joins the acceptor");
    assert!(
        ended.load(Ordering::SeqCst),
        "the acceptor ended before the join returned"
    );
}

#[test]
fn join_after_wake_returns_when_the_socket_path_is_gone() {
    let temp = Temp::new();
    let socket = temp.dir.join("wake");
    let (acceptor, ended) = one_accept_thread(&socket);
    fs::remove_file(&socket).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-wake".to_owned())
        .spawn(move || {
            join_after_wake(&socket, acceptor);
            done_tx.send(()).unwrap_or(());
        })
        .unwrap();
    done_rx
        .recv_timeout(DEADLINE)
        .expect("the wake returns without a join");
    assert!(!ended.load(Ordering::SeqCst), "nothing woke the acceptor");
}

/// The gate the forced-stop test and its peak reader coordinate
/// through: the reader runs while the shutdown's stop lines hold the
/// log's lock, so its `inside` signal proves the pair holds the lock;
/// it then waits for the release, which the test sends only after a
/// departing client's write has started. One shot: the reader takes it.
struct StopGate {
    inside_tx: mpsc::Sender<()>,
    release_rx: mpsc::Receiver<()>,
}

static STOP_GATE: std::sync::Mutex<Option<StopGate>> = std::sync::Mutex::new(None);

/// The peak reader for the forced-stop test: proves the stop lines hold
/// the log's lock, then waits for the release with a wall-clock deadline.
fn gated_peak() -> Option<u64> {
    let gate = STOP_GATE
        .lock()
        .unwrap()
        .take()
        .expect("the stop test installs its gate");
    gate.inside_tx.send(()).unwrap();
    gate.release_rx.recv_timeout(DEADLINE).unwrap();
    Some(9)
}

#[test]
fn a_signal_stop_keeps_peak_memory_next_to_hub_stopped_during_disconnects() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (inside_tx, inside_rx) = mpsc::channel::<()>();
    let (attempt_tx, attempt_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    *STOP_GATE.lock().unwrap() = Some(StopGate {
        inside_tx,
        release_rx,
    });
    let (hub, got, done) =
        serve_with_hub_at_peak(&temp, IDLE, Arc::clone(&clock), Level::Debug, gated_peak);
    await_idle_park(&clock, clock.origin(), "at start");
    let client = connect(&temp);
    await_open_park(&clock, "after the arrival");
    // The signal shutdown's stop lines take the log's lock, and the
    // gated reader proves it, then waits (`docs/testing.md`, "Races are
    // forced, not waited for"). The test then starts a departing
    // client's write, whose signal goes out just before its `line`
    // call, and only then releases the pair: the rival's line must land
    // after `hub_stopped`. Every wait has a wall-clock deadline and
    // nothing sleeps.
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub.waker().wake();
    inside_rx
        .recv_timeout(DEADLINE)
        .expect("the shutdown reaches its stop lines");
    let rival = thread::Builder::new()
        .name("hub-test-disconnect".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            move || {
                attempt_tx.send(()).unwrap();
                hub.diag
                    .info("client_disconnected", "Client 7 disconnected.");
            }
        })
        .unwrap();
    attempt_rx
        .recv_timeout(DEADLINE)
        .expect("the disconnect starts its write");
    release_tx.send(()).unwrap();
    assert_eq!(done.recv_timeout(DEADLINE).expect("the hub stops"), 143);
    rival.join().unwrap();
    drop(client);
    let lines: Vec<serde_json::Value> = temp
        .log()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let at = lines
        .iter()
        .position(|line| line["code"] == "hub_stopped")
        .expect("the hub logs its stop");
    assert!(at > 0, "a line comes before the stop");
    assert_eq!(lines[at - 1]["code"], "peak_memory");
    assert_eq!(lines[at - 1]["data"]["peak_kib"], 9);
    let rival_at = lines
        .iter()
        .position(|line| line["message"] == "Client 7 disconnected.")
        .expect("the rival disconnect is logged");
    assert!(
        rival_at > at,
        "the competing disconnect lands after the stop"
    );
}
