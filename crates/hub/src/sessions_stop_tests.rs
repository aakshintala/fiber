//! Tests for the sessions-stop loop: every session shuts down through
//! its own shutdown, bounded, with the interleavings forced by hooks and
//! barriers instead of waited for.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use serde_json::Value;

use super::*;
use crate::Starter;
use crate::fake::FakeStarter;
use crate::testkit;
use fakes::Deadline;
use fakes::clock::FakeClock;

/// One deadline per join: the loop never parks past it.
const DEADLINE: Duration = Duration::from_secs(10);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
    clock: Arc<FakeClock>,
}

impl Temp {
    fn new(prefix: &str) -> Self {
        let (held, dir) = testkit::with_run(prefix);
        Self {
            dir,
            held,
            clock: FakeClock::new(),
        }
    }

    fn hub(&self, starter: FakeStarter) -> Arc<Hub> {
        let timed: Arc<dyn Clock> = Arc::clone(&self.clock) as Arc<dyn Clock>;
        Arc::new(testkit::hub(&self.dir, Arc::new(starter), timed))
    }

    fn run(&self, id: &SessionId) -> PathBuf {
        self.dir.join("run").join(&id.0)
    }
}

/// Starts one fake session through `starter` under `home`.
fn start(starter: &FakeStarter, n: u64) -> SessionId {
    let id = SessionId(testkit::id(n));
    starter
        .start(&id, Path::new("/w"), None, &[], false, None)
        .unwrap();
    id
}

/// Runs the loop on a thread: the receiver carries its outcome.
fn stop_async(hub: &Arc<Hub>) -> mpsc::Receiver<Outcome> {
    let (tx, rx) = mpsc::channel();
    let hub = Arc::clone(hub);
    thread::Builder::new()
        .name("sessions-stop-test".to_owned())
        .spawn(move || {
            tx.send(run(&hub)).unwrap_or(());
        })
        .unwrap();
    rx
}

/// Joins a loop thread under one deadline: a loop that waits out the
/// bound instead of answering fails here.
fn join(rx: mpsc::Receiver<Outcome>) -> Outcome {
    Deadline::after(DEADLINE).recv_or_fail(&rx, "the sessions stop")
}

fn command_of(line: &str) -> Value {
    serde_json::from_str(line).unwrap()
}

#[test]
fn stops_a_running_session_through_subscribe_then_close() {
    let temp = Temp::new("ss-stop");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let probe = starter.clone();
    let id = start(&starter, 1);
    let hub = temp.hub(starter);
    let outcome = join(stop_async(&hub));
    assert_eq!(outcome.confirmed, vec![id.clone()]);
    assert!(outcome.unconfirmed.is_empty());
    // The socket is gone, as a session's exit leaves it.
    assert!(UnixStream::connect(temp.run(&id)).is_err());
    // This connection subscribed before it closed, each line on a fresh
    // hub-minted id: a close-first mutant fails these.
    let conns = probe.received_by_connection();
    assert_eq!(conns.len(), 1);
    assert_eq!(conns[0].len(), 2);
    let subscribe = command_of(&conns[0][0]);
    let close = command_of(&conns[0][1]);
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["args"]["level"], "summary");
    assert_eq!(close["command"], "close");
    assert_eq!(close["args"]["now"], true);
    let subscribe_id = subscribe["id"].as_str().unwrap();
    let close_id = close["id"].as_str().unwrap();
    assert!(subscribe_id.starts_with("c_hub_stop_"));
    assert!(close_id.starts_with("c_hub_stop_"));
    assert_ne!(subscribe_id, close_id);
}

#[test]
fn stops_every_running_session_leaving_no_socket_behind() {
    let temp = Temp::new("ss-stop-all");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let first = start(&starter, 1);
    let second = start(&starter, 2);
    let hub = temp.hub(starter);
    let outcome = join(stop_async(&hub));
    assert_eq!(outcome.confirmed, vec![first.clone(), second.clone()]);
    assert!(outcome.unconfirmed.is_empty());
    assert!(UnixStream::connect(temp.run(&first)).is_err());
    assert!(UnixStream::connect(temp.run(&second)).is_err());
}

