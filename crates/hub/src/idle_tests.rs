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
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use log::diag::{Level, Severity};
use serde_json::{Value, json};

use super::*;
use crate::connection::Hub;
use crate::fake::FakeStarter;
use crate::listen::Held;
use fakes::Deadline;

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
        let (held, dir) = crate::testkit::home("hi");
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
) -> (mpsc::Receiver<i32>, thread::ThreadId) {
    serve_at(temp, idle, clock, Level::Info)
}

/// [`serve_in`] at a diagnostic level.
fn serve_at(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
    level: Level,
) -> (mpsc::Receiver<i32>, thread::ThreadId) {
    serve_mode(temp, idle, clock, level, crate::Mode::OnDemand)
}

/// [`serve_at`] in `mode`.
fn serve_mode(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
    level: Level,
    mode: crate::Mode,
) -> (mpsc::Receiver<i32>, thread::ThreadId) {
    let home = temp.dir.clone();
    let timed: Arc<dyn contract::clock::Clock> = clock;
    let (done_tx, done_rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("hub-test-serve".to_owned())
        .spawn(move || {
            let code = crate::serve(
                &home,
                mode,
                move || {
                    Ok(crate::Settings {
                        idle_exit: idle,
                        level,
                        port: None,
                    })
                },
                "0.0.0",
                None,
                Arc::new(FakeStarter::hang(&home)),
                timed,
                crate::Environments::ready(crate::Environment {
                    vars: Vec::new(),
                    source: contract::events::VariablesSource::Inherited,
                }),
                Arc::new(crate::fake::FakeMachine::default()),
            )
            .expect("the hub starts");
            done_tx.send(code).unwrap_or(());
        })
        .unwrap();
    let serve_id = handle.thread().id();
    (done_rx, serve_id)
}

/// [`serve_mode`] with the hub's shared environment.
fn serve_mode_env(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
    level: Level,
    mode: crate::Mode,
    environments: Arc<crate::Environments>,
) -> (mpsc::Receiver<i32>, thread::ThreadId) {
    let home = temp.dir.clone();
    let timed: Arc<dyn contract::clock::Clock> = clock;
    let (done_tx, done_rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("hub-test-serve".to_owned())
        .spawn(move || {
            let code = crate::serve(
                &home,
                mode,
                move || {
                    Ok(crate::Settings {
                        idle_exit: idle,
                        level,
                        port: None,
                    })
                },
                "0.0.0",
                None,
                Arc::new(FakeStarter::hang(&home)),
                timed,
                environments,
                Arc::new(crate::fake::FakeMachine::default()),
            )
            .expect("the hub starts");
            done_tx.send(code).unwrap_or(());
        })
        .unwrap();
    let serve_id = handle.thread().id();
    (done_rx, serve_id)
}

/// Serves the hub on a thread with its handle and its signal flag.
fn serve_with_hub(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
) -> (Arc<Hub>, Arc<AtomicI32>, mpsc::Receiver<i32>) {
    serve_with_hub_and_starter(temp, idle, clock, FakeStarter::hang(&temp.dir))
}

/// [`serve_with_hub`] with the test's starter: sessions-stop tests run
/// real fake sessions instead of the hanging starter.
fn serve_with_hub_and_starter(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
    starter: FakeStarter,
) -> (Arc<Hub>, Arc<AtomicI32>, mpsc::Receiver<i32>) {
    serve_with_hub_at_and_starter(temp, idle, clock, Level::Info, starter)
}

/// [`serve_with_hub`] at a diagnostic level.
fn serve_with_hub_at(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
    level: Level,
) -> (Arc<Hub>, Arc<AtomicI32>, mpsc::Receiver<i32>) {
    let idle_seam: Arc<dyn Fn(bool) + Send + Sync> = Arc::new(|_| {});
    serve_with_hub_at_peak_between_and_starter(
        temp,
        idle,
        clock,
        level,
        log::diag::peak_kib,
        idle_seam,
        FakeStarter::hang(&temp.dir),
    )
}

/// [`serve_with_hub_at`] with the test's starter.
fn serve_with_hub_at_and_starter(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
    level: Level,
    starter: FakeStarter,
) -> (Arc<Hub>, Arc<AtomicI32>, mpsc::Receiver<i32>) {
    let idle_seam: Arc<dyn Fn(bool) + Send + Sync> = Arc::new(|_| {});
    serve_with_hub_at_peak_between_and_starter(
        temp,
        idle,
        clock,
        level,
        log::diag::peak_kib,
        idle_seam,
        starter,
    )
}

/// [`serve_with_hub_at`] reading the peak memory with `peak` and running
/// `between` between the stop lines' two appends. Tests use them to
/// force the stop-pair race with a deterministic `peak_memory` value.
fn serve_with_hub_at_peak_between_and_starter(
    temp: &Temp,
    idle: Duration,
    clock: Arc<fakes::clock::FakeClock>,
    level: Level,
    peak: fn() -> Option<u64>,
    between: Arc<dyn Fn(bool) + Send + Sync>,
    starter: FakeStarter,
) -> (Arc<Hub>, Arc<AtomicI32>, mpsc::Receiver<i32>) {
    // Keeps its own hub: the diag carries the test's level, peak reader and between hook.
    let timed: Arc<dyn contract::clock::Clock> = clock;
    let hub = Arc::new(Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(starter),
        Arc::clone(&timed),
        crate::diag::open(&temp.dir, timed)
            .with_level(level)
            .with_peak(peak)
            .with_between(between),
    ));
    hub.diag
        .line(Severity::Info, None, "hub_started", "The hub started.");
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
#[track_caller]
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
    Deadline::after(WITHIN)
        .recv(&done_rx)
        .unwrap_or_else(|_| panic!("the hub parks with no deadline {what}"));
}

/// Waits, at most `WITHIN`, until the hub parks until `zero + IDLE`: it saw
/// the count reach 0 at `zero`. Panics past the deadline naming the wait.
#[track_caller]
fn await_idle_park(clock: &Arc<fakes::clock::FakeClock>, zero: Instant, what: &str) {
    assert!(
        clock.await_parked(zero + IDLE, fakes::Deadline::after(WITHIN)),
        "the hub parks until idle_exit after the count reached 0 {what}"
    );
}

/// A client on the hub's socket: connected once its `hub_hello` arrives.
struct Client {
    stream: UnixStream,
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
        stream: stream.try_clone().unwrap(),
        read,
    }
}

impl Client {
    /// Sends one command line.
    fn send(&mut self, line: &Value) {
        let mut bytes = serde_json::to_vec(line).unwrap();
        bytes.push(b'\n');
        self.stream.write_all(&bytes).unwrap();
        self.stream.flush().unwrap();
    }

    /// The next line, parsed, or `None` at EOF.
    fn next(&mut self, what: &str) -> Option<Value> {
        let mut text = String::new();
        match self.read.read_line(&mut text) {
            Ok(0) => None,
            Ok(_) => Some(serde_json::from_str(&text).unwrap()),
            Err(error) => panic!("the hub never answers {what}: {error:?}"),
        }
    }
}

#[test]
fn with_no_clients_the_hub_exits_at_idle_exit() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (done, serve_id) = serve_in(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    // Exactly the deadline: the exit is due at, not after, it.
    clock.advance_when_parked(
        Some(clock.origin() + IDLE),
        Some(serve_id),
        IDLE,
        &Deadline::after(WITHIN),
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits idle"),
        0
    );
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
    let (done, serve_id) = serve_at(&temp, IDLE, Arc::clone(&clock), Level::Debug);
    await_idle_park(&clock, clock.origin(), "at start");
    clock.advance_when_parked(
        Some(clock.origin() + IDLE),
        Some(serve_id),
        IDLE,
        &Deadline::after(WITHIN),
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits idle"),
        0
    );
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
    let (done, serve_id) = serve_in(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    fs::remove_file(temp.socket()).unwrap();
    clock.advance_when_parked(
        Some(clock.origin() + IDLE),
        Some(serve_id),
        IDLE,
        &Deadline::after(WITHIN),
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits idle"),
        0
    );
    assert!(temp.log().contains("The hub stopped: idle."));
}

#[test]
fn one_tick_before_idle_exit_the_hub_still_answers() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (_hub, _got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    clock.advance_when_parked(
        Some(clock.origin() + IDLE),
        None,
        IDLE.checked_sub(TICK).unwrap(),
        &Deadline::after(WITHIN),
    );
    // A hub that had exited answers EOF or refuses: the hello proves it
    // still serves.
    let client = connect(&temp);
    await_open_park(&clock, "after the arrival");
    drop(client);
    let zero = clock.now();
    await_idle_park(&clock, zero, "after the departure");
    clock.advance_when_parked(Some(zero + IDLE), None, IDLE, &Deadline::after(WITHIN));
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits idle"),
        0
    );
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
    clock.advance_when_parked(None, None, IDLE * 100, &Deadline::after(WITHIN));
    drop(client);
    // The timer starts at the departure, so this park also proves the hub
    // outlived the advance.
    let zero = clock.now();
    await_idle_park(&clock, zero, "after the departure");
    clock.advance_when_parked(Some(zero + IDLE), None, IDLE, &Deadline::after(WITHIN));
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits idle"),
        0
    );
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
    clock.advance_when_parked(None, None, IDLE, &Deadline::after(WITHIN));
    drop(first);
    let first_zero = clock.now();
    await_idle_park(&clock, first_zero, "after the first departure");
    clock.advance_when_parked(
        Some(first_zero + IDLE),
        None,
        IDLE.checked_sub(TICK).unwrap(),
        &Deadline::after(WITHIN),
    );
    // Within the window: the hello proves the hub still serves.
    let second = connect(&temp);
    await_open_park(&clock, "after the second arrival");
    // Past the first departure's deadline: the reconnect cleared it.
    clock.advance_when_parked(None, None, IDLE, &Deadline::after(WITHIN));
    drop(second);
    let second_zero = clock.now();
    await_idle_park(&clock, second_zero, "after the second departure");
    clock.advance_when_parked(
        Some(second_zero + IDLE),
        None,
        IDLE,
        &Deadline::after(WITHIN),
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits idle"),
        0
    );
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
    clock.advance_when_parked(None, None, IDLE, &Deadline::after(WITHIN));
    hub.rollback(n);
    let zero = clock.now();
    await_idle_park(&clock, zero, "after the rollback");
    clock.advance_when_parked(Some(zero + IDLE), None, IDLE, &Deadline::after(WITHIN));
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits idle"),
        0
    );
    assert!(!temp.log().contains("client_disconnected"));
}