#[test]
fn close_before_subscribe_is_rejected_not_subscribed() {
    let temp = Temp::new("ss-strict");
    let starter = FakeStarter::strict(&temp.dir);
    let id = start(&starter, 1);
    let mut stream = UnixStream::connect(temp.run(&id)).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let line = serde_json::json!({"id": "c_1", "command": "close", "args": {"now": true}});
    let mut bytes = serde_json::to_vec(&line).unwrap();
    bytes.push(b'\n');
    stream.write_all(&bytes).unwrap();
    let mut read = BufReader::new(stream.try_clone().unwrap());
    let answer = support::framing::read_line(&mut read, support::framing::MAX_LINE)
        .unwrap()
        .unwrap();
    let answer: Value = serde_json::from_slice(&answer).unwrap();
    assert_eq!(answer["kind"], "command_rejected");
    assert_eq!(answer["payload"]["command_id"], "c_1");
    assert_eq!(answer["payload"]["code"], "not_subscribed");
}

#[test]
fn a_refused_socket_is_skipped() {
    let temp = Temp::new("ss-refused");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let live = start(&starter, 1);
    // A listener dropped without unlinking: the path refuses, as a dead
    // session's socket does.
    let stale = SessionId(testkit::id(2));
    let stale_listener = UnixListener::bind(temp.run(&stale)).unwrap();
    drop(stale_listener);
    let hub = temp.hub(starter);
    let outcome = join(stop_async(&hub));
    assert_eq!(outcome.confirmed, vec![live]);
    assert!(outcome.unconfirmed.is_empty());
    assert!(temp.run(&stale).exists());
}

#[test]
fn a_session_that_exits_between_enumeration_and_connect_is_skipped() {
    let temp = Temp::new("ss-exit");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let live = start(&starter, 1);
    let gone = SessionId(testkit::id(2));
    // Held open so the listing finds a live path; the hook exits it
    // after the enumeration, before any connect.
    let _held = UnixListener::bind(temp.run(&gone)).unwrap();
    let hub = temp.hub(starter);
    let path = temp.run(&gone);
    *lock(&hub.on_scan_listed) = Some(Box::new(move || {
        fs::remove_file(&path).unwrap();
    }));
    let outcome = join(stop_async(&hub));
    assert_eq!(outcome.confirmed, vec![live]);
    assert!(outcome.unconfirmed.is_empty());
}

#[test]
fn a_session_started_mid_scan_is_shut_down_by_the_rescan() {
    let temp = Temp::new("ss-midscan");
    let starter = Arc::new(FakeStarter::exit_on_close(&temp.dir));
    let first = start(&starter, 1);
    let hub = temp.hub((*starter).clone());
    let second = SessionId(testkit::id(2));
    let for_hook = Arc::clone(&starter);
    let second_for_hook = second.clone();
    *lock(&hub.on_scan_listed) = Some(Box::new(move || {
        // The starter binds fully before the scan resumes: the rescan
        // finds a running session, not a half-bound one.
        for_hook
            .start(&second_for_hook, Path::new("/w"), None, &[], false, None)
            .unwrap();
    }));
    let outcome = join(stop_async(&hub));
    assert_eq!(outcome.confirmed, vec![first.clone(), second.clone()]);
    assert!(outcome.unconfirmed.is_empty());
    assert!(UnixStream::connect(temp.run(&second)).is_err());
}

#[test]
fn the_loop_waits_for_binds_in_flight_before_scanning() {
    let temp = Temp::new("ss-inflight");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let id = start(&starter, 1);
    let hub = temp.hub(starter);
    // A held bind keeps the first zero-wait parked: the parked signal is
    // the hook, fired on the loop's thread.
    hub.inflight_binds.fetch_add(1, Ordering::SeqCst);
    let (entered_tx, entered_rx) = mpsc::channel();
    *lock(&hub.on_inflight_wait) = Some(Box::new(move || {
        entered_tx.send(()).unwrap_or(());
    }));
    let rx = stop_async(&hub);
    Deadline::after(DEADLINE).recv_or_fail(&entered_rx, "the zero-wait park");
    // Parked, not done: no zero-wait means the outcome arrives before the
    // hook ever fires.
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    hub.inflight_binds.fetch_sub(1, Ordering::SeqCst);
    hub.tick.wake();
    let outcome = join(rx);
    assert_eq!(outcome.confirmed, vec![id]);
    assert!(outcome.unconfirmed.is_empty());
}