#[test]
fn a_start_up_park_still_standing_does_not_prove_the_departure() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let origin = clock.origin();
    let (hub, _got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, origin, "at start");
    // Hold the tick lock so the notified idle thread cannot leave its
    // start-up park: the advance and the arrival below both block in
    // `Tick::wake` until the guard drops.
    let held = hub.tick.hold();
    // Thread A moves fake time to the idle exit. Its wake finds the
    // guard held and reports through the armed attempt at once, before
    // it blocks on the guard: the report proves the offset already moved.
    let (attempt_tx, attempt_rx) = mpsc::channel();
    *support::lock(&hub.tick.attempt) = Some(attempt_tx);
    let (a_tx, a_done) = mpsc::channel();
    let advance = thread::spawn({
        let clock = Arc::clone(&clock);
        move || {
            clock.advance_when_parked(Some(origin + IDLE), None, IDLE, &Deadline::after(WITHIN));
            a_tx.send(()).unwrap_or(());
        }
    });
    Deadline::after(DEADLINE).recv_or_fail(&attempt_rx, "the advance to reach its wake");
    // Thread B counts a client through a fresh arm. Counting clears
    // `zero_since` before the wake, which blocks on the guard the same
    // way: the report proves the count is in place.
    let (attempt_tx, attempt_rx) = mpsc::channel();
    *support::lock(&hub.tick.attempt) = Some(attempt_tx);
    let (counted, peer) = UnixStream::pair().unwrap();
    let (b_tx, b_done) = mpsc::channel();
    let arrival = thread::spawn({
        let hub = Arc::clone(&hub);
        move || {
            let n = hub.register(&counted);
            b_tx.send(n).unwrap_or(());
        }
    });
    Deadline::after(DEADLINE).recv_or_fail(&attempt_rx, "the arrival to reach its wake");
    // The stale state, read with no waits: a client is counted, yet the
    // only park is still the start-up one.
    assert_eq!(hub.clients(), 1);
    assert_eq!(clock.parked(), vec![Some(origin + IDLE)]);
    // The old gate: the stale park still satisfies the instant wait even
    // with a client counted, so advancing on it races the arrival.
    assert!(
        clock.await_parked(origin + IDLE, fakes::Deadline::after(WITHIN)),
        "the stale start-up park still satisfies the instant gate with a client counted"
    );
    // The new gate: no unbounded park exists while the guard is held. A
    // zero-bound probe reads the predicate with no elapsed-time wait:
    // the stale finite park alone cannot satisfy it.
    assert!(
        !clock.await_parked_unbounded(fakes::Deadline::after(Duration::ZERO)),
        "the stale park answers no unbounded wait while the guard is held"
    );
    // Release both wakes and the idle thread: it leaves the stale park,
    // rechecks under the tick lock with the count and the later time in
    // place, finds a client open, and parks with no deadline.
    drop(held);
    Deadline::after(DEADLINE).recv_or_fail(&a_done, "the advance to finish its wake");
    let n = Deadline::after(DEADLINE)
        .recv_or_fail(&b_done, "the arrival to finish its wake")
        .expect("the clone succeeds");
    advance.join().expect("the advance thread joins");
    arrival.join().expect("the arrival thread joins");
    assert!(
        clock.await_parked_unbounded(fakes::Deadline::after(WITHIN)),
        "the hub parks with no deadline once it saw the client"
    );
    // The departure starts the timer at the advanced time; the advance
    // that raced the arrival ends nothing while fake time stays frozen.
    hub.rollback(n);
    let zero = clock.now();
    assert_eq!(zero, origin + IDLE);
    await_idle_park(&clock, zero, "after the rollback");
    // The idle park above already proves the hub is still waiting: it has
    // not exited, so no code is waiting on its result.
    assert!(
        done.try_recv().is_err(),
        "the advance that raced the arrival does not end the hub"
    );
    // Driving the timer from the departure exits the hub.
    clock.advance_when_parked(Some(zero + IDLE), None, IDLE, &Deadline::after(WITHIN));
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits idle"),
        0
    );
    drop(peer);
}

#[test]
fn an_accept_after_the_exit_claim_gets_eof_without_hello() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let hub = crate::testkit::hub(&temp.dir, Arc::new(FakeStarter::hang(&temp.dir)), timed);
    // No code under test waits on the clock here; the move happens before hub.idle_wait runs, which then runs on the test's own thread with no other hub thread (ruling 6179406086).
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
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    assert!(!temp.socket().exists());
    assert!(temp.log().contains("The hub stopped: signal."));
    assert!(!temp.log().contains("peak_memory"));
}

#[test]
fn a_signal_aborts_a_sessions_stop_with_a_partial_answer() {
    // One strict session answers `subscribe` and `close`, then holds
    // its socket open: the stop's fan-out parks with it unconfirmed.
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (hub, got, done) = serve_with_hub_and_starter(
        &temp,
        IDLE,
        Arc::clone(&clock),
        FakeStarter::strict(&temp.dir),
    );
    await_idle_park(&clock, clock.origin(), "at start");
    let workspace = temp.dir.join("w");
    fs::create_dir_all(&workspace).unwrap();
    let mut asker = connect(&temp);
    asker.send(&json!({"id": "c_1", "command": "start", "args": {"workspace": workspace}}));
    let started = asker.next("the start").expect("the hub starts the session");
    let session = started["payload"]["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The fan-out wait parks with the session unconfirmed.
    let (parked_tx, parked) = mpsc::channel();
    *hub.on_fanout_wait.lock().unwrap() = Some(Box::new(move || {
        parked_tx.send(()).unwrap_or(());
    }));
    asker.send(&json!({"id": "c_stop", "command": "stop", "args": {"sessions": true}}));
    assert!(
        Deadline::after(DEADLINE).recv(&parked).is_ok(),
        "the loop parks unconfirmed"
    );
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub.waker().wake();
    // The asker reads the partial rejection naming the session, then EOF.
    let rejected = asker
        .next("the rejection")
        .expect("the hub answers the stop");
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["code"], "io_failed");
    assert_eq!(rejected["payload"]["command_id"], "c_stop");
    assert!(
        rejected["payload"]["message"]
            .as_str()
            .unwrap()
            .contains(&session),
        "the rejection names the session: {rejected}"
    );
    assert!(asker.next("the close").is_none(), "EOF follows the answer");
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    // One stop, one line, and the fake clock never advanced.
    assert_eq!(temp.log().matches("hub_stopped").count(), 1);
    assert_eq!(clock.now(), clock.origin());
}

#[test]
fn a_second_bare_stop_aborts_a_sessions_stop() {
    // Same unconfirmed session, but a second client's bare `stop`
    // aborts the scan instead of a signal: its `request_stop` drives
    // the same Signal arm.
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (hub, _got, done) = serve_with_hub_and_starter(
        &temp,
        IDLE,
        Arc::clone(&clock),
        FakeStarter::strict(&temp.dir),
    );
    await_idle_park(&clock, clock.origin(), "at start");
    let workspace = temp.dir.join("w");
    fs::create_dir_all(&workspace).unwrap();
    let mut asker = connect(&temp);
    asker.send(&json!({"id": "c_1", "command": "start", "args": {"workspace": workspace}}));
    let started = asker.next("the start").expect("the hub starts the session");
    let session = started["payload"]["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (parked_tx, parked) = mpsc::channel();
    *hub.on_fanout_wait.lock().unwrap() = Some(Box::new(move || {
        parked_tx.send(()).unwrap_or(());
    }));
    asker.send(&json!({"id": "c_stop", "command": "stop", "args": {"sessions": true}}));
    assert!(
        Deadline::after(DEADLINE).recv(&parked).is_ok(),
        "the loop parks unconfirmed"
    );
    let mut second = connect(&temp);
    second.send(&json!({"id": "c_2", "command": "stop", "args": {}}));
    // The second client reads its bare ack, then EOF.
    let ack = second
        .next("the bare ack")
        .expect("the hub acks the bare stop");
    assert_eq!(ack["kind"], "command_accepted");
    assert_eq!(ack["payload"]["command_id"], "c_2");
    assert!(
        ack["payload"].get("result").is_none(),
        "a bare ack carries no result"
    );
    assert!(second.next("the close").is_none(), "EOF follows the ack");
    // The asker reads the rejection, then EOF.
    let rejected = asker
        .next("the rejection")
        .expect("the hub answers the stop");
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["code"], "io_failed");
    assert!(
        rejected["payload"]["message"]
            .as_str()
            .unwrap()
            .contains(&session),
        "the rejection names the session: {rejected}"
    );
    assert!(asker.next("the close").is_none(), "EOF follows the answer");
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    assert_eq!(clock.now(), clock.origin());
}

#[test]
fn a_signal_before_the_first_listing_names_only_the_live_session() {
    // A start held between the admit steps keeps the serving thread in
    // its first zero-wait: the aborting probe lists `run/` and names
    // the live listener, skipping the stale path.
    const LIVE: &str = "s_0123456789abcdef";
    const STALE: &str = "s_1111111111111111";
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (hub, got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    let run = temp.dir.join("run");
    let _live = UnixListener::bind(run.join(LIVE)).unwrap();
    drop(UnixListener::bind(run.join(STALE)).unwrap());
    // The start pauses after its increment, before its flag load.
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    *hub.between_admit_steps.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap_or(());
        release.recv().unwrap_or(());
    }));
    let workspace = temp.dir.join("w");
    fs::create_dir_all(&workspace).unwrap();
    let mut starter = connect(&temp);
    starter.send(&json!({"id": "c_1", "command": "start", "args": {"workspace": workspace}}));
    assert!(
        Deadline::after(DEADLINE).recv(&entered).is_ok(),
        "the start pauses between the admit steps"
    );
    let (waiting_tx, waiting) = mpsc::channel();
    *hub.on_inflight_wait.lock().unwrap() = Some(Box::new(move || {
        waiting_tx.send(()).unwrap_or(());
    }));
    let mut asker = connect(&temp);
    asker.send(&json!({"id": "c_stop", "command": "stop", "args": {"sessions": true}}));
    assert!(
        Deadline::after(DEADLINE).recv(&waiting).is_ok(),
        "the stop parks on the nonzero count"
    );
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub.waker().wake();
    // The probe names the live listener and skips the stale path.
    let rejected = asker
        .next("the rejection")
        .expect("the hub answers the stop");
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["code"], "io_failed");
    let message = rejected["payload"]["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains(LIVE),
        "the rejection names the live session: {message}"
    );
    assert!(
        !message.contains(STALE),
        "the rejection skips the stale path: {message}"
    );
    assert!(asker.next("the close").is_none(), "EOF follows the answer");
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    assert_eq!(clock.now(), clock.origin());
    // Releasing the paused start refuses it on the closed socket.
    drop(release_tx);
}

#[test]
fn a_signal_abort_flush_expires_when_the_answer_never_lands() {
    // The serving thread sees the abort but never writes: the arm's
    // flush wait expires on the injected clock, and the asker reads
    // EOF with no answer.
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (hub, got, done) = serve_with_hub_and_starter(
        &temp,
        IDLE,
        Arc::clone(&clock),
        FakeStarter::strict(&temp.dir),
    );
    await_idle_park(&clock, clock.origin(), "at start");
    let workspace = temp.dir.join("w");
    fs::create_dir_all(&workspace).unwrap();
    let mut asker = connect(&temp);
    asker.send(&json!({"id": "c_1", "command": "start", "args": {"workspace": workspace}}));
    asker.next("the start").expect("the hub starts the session");
    let (parked_tx, parked) = mpsc::channel();
    *hub.on_fanout_wait.lock().unwrap() = Some(Box::new(move || {
        parked_tx.send(()).unwrap_or(());
    }));
    let (flushed_tx, flushed) = mpsc::channel();
    *hub.on_abort_flush.lock().unwrap() = Some(Box::new(move || {
        flushed_tx.send(thread::current().id()).unwrap_or(());
    }));
    // The serving thread blocks after the abort, before its answer.
    // Its entry proves it left the fan-out park, so the arm's flush
    // wait is the only clock park when the test advances.
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    *hub.before_abort_answer.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap_or(());
        release.recv().unwrap_or(());
    }));
    asker.send(&json!({"id": "c_stop", "command": "stop", "args": {"sessions": true}}));
    assert!(
        Deadline::after(DEADLINE).recv(&parked).is_ok(),
        "the loop parks unconfirmed"
    );
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub.waker().wake();
    assert!(
        Deadline::after(DEADLINE).recv(&entered).is_ok(),
        "the loop sees the abort"
    );
    // The arm parks in its flush wait with the answer still held.
    let end = clock.origin() + SESSIONS_ABORT_FLUSH;
    assert!(
        clock.await_parked(end, fakes::Deadline::after(WITHIN)),
        "the arm parks on the flush bound"
    );
    // The hook records the arm thread, whose flush wait the advances
    // below name: under load the re-woken fan-out worker can re-park
    // at the sessions bound first.
    let arm = Deadline::after(DEADLINE)
        .recv(&flushed)
        .expect("the arm parks in its flush wait");
    // One millisecond short of the bound the hub stays.
    clock.advance_when_parked(
        Some(end),
        Some(arm),
        SESSIONS_ABORT_FLUSH
            .checked_sub(Duration::from_millis(1))
            .expect("the flush bound exceeds a millisecond"),
        &fakes::Deadline::after(DEADLINE),
    );
    assert!(done.try_recv().is_err(), "the hub stays before the bound");
    clock.advance_when_parked(
        Some(end),
        Some(arm),
        Duration::from_millis(1),
        &fakes::Deadline::after(DEADLINE),
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    assert!(
        asker.next("the close").is_none(),
        "EOF arrives with no answer"
    );
    // The released write lands on the closed socket and fails silently.
    drop(release_tx);
}

#[test]
fn a_debug_hub_writes_peak_memory_just_before_its_signal_stop() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (hub, got, done) = serve_with_hub_at(&temp, IDLE, Arc::clone(&clock), Level::Debug);
    await_idle_park(&clock, clock.origin(), "at start");
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub.waker().wake();
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
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
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
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
    let (done, _) = serve_in(
        &temp,
        Duration::ZERO,
        Arc::clone(&fakes::clock::FakeClock::new()),
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits at once"),
        0
    );
}