#[test]
fn abort_mid_fan_out_names_the_session_still_running() {
    let temp = Temp::new("ss-abort");
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let id = start(&starter, 1);
    let hub = temp.hub(starter);
    let (waiting_tx, waiting_rx) = mpsc::channel();
    *lock(&hub.on_fanout_wait) = Some(Box::new(move || {
        waiting_tx.send(()).unwrap_or(());
    }));
    let started = temp.clock.now();
    let rx = stop_async(&hub);
    Deadline::after(DEADLINE).recv_or_fail(&waiting_rx, "the fan-out wait park");
    hub.request_stop();
    hub.tick.wake();
    let outcome = join(rx);
    assert!(outcome.confirmed.is_empty());
    assert_eq!(outcome.unconfirmed, vec![id.clone()]);
    // Still running: its socket accepts, and the clock never moved.
    assert!(UnixStream::connect(temp.run(&id)).is_ok());
    assert_eq!(temp.clock.now(), started);
}

#[test]
fn abort_in_the_first_zero_wait_probes_without_waiting() {
    let temp = Temp::new("ss-abort-wait");
    let live = SessionId(testkit::id(1));
    let _held = UnixListener::bind(temp.run(&live)).unwrap();
    let stale = SessionId(testkit::id(2));
    let stale_listener = UnixListener::bind(temp.run(&stale)).unwrap();
    drop(stale_listener);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter);
    // The held bind keeps the serving thread in its first zero-wait; the
    // hook is the parked signal.
    hub.inflight_binds.fetch_add(1, Ordering::SeqCst);
    let (entered_tx, entered_rx) = mpsc::channel();
    *lock(&hub.on_inflight_wait) = Some(Box::new(move || {
        entered_tx.send(()).unwrap_or(());
    }));
    let started = temp.clock.now();
    let rx = stop_async(&hub);
    Deadline::after(DEADLINE).recv_or_fail(&entered_rx, "the zero-wait park");
    hub.request_stop();
    hub.tick.wake();
    let outcome = join(rx);
    assert!(outcome.confirmed.is_empty());
    // The live listener is named; the stale path is skipped, never named.
    assert_eq!(outcome.unconfirmed, vec![live]);
    assert_eq!(temp.clock.now(), started);
}

#[test]
fn a_split_acknowledgement_parses_whole_after_its_tail_arrives() {
    let temp = Temp::new("ss-split-ack");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let (client, mut session) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let mut full = serde_json::to_vec(&serde_json::json!({"kind": "command_accepted", "ts": 1,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"command_id": "c_split"}}))
    .unwrap();
    full.push(b'\n');
    let cut = full.len() / 2;
    // The prefix lands before the reader exists: the parked wait below
    // can only be the post-prefix wait, never an empty-socket one, so
    // the tail cannot arrive before the reader consumes the prefix.
    session.write_all(&full[..cut]).unwrap();
    let stream = client.try_clone().unwrap();
    let read = BufReader::new(client);
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut read = read;
        let ack = read_ack(&mut read, &stream, &hub, "c_split", end);
        done_tx.send(matches!(ack, Ack::Accepted)).unwrap_or(());
    });
    // The head holds no line: the parked wait below is the signal the
    // reader kept it and waits for the tail.
    assert!(
        temp.clock.await_parked(end, Deadline::after(DEADLINE)),
        "the acknowledgement read parks for its tail"
    );
    session.write_all(&full[cut..]).unwrap();
    assert!(
        Deadline::after(DEADLINE)
            .recv(&done_rx)
            .expect("the read ends"),
        "the split acknowledgement is accepted whole"
    );
}