#[test]
fn a_signal_stop_keeps_peak_memory_next_to_hub_stopped_during_disconnects() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    // The seam runs between the stop lines' two appends and reports
    // whether the log's lock is still held, which only the single-lock
    // implementation can satisfy. No threads, no waits: the property
    // is read directly off the lock.
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let between: Arc<dyn Fn(bool) + Send + Sync> = Arc::new({
        let calls = Arc::clone(&calls);
        move |held| calls.lock().unwrap().push(held)
    });
    let (hub, got, done) = serve_with_hub_at_peak_between_and_starter(
        &temp,
        IDLE,
        Arc::clone(&clock),
        Level::Debug,
        || Some(9),
        between,
        FakeStarter::hang(&temp.dir),
    );
    await_idle_park(&clock, clock.origin(), "at start");
    let client = connect(&temp);
    await_open_park(&clock, "after the arrival");
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub.waker().wake();
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    drop(client);
    assert_eq!(*calls.lock().unwrap(), [true]);
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
}

/// One idle millisecond: an on-demand hub would exit almost at once.
const BRIEF: Duration = Duration::from_millis(1);

/// How long the test watches a wait that must not finish.
const STILL: Duration = Duration::from_millis(200);

/// Ends the hub serving in this process the way the service manager does.
fn raise_sigterm() {
    signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
}

#[test]
fn an_installed_hub_never_exits_for_idleness_and_stops_on_sigterm() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (done, serve_id) = serve_mode(
        &temp,
        BRIEF,
        Arc::clone(&clock),
        Level::Info,
        crate::Mode::Installed,
    );
    // An installed hub parks with no deadline even with no client.
    assert!(
        clock.await_parked_unbounded(fakes::Deadline::after(WITHIN)),
        "the installed hub waits with no idle deadline"
    );
    clock.advance_when_parked(
        None,
        Some(serve_id),
        Duration::from_secs(3600),
        &Deadline::after(WITHIN),
    );
    let client = connect(&temp);
    drop(client);
    assert!(
        Deadline::after(STILL).recv(&done).is_err(),
        "the installed hub exited for idleness"
    );
    raise_sigterm();
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    assert!(!temp.socket().exists());
    assert!(temp.log().contains("The hub stopped: signal."));
}

#[test]
fn an_installed_hub_waits_for_the_lock_then_binds() {
    let temp = Temp::new();
    let other = crate::listen::lock(&temp.dir)
        .unwrap()
        .expect("the other hub's lock");
    let clock = fakes::clock::FakeClock::new();
    let (done, _) = serve_mode(
        &temp,
        BRIEF,
        Arc::clone(&clock),
        Level::Info,
        crate::Mode::Installed,
    );
    assert!(
        Deadline::after(STILL).recv(&done).is_err(),
        "the installed hub returned while another held the lock"
    );
    assert!(!temp.socket().exists(), "a waiting hub binds nothing");
    assert!(
        !temp.dir.join("logs").exists(),
        "a waiting hub writes no log"
    );
    drop(other);
    assert!(
        clock.await_parked_unbounded(fakes::Deadline::after(WITHIN)),
        "the installed hub serves once the lock is released"
    );
    drop(connect(&temp));
    raise_sigterm();
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
}

#[test]
fn each_exit_has_its_code() {
    assert_eq!(Exit::Idle.code(), 0);
    assert_eq!(Exit::Failed.code(), 1);
    assert_eq!(Exit::Signal(signal_hook::consts::SIGTERM).code(), 143);
}

/// Serves hub B in `temp` after `run/` was removed and recreated, on its
/// own clock so only A's expiry is driven. Returns B's hub, its signal flag
/// and its exit-code receiver.
fn serve_second_hub(
    temp: &Temp,
) -> (
    Arc<Hub>,
    Arc<AtomicI32>,
    mpsc::Receiver<i32>,
    Arc<fakes::clock::FakeClock>,
) {
    let clock = fakes::clock::FakeClock::new();
    let (hub, got, done) = serve_with_hub(temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "the second hub serves");
    (hub, got, done, clock)
}

/// Stops hub B the way the service manager does and takes its exit code.
fn stop_second_hub(hub: &Arc<Hub>, got: &Arc<AtomicI32>, done: mpsc::Receiver<i32>) {
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub.waker().wake();
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the second hub stops"),
        143
    );
}

#[test]
fn a_signal_exits_when_run_was_replaced_by_a_second_hub() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (hub_a, got_a, done_a) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "the first hub serves");
    // While the first hub lives, `run/` is removed and recreated: a second
    // hub takes the new directory's lock and binds the same `run/hub`.
    fs::remove_dir_all(temp.dir.join("run")).unwrap();
    let (hub_b, got_b, done_b, _clock_b) = serve_second_hub(&temp);
    got_a.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub_a.waker().wake();
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done_a)
            .expect("the first hub exits on its signal"),
        143
    );
    // The first hub's exit must not unlink the second hub's socket.
    drop(connect(&temp));
    stop_second_hub(&hub_b, &got_b, done_b);
}

#[test]
fn an_idle_exit_lands_when_run_was_replaced_by_a_second_hub() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (_hub_a, _got_a, done_a) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "the first hub serves");
    // While the first hub lives, `run/` is removed and recreated: a second
    // hub takes the new directory's lock and binds the same `run/hub`.
    fs::remove_dir_all(temp.dir.join("run")).unwrap();
    let (hub_b, got_b, done_b, _clock_b) = serve_second_hub(&temp);
    clock.advance_when_parked(
        Some(clock.origin() + IDLE),
        None,
        IDLE,
        &Deadline::after(WITHIN),
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done_a)
            .expect("the first hub exits idle"),
        0
    );
    // The first hub's exit must not unlink the second hub's socket.
    drop(connect(&temp));
    stop_second_hub(&hub_b, &got_b, done_b);
}

#[test]
fn an_idle_exit_lands_when_run_is_gone_entirely() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (_hub, _got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    fs::remove_dir_all(temp.dir.join("run")).unwrap();
    clock.advance_when_parked(
        Some(clock.origin() + IDLE),
        None,
        IDLE,
        &Deadline::after(WITHIN),
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits idle"),
        0
    );
}

#[test]
fn an_idle_exit_connects_nowhere_through_the_socket_path() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let (_hub, _got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    fs::remove_file(temp.socket()).unwrap();
    let rebound = UnixListener::bind(temp.socket()).unwrap();
    rebound.set_nonblocking(true).unwrap();
    clock.advance_when_parked(
        Some(clock.origin() + IDLE),
        None,
        IDLE,
        &Deadline::after(WITHIN),
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits idle"),
        0
    );
    assert!(
        fs::symlink_metadata(temp.socket()).is_ok(),
        "the exit leaves the rebound socket in place"
    );
    match rebound.accept() {
        Ok(_) => panic!("the idle exit connected through the rebound socket path"),
        Err(error) => assert_eq!(
            error.kind(),
            std::io::ErrorKind::WouldBlock,
            "the rebound listener stays quiet"
        ),
    }
}

/// A fake `$SHELL` under `temp` running `body`, with the pid prologue.
fn fake_shell(temp: &Temp, body: &str) -> std::path::PathBuf {
    let nonce = match fakes::check_run() {
        Some((name, value)) => format!("export {name}={}\n", value.to_string_lossy()),
        None => String::new(),
    };
    let body = format!(
        "export FAKE_DIR={}\necho $$ > $FAKE_DIR/pid\n{}{}",
        temp.dir.display(),
        nonce,
        body
    );
    fakes::script(&temp.dir, "shell", &body)
}

/// The inherited environment handing `shell` as `$SHELL`.
fn inherited_env(
    shell: &std::path::Path,
    home: &std::path::Path,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    vec![
        (
            std::ffi::OsString::from("HOME"),
            home.as_os_str().to_owned(),
        ),
        (
            std::ffi::OsString::from("USER"),
            std::ffi::OsString::from("fiber-test"),
        ),
        (
            std::ffi::OsString::from("SHELL"),
            shell.as_os_str().to_owned(),
        ),
    ]
}

/// The fake shell's pid, read under a wall-clock deadline.
fn shell_pid(temp: &Temp) -> u32 {
    let deadline = Deadline::after(WITHIN);
    loop {
        if let Ok(text) = fs::read_to_string(temp.dir.join("pid"))
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            return pid;
        }
        if deadline.left().is_zero() {
            panic!("waited for the fake shell to report its pid");
        }
        thread::yield_now();
    }
}

#[test]
fn a_signal_during_a_hung_first_capture_stops_with_no_environment_built() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "exec sleep 600");
    let clock = fakes::clock::FakeClock::new();
    let origin = clock.origin();
    let environments = crate::Environments::new(inherited_env(&shell, &temp.dir), &temp.dir);
    let (done, _) = serve_mode_env(
        &temp,
        IDLE,
        Arc::clone(&clock),
        Level::Info,
        crate::Mode::OnDemand,
        Arc::clone(&environments),
    );
    let pid = shell_pid(&temp);
    let _watchdog = fakes::Watchdog::group(pid);
    assert!(
        clock.await_parked(
            origin + crate::environment::capture::CAPTURE_DEADLINE,
            fakes::Deadline::after(WITHIN)
        ),
        "the first capture waits on its deadline"
    );
    raise_sigterm();
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    assert!(
        fakes::pids_exit(&[pid], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    assert!(
        !temp.log().contains("environment_built"),
        "a cancelled capture writes no line"
    );
    assert!(temp.log().contains("The hub stopped: signal."));
}

#[test]
fn close_runs_before_hub_stopped_in_the_shutdown_race() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "export MARK=1\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let environments = crate::Environments::new(inherited_env(&shell, &temp.dir), &temp.dir);
    // Hold the capture between its end and its publish: the hook reports
    // it is held, then blocks until the test releases it.
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    *support::lock(&environments.before_publish) = Some(Box::new(move || {
        held_tx.send(()).unwrap_or(());
        Deadline::after(DEADLINE).recv(&release_rx).unwrap_or(());
    }));
    let (done, _) = serve_mode_env(
        &temp,
        IDLE,
        Arc::clone(&clock),
        Level::Info,
        crate::Mode::OnDemand,
        Arc::clone(&environments),
    );
    Deadline::after(DEADLINE)
        .recv(&held_rx)
        .expect("the capture holds before publishing");
    raise_sigterm();
    crate::testkit::await_true("close begins", {
        let environments = Arc::clone(&environments);
        move || environments.is_closed()
    });
    assert!(
        !temp.log().contains("hub_stopped"),
        "close runs before hub_stopped is written"
    );
    release_tx.send(()).unwrap_or(());
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    assert!(
        !temp.log().contains("environment_built"),
        "the cancelled capture writes no line"
    );
    assert!(
        temp.log().trim_end().ends_with("}"),
        "the log ends with a line"
    );
    let last = temp.log().lines().last().unwrap_or_default().to_owned();
    assert!(
        last.contains("hub_stopped"),
        "the log ends with hub_stopped: {last}"
    );
}

#[test]
fn an_idle_exiting_hub_answers_one_connection_eof_and_leaves_the_next_unanswered_until_its_socket_closes()
 {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "export MARK=1\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let environments = crate::Environments::new(inherited_env(&shell, &temp.dir), &temp.dir);
    // Hold the cleanup after the idle-exit claim: the hook reports it is
    // held, then blocks until the test releases it.
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    *support::lock(&environments.before_publish) = Some(Box::new(move || {
        held_tx.send(()).unwrap_or(());
        Deadline::after(DEADLINE).recv(&release_rx).unwrap_or(());
    }));
    let (done, serve_id) = serve_mode_env(
        &temp,
        IDLE,
        Arc::clone(&clock),
        Level::Info,
        crate::Mode::OnDemand,
        Arc::clone(&environments),
    );
    Deadline::after(DEADLINE)
        .recv(&held_rx)
        .expect("the capture holds before publishing");
    await_idle_park(&clock, clock.origin(), "at start");
    clock.advance_when_parked(
        Some(clock.origin() + IDLE),
        Some(serve_id),
        IDLE,
        &Deadline::after(WITHIN),
    );
    crate::testkit::await_true("close begins", {
        let environments = Arc::clone(&environments);
        move || environments.is_closed()
    });
    // The first connection after the claim is dropped unanswered. No
    // socket-level read timeout: the claim already set `stop`, so the
    // accept loop may drop the peer between `connect` and any `setsockopt`,
    // which fails the call (`EINVAL` on macOS, see the forced-positions
    // test below). The channel receive already bounds the wait.
    let first = UnixStream::connect(temp.socket()).expect("the first connection connects");
    let (first_tx, first_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut first = first;
        let mut got = Vec::new();
        let read = first.read_to_end(&mut got);
        first_tx.send((read, got)).unwrap_or(());
    });
    let (read, got) = Deadline::after(DEADLINE)
        .recv(&first_rx)
        .expect("the first connection ends");
    assert_eq!(
        read.expect("EOF, not a hello"),
        0,
        "the first connection reads EOF"
    );
    assert!(got.is_empty(), "the first connection carries no bytes");
    // The second connection queues unanswered while the hold lasts. The
    // reader probes its socket non-blocking while the hold lasts: WouldBlock
    // proves no bytes arrived and the peer is still open (EOF would read
    // Ok(0)). The probe's signal orders that proof before these asserts,
    // and the hold proves the hub cannot answer between the probe and the
    // release, so no timing assertion is needed.
    let second = UnixStream::connect(temp.socket()).expect("the second connection connects");
    let (reading_tx, reading_rx) = mpsc::channel::<Result<(), String>>();
    let (result_tx, result_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut second = second;
        second.set_nonblocking(true).unwrap();
        let mut probe = [0u8; 1];
        let probed = match second.read(&mut probe) {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
            other => Err(format!("{other:?}")),
        };
        reading_tx.send(probed).unwrap_or(());
        second.set_nonblocking(false).unwrap();
        let mut bytes = Vec::new();
        let read = second.read_to_end(&mut bytes);
        let size = read
            .as_ref()
            .map(|size| *size)
            .map_err(|error| error.kind());
        result_tx.send((size, bytes)).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&reading_rx)
        .expect("the reader probed its read")
        .expect("the second connection stays unanswered while the hold lasts");
    assert!(
        temp.socket().exists(),
        "the socket stays bound during cleanup"
    );
    assert!(
        !temp.log().contains("hub_stopped"),
        "no stop line while the hold lasts"
    );
    release_tx.send(()).unwrap_or(());
    let (size, bytes) = Deadline::after(DEADLINE)
        .recv(&result_rx)
        .expect("the second connection ends once the socket closes");
    assert!(bytes.is_empty(), "the second connection carries no bytes");
    match size {
        Ok(0) => {}
        Err(std::io::ErrorKind::ConnectionReset) => {}
        other => panic!("the second connection ends EOF or reset, got {other:?}"),
    }
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub exits idle"),
        0
    );
    assert!(!temp.socket().exists(), "the exit removes the socket");
    let last = temp.log().lines().last().unwrap_or_default().to_owned();
    assert!(
        last.contains("hub_stopped"),
        "the log ends with hub_stopped: {last}"
    );
}

/// A hub whose idle exit claimed `stop` and stalled closing its
/// environment in the test's `before_publish` hold: the accept loop drops
/// every connection unanswered, while the listener stays bound until the
/// test releases the hold.
struct Stalled {
    temp: Temp,
    release_tx: mpsc::Sender<()>,
    done: mpsc::Receiver<i32>,
}

fn start_stalled_hub() -> Stalled {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "export MARK=1\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let environments = crate::Environments::new(inherited_env(&shell, &temp.dir), &temp.dir);
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    *support::lock(&environments.before_publish) = Some(Box::new(move || {
        held_tx.send(()).unwrap_or(());
        release_rx.recv().unwrap_or(());
    }));
    let (done, serve_id) = serve_mode_env(
        &temp,
        IDLE,
        Arc::clone(&clock),
        Level::Info,
        crate::Mode::OnDemand,
        Arc::clone(&environments),
    );
    Deadline::after(DEADLINE)
        .recv(&held_rx)
        .expect("the capture holds before publishing");
    await_idle_park(&clock, clock.origin(), "at start");
    clock.advance_when_parked(
        Some(clock.origin() + IDLE),
        Some(serve_id),
        IDLE,
        &Deadline::after(WITHIN),
    );
    crate::testkit::await_true("close begins", {
        let environments = Arc::clone(&environments);
        move || environments.is_closed()
    });
    Stalled {
        temp,
        release_tx,
        done,
    }
}

/// Reads to EOF on a clone: `Ok(0)` with no bytes proves the accept loop
/// took the connection and dropped it unanswered.
fn await_drop(stream: &UnixStream, what: &str) {
    let probe = stream.try_clone().unwrap();
    let (eof_tx, eof_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut probe = probe;
        let mut sink = Vec::new();
        let read = probe.read_to_end(&mut sink).map_err(|error| error.kind());
        eof_tx.send(read).unwrap_or(());
    });
    let read = Deadline::after(DEADLINE)
        .recv(&eof_rx)
        .unwrap_or_else(|_| panic!("the accept loop drops the connection {what}"));
    assert_eq!(read, Ok(0), "the drop carries no bytes {what}");
}

/// #2259: the exiting hub's accept-side drop lands between a client's
/// `connect` and its `set_read_timeout`, failing the setup call. Each
/// position below forces the teardown to one side of the setup calls with
/// a proof signal first, so every outcome is ordered, not raced.
#[test]
fn each_forced_teardown_position_around_connect_and_set_read_timeout() {
    // The accept-side drop lands before `set_read_timeout` while the
    // listener stays bound: EOF on the clone proves the drop, so the
    // setup call runs strictly after it.
    let dropped = start_stalled_hub();
    let racy = UnixStream::connect(dropped.temp.socket()).expect("the drop position connects");
    await_drop(&racy, "at the drop position");
    let timed = racy.set_read_timeout(Some(DEADLINE));
    // A read timeout set after the peer's drop is `EINVAL` where the
    // kernel rejects the option on a dead socket.
    #[cfg(target_os = "macos")]
    assert_eq!(
        timed.map_err(|error| error.raw_os_error()),
        Err(Some(libc::EINVAL)),
        "a read timeout set after the accept-side drop is EINVAL"
    );
    #[cfg(not(target_os = "macos"))]
    drop(timed);
    dropped.release_tx.send(()).unwrap_or(());
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&dropped.done)
            .expect("the drop position exits idle"),
        0
    );
    // Both closes land before `set_read_timeout`: the drop as above,
    // then the listener drop and unlink after the release. The missing
    // path proves the unlink, so the setup call runs strictly after it.
    let unlinked = start_stalled_hub();
    let queued = UnixStream::connect(unlinked.temp.socket()).expect("the unlink position connects");
    await_drop(&queued, "at the unlink position");
    unlinked.release_tx.send(()).unwrap_or(());
    crate::testkit::await_true("the exit removes the socket at the unlink position", {
        let socket = unlinked.temp.socket();
        move || !socket.exists()
    });
    let timed = queued.set_read_timeout(Some(DEADLINE));
    #[cfg(target_os = "macos")]
    assert_eq!(
        timed.map_err(|error| error.raw_os_error()),
        Err(Some(libc::EINVAL)),
        "a read timeout set after the unlink is EINVAL"
    );
    #[cfg(not(target_os = "macos"))]
    drop(timed);
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&unlinked.done)
            .expect("the unlink position exits idle"),
        0
    );
    // The unlink lands before `connect`: the missing path proves it, so
    // the connect runs strictly after it and finds no socket.
    let gone = start_stalled_hub();
    gone.release_tx.send(()).unwrap_or(());
    crate::testkit::await_true("the exit removes the socket at the gone position", {
        let socket = gone.temp.socket();
        move || !socket.exists()
    });
    let connected = UnixStream::connect(gone.temp.socket());
    assert_eq!(
        connected.err().map(|error| error.kind()),
        Some(std::io::ErrorKind::NotFound),
        "connecting after the unlink finds no socket"
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&gone.done)
            .expect("the gone position exits idle"),
        0
    );
}

/// Sends one hub command line on `stream` and reads its answer.
fn send_command(
    stream: &UnixStream,
    read: &mut BufReader<UnixStream>,
    id: &str,
    command: &str,
) -> serde_json::Value {
    use std::io::Write as _;
    let line = serde_json::json!({"id": id, "command": command, "args": {}});
    let mut bytes = serde_json::to_vec(&line).unwrap();
    bytes.push(b'\n');
    stream.try_clone().unwrap().write_all(&bytes).unwrap();
    let mut answer = String::new();
    read.read_line(&mut answer)
        .expect("the hub answers the command");
    serde_json::from_str(&answer).unwrap()
}

#[test]
fn a_local_stop_accepts_then_ends_the_hub_as_a_signal_with_sessions_intact() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    // One recorded session: the stop must leave it listed and resumable.
    crate::testkit::write_log(&temp.dir, "s_0000000000000001", "/w", None, &[]);
    let log_path = temp
        .dir
        .join("projects")
        .join("-w")
        .join("sessions")
        .join("s_0000000000000001")
        .join("events.jsonl");
    let before = fs::read(&log_path).unwrap();
    let (_hub, _got, done) = serve_with_hub(&temp, IDLE, Arc::clone(&clock));
    await_idle_park(&clock, clock.origin(), "at start");
    let stream = UnixStream::connect(temp.socket()).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut read = BufReader::new(stream.try_clone().unwrap());
    let mut hello = String::new();
    read.read_line(&mut hello).expect("the hub speaks first");
    assert!(hello.contains("\"kind\":\"hub_hello\""));
    // The acknowledgement arrives before the exit closes the socket: the
    // store runs before the ack is written, and the wake after it.
    let answer = send_command(&stream, &mut read, "c_stop", "stop");
    assert_eq!(
        answer.get("kind"),
        Some(&serde_json::json!("command_accepted"))
    );
    assert_eq!(
        answer
            .get("payload")
            .and_then(|payload| payload.get("command_id")),
        Some(&serde_json::json!("c_stop"))
    );
    assert!(
        answer
            .get("payload")
            .and_then(|payload| payload.get("result"))
            .is_none(),
        "the stop carries no result"
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    assert!(!temp.socket().exists());
    assert!(temp.log().contains("The hub stopped: signal."));
    assert_eq!(
        fs::read(&log_path).unwrap(),
        before,
        "the stop leaves the session's log untouched"
    );
}

#[test]
fn a_stop_requested_before_the_run_starts_exits_without_waiting() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let timed: Arc<dyn Clock> = clock;
    let hub = Arc::new(Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(FakeStarter::hang(&temp.dir)),
        Arc::clone(&timed),
        crate::diag::open(&temp.dir, timed),
    ));
    hub.request_stop();
    let lock = crate::listen::lock(&temp.dir).unwrap().unwrap();
    let bound = crate::listen::bind(&lock, &temp.dir).unwrap().unwrap();
    let held = Held::new(lock, bound);
    let got = AtomicI32::new(0);
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-run".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            move || {
                let exit = run(&hub, &held, IDLE, &got);
                held.stop();
                done_tx.send(exit.code()).unwrap_or(());
            }
        })
        .unwrap();
    // The fake clock never advances: a check-after-return implementation
    // parks to idle expiry and this deadline fails it.
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done_rx)
            .expect("a preset stop exits without waiting"),
        143
    );
    assert!(!temp.socket().exists());
}

#[test]
fn a_stop_store_waits_on_the_tick_hold() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let timed: Arc<dyn Clock> = clock;
    let hub = Arc::new(crate::testkit::hub(
        &temp.dir,
        Arc::new(FakeStarter::hang(&temp.dir)),
        timed,
    ));
    // Hold the tick guard: the store below cannot land until it drops.
    let held = hub.tick.hold();
    // Arm the stop-store's arrival signal before spawning it: the report
    // proves the stop thread reached the held lock, not that it was
    // merely unscheduled.
    let (attempt_tx, attempt_rx) = mpsc::channel();
    *support::lock(&hub.tick.attempt) = Some(attempt_tx);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn({
        let hub = Arc::clone(&hub);
        move || {
            hub.request_stop();
            done_tx.send(()).unwrap_or(());
        }
    });
    Deadline::after(DEADLINE).recv_or_fail(&attempt_rx, "the stop to reach the held lock");
    // Still held, with the arrival proven: the store has not landed, read
    // with no clock.
    assert!(
        done_rx.try_recv().is_err(),
        "the stop store rides the tick hold"
    );
    drop(held);
    Deadline::after(DEADLINE).recv_or_fail(&done_rx, "the store to land once the hold drops");
    assert!(matches!(
        hub.idle_wait(IDLE, &AtomicBool::new(false), &AtomicI32::new(0)),
        Idle::Signal(signal_hook::consts::SIGTERM)
    ));
}
