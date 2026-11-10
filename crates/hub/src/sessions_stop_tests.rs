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
use std::io::{BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use rustix::event::PollFlags;
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{Errno, FdFlags, fcntl_getfd};
use rustix::net::{self, AddressFamily, Ipv4Addr, SocketAddrV4, SocketType};
use serde_json::Value;

use super::transport::*;
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
        .start(&id, Path::new("/w"), None, &[], false, None, false)
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

/// An unreadable `run/` fails the stop instead of reading as absence: a
/// file where the directory belongs makes the listing fail, so no
/// session is named, stopped or running.
#[test]
fn an_unreadable_run_directory_fails_the_stop() {
    let temp = Temp::new("ss-run-unreadable");
    fs::remove_dir(temp.dir.join("run")).unwrap();
    fs::write(temp.dir.join("run"), b"").unwrap();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let outcome = join(stop_async(&hub));
    assert!(matches!(outcome, Outcome::ListFailed));
}

/// A missing `run/` is no sessions: a home that never started one stops
/// nothing and answers zero, instead of failing the listing.
#[test]
fn a_missing_run_directory_stops_nothing() {
    let temp = Temp::new("ss-run-missing");
    fs::remove_dir(temp.dir.join("run")).unwrap();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let outcome = join(stop_async(&hub));
    let (confirmed, unconfirmed) = listed(outcome);
    assert!(confirmed.is_empty());
    assert!(unconfirmed.is_empty());
}

/// Joins a loop thread under one deadline: a loop that waits out the
/// bound instead of answering fails here.
fn join(rx: mpsc::Receiver<Outcome>) -> Outcome {
    Deadline::after(DEADLINE).recv_or_fail(&rx, "the sessions stop")
}

/// Splits a listed outcome: panics on `ListFailed`, so a listing
/// failure can never read as an empty count.
fn listed(outcome: Outcome) -> (Vec<SessionId>, Vec<SessionId>) {
    match outcome {
        Outcome::Listed {
            confirmed,
            unconfirmed,
            ..
        } => (confirmed, unconfirmed),
        Outcome::ListFailed => panic!("the listing succeeded"),
    }
}

/// Splits a listed outcome with its keeper lists: the keeper tests assert
/// every list, so a session misclassified as a keeper fails here.
#[allow(
    clippy::type_complexity,
    reason = "test helper; the tuple is the outcome"
)]
fn listed_all(
    outcome: Outcome,
) -> (
    Vec<SessionId>,
    Vec<SessionId>,
    Vec<SessionId>,
    Vec<SessionId>,
) {
    match outcome {
        Outcome::Listed {
            confirmed,
            keepers,
            unconfirmed,
            unconfirmed_keepers,
        } => (confirmed, keepers, unconfirmed, unconfirmed_keepers),
        Outcome::ListFailed => panic!("the listing succeeded"),
    }
}

fn command_of(line: &str) -> Value {
    serde_json::from_str(line).unwrap()
}

/// The frozen stop line the hub writes, as the fake's policy matches it.
const FROZEN_STOP: &str = "{\"shutdown\":true}\n";

/// What a fake crashed-session socket does per connection: models a
/// post-freeze keeper, which stops on the frozen line and unlinks.
#[derive(Clone, Copy)]
enum KeeperAfter {
    /// Drop the subscribe probe; on the frozen line, unlink and stop
    /// accepting, as a stopping keeper's exit does.
    Stop,
    /// Drop the subscribe probe, then hold the fresh connection open:
    /// the close never comes.
    Wedged,
    /// Read the subscribe, then vanish before the fresh connection: the
    /// gone race.
    Vanish,
    /// Drop the odd connections, and answer the even ones with a line and
    /// hold them: a line is never a confirmation.
    Answer,
}

/// A crashed session's dir with a keeper's lock label, as a takeover
/// leaves it: the split reports the id with the keeper part.
fn hold_keeper_label(temp: &Temp, id: &SessionId) {
    let dir = temp
        .dir
        .join("projects")
        .join("-p")
        .join("sessions")
        .join(&id.0);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("session.lock"), "keeper 4242\n").unwrap();
}

/// A fake keeper socket at `run/<id>`: every connection's raw line is
/// recorded, in accept order.
struct FakeKeeper {
    lines: mpsc::Receiver<String>,
}

impl FakeKeeper {
    fn spawn(run: &Path, id: &SessionId, after: KeeperAfter) -> Self {
        let path = run.join(&id.0);
        let (lines_tx, lines_rx) = mpsc::channel();
        // Bound before the thread starts: the scan that follows finds a
        // bound socket, never a half-spawned one.
        let listener = UnixListener::bind(&path).unwrap();
        thread::Builder::new()
            .name("fake-keeper".to_owned())
            .spawn(move || {
                let mut connections = 0;
                loop {
                    let (mut stream, _) = match listener.accept() {
                        Ok(accepted) => accepted,
                        Err(_) => break,
                    };
                    connections += 1;
                    let mut line = String::new();
                    // Every prober writes first; the read ends at its line.
                    let _ = BufReader::new(&stream).read_line(&mut line).unwrap_or(0);
                    lines_tx.send(line.clone()).unwrap_or(());
                    match after {
                        KeeperAfter::Stop if line == FROZEN_STOP => {
                            fs::remove_file(&path).unwrap();
                            break;
                        }
                        KeeperAfter::Vanish if connections == 1 => {
                            fs::remove_file(&path).unwrap();
                            break;
                        }
                        KeeperAfter::Wedged if connections > 1 => {
                            // Hold the fresh connection open past the test:
                            // the close never comes.
                            thread::park();
                        }
                        KeeperAfter::Answer if connections % 2 == 0 => {
                            // A keeper never writes: any line proves the
                            // holder is no keeper, so it never confirms.
                            // The connection drops after the answer, so the
                            // rescan's connections resolve the same way.
                            stream
                                .write_all(b"{\"unexpected\":\"line\"}\n")
                                .unwrap_or(());
                        }
                        KeeperAfter::Stop
                        | KeeperAfter::Wedged
                        | KeeperAfter::Vanish
                        | KeeperAfter::Answer => {}
                    }
                }
            })
            .unwrap();
        Self { lines: lines_rx }
    }

    /// The recorded lines, in accept order: the subscribe probe, then the
    /// fresh stop.
    fn lines(&self) -> Vec<String> {
        let deadline = Deadline::after(DEADLINE);
        vec![
            deadline.recv_or_fail(&self.lines, "the subscribe probe"),
            deadline.recv_or_fail(&self.lines, "the fresh stop"),
        ]
    }
}

#[test]
fn keeper_eof_after_subscribe_is_stopped_through_a_fresh_connection() {
    let temp = Temp::new("ss-keeper-stop");
    let id = SessionId(testkit::id(7));
    let keeper = FakeKeeper::spawn(&temp.dir.join("run"), &id, KeeperAfter::Stop);
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let outcome = join(stop_async(&hub));
    let (confirmed, keepers, unconfirmed, unconfirmed_keepers) = listed_all(outcome);
    assert!(confirmed.is_empty());
    assert_eq!(keepers, vec![id.clone()]);
    assert!(unconfirmed.is_empty());
    assert!(unconfirmed_keepers.is_empty());
    // The probe subscribes first; the fresh connection carries exactly the
    // frozen stop line a newer Fiber writes an older keeper.
    let lines = keeper.lines();
    assert_eq!(command_of(&lines[0])["command"], "subscribe");
    assert_eq!(lines[1], "{\"shutdown\":true}\n");
    assert!(
        UnixStream::connect(temp.run(&id)).is_err(),
        "the keeper unlinked its socket"
    );
}

#[test]
fn keeper_that_never_closes_is_unconfirmed_past_the_bound() {
    let temp = Temp::new("ss-keeper-hold");
    let id = SessionId(testkit::id(8));
    hold_keeper_label(&temp, &id);
    let _keeper = FakeKeeper::spawn(&temp.dir.join("run"), &id, KeeperAfter::Wedged);
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    // The hook records the loop thread before the stop is sent, so the
    // advance below can name its wait: under load a worker's close-wait
    // can park at the same bound.
    let (waiting_tx, waiting_rx) = mpsc::channel();
    *lock(&hub.on_fanout_wait) = Some(Box::new(move || {
        waiting_tx.send(thread::current().id()).unwrap_or(());
    }));
    let rx = stop_async(&hub);
    // The park is the signal: only now may the clock move past the end.
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    assert!(
        temp.clock.await_parked(end, Deadline::after(DEADLINE)),
        "the loop waits for the close"
    );
    let loop_thread = Deadline::after(DEADLINE)
        .recv(&waiting_rx)
        .unwrap_or_else(|_| panic!("the loop enters its fan-out wait"));
    // Lands exactly on the end: passing a named park is an overshoot,
    // and the loop finishes on `now >= end` either way.
    temp.clock.advance_when_parked(
        Some(end),
        Some(loop_thread),
        SESSIONS_STOP_WITHIN,
        &Deadline::after(DEADLINE),
    );
    let outcome = join(rx);
    let (confirmed, keepers, unconfirmed, unconfirmed_keepers) = listed_all(outcome);
    assert!(confirmed.is_empty());
    assert!(keepers.is_empty());
    assert!(unconfirmed.is_empty());
    assert_eq!(unconfirmed_keepers, vec![id.clone()]);
    // Still running: its socket accepts, as a wedged keeper's does.
    assert!(UnixStream::connect(temp.run(&id)).is_ok());
}

/// A silent keeper holds the stop's read past the bound: the keeper leg
/// waits it out instead of confirming at once. Pins the read's
/// would-block wait through the leg itself: answering confirmed without
/// the wait would close a keeper that never closed.
#[test]
fn stop_keeper_waits_out_a_silent_keeper_to_the_bound() {
    let temp = Temp::new("ss-keeper-silent");
    let id = SessionId(testkit::id(23));
    let listener = UnixListener::bind(temp.run(&id)).unwrap();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        // The stop line arrives first; then silence: the read below
        // would block until the keeper exits, which never comes.
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, FROZEN_STOP);
        // Hold the socket open: closing it would read as EOF and
        // confirm a silent keeper. Loop the park so a spurious
        // wakeup cannot close it either.
        loop {
            thread::park();
        }
    });
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let end = temp.clock.origin() + KEEPERS_STOP_WITHIN;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(stop_keeper(&hub, &id, end)).unwrap_or(());
    });
    // The park is the signal: only now may the clock move past the end.
    assert!(
        temp.clock.await_parked(end, Deadline::after(DEADLINE)),
        "the stop waits for the silent keeper"
    );
    temp.clock.advance_when_parked(
        Some(end),
        None,
        KEEPERS_STOP_WITHIN,
        &Deadline::after(DEADLINE),
    );
    assert!(matches!(
        Deadline::after(DEADLINE).recv_or_fail(&rx, "the silent stop"),
        SessionOutcome::Unconfirmed
    ));
}

/// A garbage read after the stop is no wait and no confirmation: the
/// keeper leg answers unconfirmed at once instead of parking to its
/// bound, because a keeper never writes. Pins the read's error arm
/// through the leg itself: waiting out every error would leave a live
/// session unconfirmed past the bound.
#[test]
fn stop_keeper_leaves_an_overlong_read_unconfirmed_without_waiting() {
    let temp = Temp::new("ss-keeper-garbage");
    let id = SessionId(testkit::id(24));
    let listener = UnixListener::bind(temp.run(&id)).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, FROZEN_STOP);
        // Overlong with no newline: the stop's read fails outright, so
        // it answers unconfirmed without the bound moving. Tolerate the
        // stop closing mid-write: it answers as soon as it has read past
        // the cap.
        let payload = vec![b'x'; support::framing::MAX_LINE + 8];
        stream.write_all(&payload).unwrap_or(());
        // Hold the socket open past the write: closing it would read
        // as EOF and confirm, hiding a wait. Loop the park so a
        // spurious wakeup cannot close it either.
        loop {
            thread::park();
        }
    });
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let end = temp.clock.origin() + KEEPERS_STOP_WITHIN;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(stop_keeper(&hub, &id, end)).unwrap_or(());
    });
    // No clock move: the error answers unconfirmed at once. A mutant
    // that waits out every error instead parks here past the wall
    // deadline and fails.
    assert!(matches!(
        Deadline::after(DEADLINE).recv_or_fail(&rx, "the overlong stop"),
        SessionOutcome::Unconfirmed
    ));
    assert_eq!(temp.clock.now(), temp.clock.origin());
}

#[test]
fn keeper_gone_before_the_stop_write_is_gone_not_confirmed() {
    let temp = Temp::new("ss-keeper-vanish");
    let id = SessionId(testkit::id(9));
    let keeper = FakeKeeper::spawn(&temp.dir.join("run"), &id, KeeperAfter::Vanish);
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let outcome = join(stop_async(&hub));
    let (confirmed, keepers, unconfirmed, unconfirmed_keepers) = listed_all(outcome);
    assert!(confirmed.is_empty());
    assert!(keepers.is_empty());
    assert!(unconfirmed.is_empty());
    assert!(unconfirmed_keepers.is_empty());
    // Only the subscribe probe arrived: the fresh connection never reached
    // a socket, so no stop was written and nothing is confirmed.
    let probe = Deadline::after(DEADLINE).recv_or_fail(&keeper.lines, "the subscribe probe");
    assert_eq!(command_of(&probe)["command"], "subscribe");
    // The fake exits after vanishing, so the channel is empty or
    // disconnected: either way no second line arrived.
    assert!(keeper.lines.try_recv().is_err());
}

#[test]
fn keeper_line_after_the_stop_write_is_not_a_confirmation() {
    let temp = Temp::new("ss-keeper-answer");
    let id = SessionId(testkit::id(10));
    let keeper = FakeKeeper::spawn(&temp.dir.join("run"), &id, KeeperAfter::Answer);
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let outcome = join(stop_async(&hub));
    let (confirmed, keepers, unconfirmed, unconfirmed_keepers) = listed_all(outcome);
    assert!(confirmed.is_empty());
    assert!(keepers.is_empty());
    assert_eq!(unconfirmed, vec![id.clone()]);
    assert!(unconfirmed_keepers.is_empty());
    let lines = keeper.lines();
    assert_eq!(command_of(&lines[0])["command"], "subscribe");
    assert_eq!(lines[1], "{\"shutdown\":true}\n");
}

#[test]
fn abort_before_any_listing_names_keeper_held_ids_with_the_keeper_part() {
    let temp = Temp::new("ss-abort-keeper");
    let live = SessionId(testkit::id(12));
    let _held = UnixListener::bind(temp.run(&live)).unwrap();
    let keeper = SessionId(testkit::id(13));
    hold_keeper_label(&temp, &keeper);
    let _keeper_socket = UnixListener::bind(temp.run(&keeper)).unwrap();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    // Stored before the loop lists: the no-wait probe names everything
    // connectable, splitting keeper-held ids out.
    hub.request_stop();
    let outcome = join(stop_async(&hub));
    let (confirmed, keepers, unconfirmed, unconfirmed_keepers) = listed_all(outcome);
    assert!(confirmed.is_empty());
    assert!(keepers.is_empty());
    assert_eq!(unconfirmed, vec![live]);
    assert_eq!(unconfirmed_keepers, vec![keeper]);
}

#[test]
fn rescan_stops_a_keeper_started_mid_run() {
    let temp = Temp::new("ss-keeper-rescan");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let id = SessionId(testkit::id(11));
    let run = temp.dir.join("run");
    let hooked = id.clone();
    *lock(&hub.on_scan_listed) = Some(Box::new(move || {
        // The keeper binds fully before the scan resumes: the rescan
        // finds a stopping keeper, not a half-bound one.
        FakeKeeper::spawn(&run, &hooked, KeeperAfter::Stop);
    }));
    let outcome = join(stop_async(&hub));
    let (confirmed, keepers, unconfirmed, unconfirmed_keepers) = listed_all(outcome);
    assert!(confirmed.is_empty());
    assert_eq!(keepers, vec![id.clone()]);
    assert!(unconfirmed.is_empty());
    assert!(unconfirmed_keepers.is_empty());
}

#[test]
fn stops_a_running_session_through_subscribe_then_close() {
    let temp = Temp::new("ss-stop");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let probe = starter.clone();
    let id = start(&starter, 1);
    let hub = temp.hub(starter);
    let outcome = join(stop_async(&hub));
    let (confirmed, unconfirmed) = listed(outcome);
    assert_eq!(confirmed, vec![id.clone()]);
    assert!(unconfirmed.is_empty());
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

/// An expired end with no stop finishes the partial round: the loop
/// returns the first round's confirmed and unconfirmed instead of
/// rescanning. A second session starts after the first listing, so a
/// rescan would name it unconfirmed too: an `&&` mutant fails the
/// survivor-only assertion.
#[test]
fn an_expired_end_with_no_stop_finishes_the_partial_round() {
    let temp = Temp::new("ss-expired");
    let starter = FakeStarter::strict(&temp.dir);
    let probe = starter.clone();
    let id = start(&starter, 1);
    let hub = temp.hub(starter);
    let rx = stop_async(&hub);
    // The worker's close lands before its close-wait: polling for it
    // proves the first round's listing ran before the session below
    // starts.
    testkit::await_true("the worker to send close", {
        let probe = probe.clone();
        move || {
            probe
                .received()
                .iter()
                .any(|line| line.contains("\"close\""))
        }
    });
    // Started after the first listing: the rescan sees it, the finished
    // round does not.
    let _second = start(&probe, 2);
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    assert!(
        temp.clock
            .await_parked(end, fakes::Deadline::after(DEADLINE)),
        "the loop waits for the close"
    );
    // Late wake past the end park to SESSIONS_STOP_WITHIN + 1 s; the park
    // is proven just above (ruling 6261259790).
    #[allow(
        clippy::disallowed_methods,
        reason = "late wake: one advance past a park proven at its exact instant just before (ruling 6261259790)"
    )]
    temp.clock
        .advance(SESSIONS_STOP_WITHIN + Duration::from_secs(1));
    let outcome = join(rx);
    let (confirmed, unconfirmed) = listed(outcome);
    assert!(confirmed.is_empty());
    assert_eq!(unconfirmed, vec![id]);
}

#[test]
fn stops_every_running_session_leaving_no_socket_behind() {
    let temp = Temp::new("ss-stop-all");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let first = start(&starter, 1);
    let second = start(&starter, 2);
    let hub = temp.hub(starter);
    let outcome = join(stop_async(&hub));
    let (confirmed, unconfirmed) = listed(outcome);
    assert_eq!(confirmed, vec![first.clone(), second.clone()]);
    assert!(unconfirmed.is_empty());
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
    let (confirmed, unconfirmed) = listed(outcome);
    assert_eq!(confirmed, vec![live]);
    assert!(unconfirmed.is_empty());
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
    let (confirmed, unconfirmed) = listed(outcome);
    assert_eq!(confirmed, vec![live]);
    assert!(unconfirmed.is_empty());
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
            .start(
                &second_for_hook,
                Path::new("/w"),
                None,
                &[],
                false,
                None,
                false,
            )
            .unwrap();
    }));
    let outcome = join(stop_async(&hub));
    let (confirmed, unconfirmed) = listed(outcome);
    assert_eq!(confirmed, vec![first.clone(), second.clone()]);
    assert!(unconfirmed.is_empty());
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
    let (confirmed, unconfirmed) = listed(outcome);
    assert_eq!(confirmed, vec![id]);
    assert!(unconfirmed.is_empty());
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
    let (confirmed, unconfirmed) = listed(outcome);
    assert!(confirmed.is_empty());
    assert_eq!(unconfirmed, vec![id.clone()]);
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
    let (confirmed, unconfirmed) = listed(outcome);
    assert!(confirmed.is_empty());
    // The live listener is named; the stale path is skipped, never named.
    assert_eq!(unconfirmed, vec![live]);
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

use std::io::Read as _;
use std::net::{TcpListener, TcpStream, UdpSocket};

/// An expired bound with no stop is not ready: past `until` returns
/// false before polling, even with data pending on the fd. Replacing the
/// guard's `||` with `&&` polls instead, sees the readiness, and answers
/// true.
#[test]
fn an_expired_bound_with_no_stop_returns_not_ready() {
    let temp = Temp::new("ss-expired");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let hub = temp.hub(starter);
    let (reader, mut writer) = UnixStream::pair().unwrap();
    writer.write_all(b"x").unwrap();
    // The origin is already past: the clock never moves backward, and no
    // stop was requested, so the real code never reaches the poll.
    assert!(
        !wait_ready(reader.as_fd(), PollFlags::IN, &hub, temp.clock.origin()),
        "an expired bound with no stop returns not ready"
    );
}

/// A refused loopback port queues first, then reads its queued error:
/// refused means gone, so the loop skips the session and continues.
#[test]
fn a_refused_connect_reads_gone() {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let socket =
        net::socket(AddressFamily::INET, SocketType::STREAM, None).expect("the probe socket opens");
    fcntl_setfl(
        &socket,
        fcntl_getfl(&socket).unwrap().union(OFlags::NONBLOCK),
    )
    .expect("the probe socket goes non-blocking");
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let queued = net::connect(&socket, &addr).expect_err("the closed port refuses");
    assert_eq!(
        queued,
        Errno::INPROGRESS,
        "a refused loopback port queues first"
    );
    assert!(
        poll_ready(socket.as_fd(), PollFlags::OUT),
        "the refusal surfaces"
    );
    assert!(
        matches!(check_connected(socket.as_fd()), Err(ConnectError::Gone)),
        "a refused connect is gone"
    );
}

/// A connected socket reads no error: the loop proceeds to the session.
#[test]
fn a_connected_connect_reads_ok() {
    let (socket, _peer) = UnixStream::pair().unwrap();
    assert!(
        check_connected(socket.as_fd()).is_ok(),
        "a connected socket reads no error"
    );
}

/// A loopback connection reset by a linger-zero close: returns the
/// client and the release that resets it, once accepted. The server
/// resets only after the release, so the client can fill its send
/// buffer first and the reset never races the fill. Reads after the
/// reset see neither refused nor connected.
fn reset_loopback() -> (TcpStream, mpsc::Sender<()>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (conn, _) = listener.accept().unwrap();
        go_rx.recv().unwrap();
        rustix::net::sockopt::set_socket_linger(&conn, Some(Duration::ZERO))
            .expect("linger-zero arms the reset");
        // Dropping a linger-zero socket resets the connection.
    });
    let client = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    (client, go_tx, server)
}

/// Waits, event by event, until the reset surfaces as a poll error on
/// the client: the reset is in the kernel before this returns.
fn await_reset(client: &TcpStream) {
    for _ in 0..50 {
        if poll_ready(client.as_fd(), PollFlags::empty()) {
            return;
        }
    }
    panic!("the reset surfaces as a poll error");
}

/// A reset after the close acknowledgement still confirms: the loop
/// reads the close as accepted and the reset as gone-after-close, not
/// as a wait. Waiting on the reset instead would hang past the bound.
#[test]
fn a_reset_after_the_close_ack_still_confirms() {
    let temp = Temp::new("ss-reset-after-close");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let id = SessionId(testkit::id(7));
    let listener = UnixListener::bind(temp.run(&id)).unwrap();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut read = BufReader::new(stream);
        let mut answered = 0;
        while let Ok(Some(line)) =
            support::framing::read_line(&mut read, support::framing::MAX_LINE)
        {
            let command: Value = serde_json::from_slice(&line).unwrap();
            let ack = serde_json::json!({"kind": "command_accepted",
                "payload": {"command_id": command.get("id")}});
            let mut bytes = serde_json::to_vec(&ack).unwrap();
            bytes.push(b'\n');
            writer.write_all(&bytes).unwrap();
            answered += 1;
            if answered == 2 {
                rustix::net::sockopt::set_socket_linger(&writer, Some(Duration::ZERO))
                    .expect("linger-zero arms the reset");
                return;
            }
        }
    });
    let outcome = join(stop_async(&hub));
    let (confirmed, unconfirmed) = listed(outcome);
    assert_eq!(confirmed, vec![id]);
    assert!(unconfirmed.is_empty());
}

/// A reset loopback connection reads neither refused nor connected: the
/// loop neither skips the session nor proceeds, but waits out the bound.
#[test]
fn a_reset_connect_reads_unconfirmed() {
    let (client, go, server) = reset_loopback();
    go.send(()).unwrap();
    server.join().unwrap();
    await_reset(&client);
    assert!(
        matches!(
            check_connected(client.as_fd()),
            Err(ConnectError::Unconfirmed)
        ),
        "a reset connect is unconfirmed"
    );
}

/// A reset socket reports its error even when no event is requested:
/// the wait always joins the error and hangup bits, so a reset answers
/// ready with an empty request.
#[test]
fn a_reset_socket_reports_its_error_when_nothing_is_requested() {
    let (client, go, server) = reset_loopback();
    go.send(()).unwrap();
    server.join().unwrap();
    await_reset(&client);
    assert!(
        poll_ready(client.as_fd(), PollFlags::empty()),
        "a reset socket reports its error"
    );
}

/// A path-only fd polls ready through its invalid marker: the wait
/// counts `POLLNVAL` as readiness, as it does hangup and error.
/// Linux-only: `O_PATH` opens a fd that cannot read or write.
#[test]
#[cfg(target_os = "linux")]
fn a_path_only_fd_polls_ready() {
    let temp = Temp::new("ss-pollnval");
    let held = rustix::fs::openat(
        rustix::fs::CWD,
        &temp.dir,
        rustix::fs::OFlags::PATH.union(rustix::fs::OFlags::RDONLY),
        rustix::fs::Mode::empty(),
    )
    .expect("the path fd opens");
    assert!(
        poll_ready(held.as_fd(), PollFlags::IN),
        "a path-only fd polls ready through POLLNVAL"
    );
}

/// A bare error polls ready: a refused datagram surfaces as `POLLERR`
/// with no readability, hangup or invalid marker beside it, which the
/// wait counts as readiness like any other event.
#[test]
fn a_bare_error_polls_ready() {
    // Bind-then-drop leaves a port nothing answers: each datagram it
    // refuses comes back as an error with nothing else queued.
    let closed = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.connect(format!("127.0.0.1:{port}")).unwrap();
    // The refusal arrives after the send: each pass sends again and
    // waits one readiness slice, so a consumed error is replaced by
    // the next refusal until one is seen.
    let mut ready = false;
    for _ in 0..50 {
        socket.send(b"x").unwrap_or(0);
        if poll_ready(socket.as_fd(), PollFlags::IN) {
            ready = true;
            break;
        }
    }
    assert!(ready, "a bare error polls ready");
}

/// A quiet socket polls not ready: replacing the poll with true would
/// wait where there is nothing to wait for.
#[test]
fn a_quiet_socket_polls_not_ready() {
    let (socket, _peer) = UnixStream::pair().unwrap();
    assert!(
        !poll_ready(socket.as_fd(), PollFlags::IN),
        "a quiet socket polls not ready"
    );
}

/// Readiness reports each single-flag combination: readable alone is
/// ready, writable alone is ready, and neither is not.
#[test]
fn readiness_reports_each_single_flag_combination() {
    let (readable, peer) = UnixStream::pair().unwrap();
    (&peer).write_all(b"x").unwrap();
    assert!(
        poll_ready(readable.as_fd(), PollFlags::IN),
        "a readable socket polls ready"
    );
    let (writable, _held) = UnixStream::pair().unwrap();
    assert!(
        poll_ready(writable.as_fd(), PollFlags::OUT),
        "a fresh pair polls writable"
    );
    let (quiet, _kept) = UnixStream::pair().unwrap();
    assert!(
        !poll_ready(quiet.as_fd(), PollFlags::IN),
        "a quiet socket polls not ready"
    );
}

/// A full send buffer makes the bounded write wait, not fail: once the
/// peer drains, the line lands whole.
#[test]
fn a_full_send_buffer_retries_until_drained() {
    let temp = Temp::new("ss-write-full");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let (writer, peer) = UnixStream::pair().unwrap();
    writer.set_nonblocking(true).unwrap();
    peer.set_nonblocking(true).unwrap();
    // Fill the send buffer: small writes until the kernel says to wait.
    let filler = vec![0u8; 65_536];
    let mut written = 0;
    for _ in 0..128 {
        match (&writer).write(&filler) {
            Ok(n) => written += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("filling the send buffer: {error}"),
        }
    }
    let line = serde_json::json!({"id": "c_write_full", "command": "subscribe",
        "args": {"level": "summary"}});
    let mut expected = serde_json::to_vec(&line).unwrap();
    expected.push(b'\n');
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    let queued = writer.try_clone().unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx
            .send(write_line_bounded(&queued, &line, &hub, end))
            .unwrap_or(());
    });
    assert!(
        temp.clock.await_parked(end, Deadline::after(DEADLINE)),
        "the bounded write waits for room"
    );
    // Drain exactly the filler: the freed room unblocks the write,
    // while the line it sends next stays unread for the assertion
    // below. `take` caps the read, so none of the line is consumed.
    let mut chunk = vec![0u8; 65_536];
    let mut drained = 0;
    while drained < written {
        match (&peer)
            .take(u64::try_from(written - drained).unwrap())
            .read(&mut chunk)
        {
            Ok(0) => panic!("the peer closed with filler still unread"),
            Ok(n) => drained += n,
            Err(error) => panic!("draining exactly the filler: {error}"),
        }
    }
    assert!(
        matches!(
            Deadline::after(DEADLINE)
                .recv(&done_rx)
                .expect("the write ends"),
            Wrote::Done
        ),
        "the bounded write lands once drained"
    );
    // The line lands last, after the filler.
    let mut received = Vec::new();
    loop {
        match (&peer).read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => received.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("reading the line: {error}"),
        }
    }
    assert!(
        received.ends_with(&expected),
        "the line lands whole after the drain"
    );
}

/// A dead peer abandons the write as gone: with the send buffer full
/// and the reader closed, the line cannot land, and the session is
/// gone rather than unconfirmed past the bound.
#[test]
fn a_dead_peer_abandons_the_write_as_gone() {
    let temp = Temp::new("ss-write-dead");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let (writer, peer) = UnixStream::pair().unwrap();
    writer.set_nonblocking(true).unwrap();
    peer.set_nonblocking(true).unwrap();
    // Fill the send buffer, then close the reader: the first write
    // after the close fails instead of waiting.
    let filler = vec![0u8; 65_536];
    for _ in 0..128 {
        match (&writer).write(&filler) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("filling the send buffer: {error}"),
        }
    }
    drop(peer);
    let line = serde_json::json!({"id": "c_write_dead", "command": "subscribe",
        "args": {"level": "summary"}});
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    // Advancing past the bound turns a blind wait into unconfirmed
    // instead of a hang.
    let outcome = thread::spawn(move || write_line_bounded(&writer, &line, &hub, end));
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    temp.clock
        .advance(SESSIONS_STOP_WITHIN + Duration::from_secs(1));
    assert!(
        matches!(outcome.join().unwrap(), Wrote::Gone),
        "a dead peer abandons the write as gone"
    );
}

/// A newline-terminated acknowledgement answers without a close: the
/// fill path finds the newline with the socket still open, so no end
/// of input rescues a scan that never sees one. Advancing past the
/// bound turns a blind scan's wait into unconfirmed instead of a hang.
#[test]
fn a_newline_terminated_ack_answers_without_a_close() {
    let temp = Temp::new("ss-ack-no-close");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let (client, session) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let stream = client.try_clone().unwrap();
    let mut read = BufReader::new(client);
    let mut full = serde_json::to_vec(&serde_json::json!({"kind": "command_accepted", "ts": 1,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"command_id": "c_open"}}))
    .unwrap();
    full.push(b'\n');
    (&session).write_all(&full).unwrap();
    // The socket stays open: no end of input follows the line.
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    let answer = thread::spawn(move || read_ack(&mut read, &stream, &hub, "c_open", end));
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    temp.clock
        .advance(SESSIONS_STOP_WITHIN + Duration::from_secs(1));
    assert!(
        matches!(answer.join().unwrap(), Ack::Accepted),
        "a newline-terminated acknowledgement answers with the socket open"
    );
}

/// A partial line at end of input still answers: without its newline the
/// acknowledgement parses once the close ends the input.
#[test]
fn a_partial_line_at_end_of_input_still_answers() {
    for tail in ["", "\r"] {
        let temp = Temp::new("ss-partial-ack");
        let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
        let (client, mut session) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        let stream = client.try_clone().unwrap();
        let mut read = BufReader::new(client);
        let mut full = serde_json::to_vec(&serde_json::json!({"kind": "command_accepted", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": {"command_id": "c_partial"}}))
        .unwrap();
        full.extend_from_slice(tail.as_bytes());
        // No newline: the close ends the input with the line partial.
        session.write_all(&full).unwrap();
        drop(session);
        let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
        assert!(
            matches!(
                read_ack(&mut read, &stream, &hub, "c_partial", end),
                Ack::Accepted
            ),
            "a partial acknowledgement with tail {tail:?} is accepted at end of input"
        );
    }
}

/// A newline-terminated acceptance counts even when the socket closes:
/// the close ends the stream, it does not erase the answer.
#[test]
fn an_accepted_answer_counts_even_when_the_socket_closes() {
    let temp = Temp::new("ss-accepted-close");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let (client, session) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let stream = client.try_clone().unwrap();
    let mut read = BufReader::new(client);
    let mut full = serde_json::to_vec(&serde_json::json!({"kind": "command_accepted", "ts": 1,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"command_id": "c_closed"}}))
    .unwrap();
    full.push(b'\n');
    (&session).write_all(&full).unwrap();
    drop(session);
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    assert!(
        matches!(
            read_ack(&mut read, &stream, &hub, "c_closed", end),
            Ack::Accepted
        ),
        "a newline-terminated acceptance counts before the close"
    );
}

/// A rejection for this command reports rejected: the arm mapping
/// `command_rejected` to `Rejected` is the only reader of a live
/// session's refusal, while a rejection naming another command is
/// skipped like any other line.
#[test]
fn a_rejected_acknowledgement_reports_rejected() {
    let temp = Temp::new("ss-rejected-ack");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    let line = |kind: &str, command_id: &str| {
        let mut line = serde_json::to_vec(&serde_json::json!({"kind": kind, "ts": 1,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": {"command_id": command_id}}))
        .unwrap();
        line.push(b'\n');
        line
    };
    // Another command's acceptance, then this command's rejection:
    // only the named line answers.
    let (client, session) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let stream = client.try_clone().unwrap();
    let mut read = BufReader::new(client);
    (&session)
        .write_all(&line("command_accepted", "c_other"))
        .unwrap();
    (&session)
        .write_all(&line("command_rejected", "c_mine"))
        .unwrap();
    drop(session);
    assert!(
        matches!(
            read_ack(&mut read, &stream, &hub, "c_mine", end),
            Ack::Rejected
        ),
        "this command's rejection reports rejected"
    );
    // Another command's rejection, then this command's acceptance:
    // the unnamed rejection is skipped, not fatal.
    let (client, session) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let stream = client.try_clone().unwrap();
    let mut read = BufReader::new(client);
    (&session)
        .write_all(&line("command_rejected", "c_other"))
        .unwrap();
    (&session)
        .write_all(&line("command_accepted", "c_mine"))
        .unwrap();
    drop(session);
    assert!(
        matches!(
            read_ack(&mut read, &stream, &hub, "c_mine", end),
            Ack::Accepted
        ),
        "another command's rejection is skipped"
    );
}

/// Events plus the acknowledgement in one write answer at once: every
/// complete buffered line drains before the socket is touched again, so
/// a buffered answer never waits out the bound with the peer open, and
/// at end of input only the final partial line takes the single parse.
#[test]
fn buffered_events_plus_the_acknowledgement_answer_at_once() {
    let temp = Temp::new("ss-ack-drain");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    let line = |kind: &str, command_id: &str| {
        let mut line = serde_json::to_vec(&serde_json::json!({"kind": kind, "ts": 1,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": {"command_id": command_id}}))
        .unwrap();
        line.push(b'\n');
        line
    };
    // Two other answers plus this command's in one write, peer open:
    // the answer is already buffered, so no wait runs.
    let (client, session) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let stream = client.try_clone().unwrap();
    let mut read = BufReader::new(client);
    let mut all = line("command_accepted", "c_first");
    all.extend_from_slice(&line("command_accepted", "c_second"));
    all.extend_from_slice(&line("command_accepted", "c_mine"));
    (&session).write_all(&all).unwrap();
    let worker = Arc::clone(&hub);
    let answer = thread::spawn(move || read_ack(&mut read, &stream, &worker, "c_mine", end));
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    temp.clock
        .advance(SESSIONS_STOP_WITHIN + Duration::from_secs(1));
    assert!(
        matches!(answer.join().unwrap(), Ack::Accepted),
        "a buffered acknowledgement answers with the socket open"
    );
    // The same write with the peer closed: the answer still parses,
    // instead of joining its lines into one failed value.
    let (client, session) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let stream = client.try_clone().unwrap();
    let mut read = BufReader::new(client);
    (&session).write_all(&all).unwrap();
    drop(session);
    assert!(
        matches!(
            read_ack(&mut read, &stream, &hub, "c_mine", end),
            Ack::Accepted
        ),
        "buffered lines parse singly at end of input"
    );
}

/// A long line far below the length edge still reaches its
/// acknowledgement: the bound is tens of megabytes out, not a product
/// of the bytes seen so far.
#[test]
fn a_long_line_below_the_edge_still_reaches_its_ack() {
    let temp = Temp::new("ss-length-below");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let (client, session) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let stream = client.try_clone().unwrap();
    let mut read = BufReader::new(client);
    let ack = serde_json::json!({"kind": "command_accepted", "ts": 1,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"command_id": "c_long"}});
    let session_thread = thread::spawn(move || {
        // A megabyte of line: far below the edge for the real bound,
        // far past it for a product of pending and chunk.
        let junk = vec![b'x'; 1 << 20];
        (&session).write_all(&junk).unwrap();
        (&session).write_all(b"\n").unwrap();
        let mut line = serde_json::to_vec(&ack).unwrap();
        line.push(b'\n');
        (&session).write_all(&line).unwrap();
    });
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    assert!(
        matches!(
            read_ack(&mut read, &stream, &hub, "c_long", end),
            Ack::Accepted
        ),
        "a megabyte line is skipped, not fatal"
    );
    session_thread.join().unwrap();
}

/// An unconnected socket fails its first read: the acknowledgement is
/// gone, never waited on. Advancing past the bound turns a blind wait
/// into unconfirmed instead of a hang.
#[test]
fn an_unconnected_socket_is_gone_not_waited_on() {
    let temp = Temp::new("ss-unconnected");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let socket = open_socket().expect("the probe socket opens");
    let unix = UnixStream::from(socket);
    let stream = unix.try_clone().unwrap();
    let mut read = BufReader::new(unix);
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    let answer = thread::spawn(move || read_ack(&mut read, &stream, &hub, "c_dead", end));
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    temp.clock
        .advance(SESSIONS_STOP_WITHIN + Duration::from_secs(1));
    assert!(
        matches!(answer.join().unwrap(), Ack::Gone),
        "an unconnected socket is gone"
    );
}

/// A stop socket starts non-blocking and close-on-exec: the flags the
/// open sets, read back through the same calls.
#[test]
fn open_socket_marks_the_fd_nonblocking_and_close_on_exec() {
    let fd = open_socket().expect("the probe socket opens");
    assert!(
        fcntl_getfl(&fd)
            .expect("the probe socket offers its flags")
            .contains(OFlags::NONBLOCK),
        "a stop socket starts non-blocking"
    );
    assert!(
        fcntl_getfd(&fd)
            .expect("the probe socket offers its descriptor flags")
            .contains(FdFlags::CLOEXEC),
        "a stop socket starts close-on-exec"
    );
}

/// An overlong socket path never confirms: no address holds it, so the
/// connect fails before the socket is touched.
#[test]
fn an_overlong_socket_path_never_confirms() {
    let fd = open_socket().expect("the probe socket opens");
    let too_long = Path::new("run").join(format!("s_{}", "a".repeat(104)));
    assert!(
        matches!(
            start_connect(fd.as_fd(), &too_long),
            Err(ConnectError::Unconfirmed)
        ),
        "a path no address holds never confirms"
    );
}

/// Runs the keepers stop on a thread: the receiver carries its outcome.
fn keepers_async(hub: &Arc<Hub>, ids: Vec<SessionId>) -> mpsc::Receiver<KeepersOutcome> {
    let (tx, rx) = mpsc::channel();
    let hub = Arc::clone(hub);
    thread::Builder::new()
        .name("sessions-stop-keepers-test".to_owned())
        .spawn(move || {
            tx.send(stop_keepers(&hub, &ids)).unwrap_or(());
        })
        .unwrap();
    rx
}

fn join_keepers(rx: mpsc::Receiver<KeepersOutcome>) -> KeepersOutcome {
    Deadline::after(DEADLINE).recv_or_fail(&rx, "the keepers stop")
}

#[test]
fn stop_keepers_confirms_an_eof_keeper_leaves_a_live_session_and_skips_a_gone_one() {
    let temp = Temp::new("ss-keepers-mixed");
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let probe = starter.clone();
    let live = start(&starter, 1);
    let keeper = SessionId(testkit::id(2));
    let fake = FakeKeeper::spawn(&temp.dir.join("run"), &keeper, KeeperAfter::Stop);
    let gone = SessionId(testkit::id(3));
    let hub = temp.hub(starter);
    let outcome = join_keepers(keepers_async(
        &hub,
        vec![gone.clone(), keeper.clone(), live.clone()],
    ));
    assert_eq!(outcome.confirmed, vec![keeper.clone()]);
    assert_eq!(outcome.live, vec![live.clone()]);
    assert!(outcome.unconfirmed.is_empty());
    // The keeper's probe subscribes first; the fresh connection carries
    // exactly the frozen stop.
    let lines = fake.lines();
    assert_eq!(command_of(&lines[0])["command"], "subscribe");
    assert_eq!(lines[1], FROZEN_STOP);
    // The live session answered the verify and got nothing else: no
    // close, no stop.
    let conns = probe.received_by_connection();
    assert_eq!(conns.len(), 1);
    assert_eq!(conns[0].len(), 1);
    assert_eq!(command_of(&conns[0][0])["command"], "subscribe");
}

#[test]
fn stop_keepers_confirms_sibling_keepers_in_the_same_run() {
    let temp = Temp::new("ss-keepers-siblings");
    let first = SessionId(testkit::id(4));
    let _first = FakeKeeper::spawn(&temp.dir.join("run"), &first, KeeperAfter::Stop);
    let second = SessionId(testkit::id(5));
    let _second = FakeKeeper::spawn(&temp.dir.join("run"), &second, KeeperAfter::Stop);
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let outcome = join_keepers(keepers_async(&hub, vec![second.clone(), first.clone()]));
    assert_eq!(outcome.confirmed, vec![first.clone(), second.clone()]);
    assert!(outcome.live.is_empty());
    assert!(outcome.unconfirmed.is_empty());
}

#[test]
fn stop_keepers_leaves_a_wedged_keeper_unconfirmed_past_the_bound() {
    let temp = Temp::new("ss-keepers-wedged");
    let wedged = SessionId(testkit::id(6));
    let _held = FakeKeeper::spawn(&temp.dir.join("run"), &wedged, KeeperAfter::Wedged);
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    // The hook records the serving thread before the bound is reached,
    // so the advance below names its wait.
    let (waiting_tx, waiting_rx) = mpsc::channel();
    *lock(&hub.on_fanout_wait) = Some(Box::new(move || {
        waiting_tx.send(thread::current().id()).unwrap_or(());
    }));
    let rx = keepers_async(&hub, vec![wedged.clone()]);
    // The park is the signal: only now may the clock move past the end.
    let end = temp.clock.origin() + KEEPERS_STOP_WITHIN;
    assert!(
        temp.clock.await_parked(end, Deadline::after(DEADLINE)),
        "the keepers wait parks at its bound"
    );
    let loop_thread = Deadline::after(DEADLINE)
        .recv(&waiting_rx)
        .unwrap_or_else(|_| panic!("the keepers loop enters its fan-out wait"));
    // Lands exactly on the end: passing a named park is an overshoot,
    // and the loop finishes on `now >= end` either way.
    temp.clock.advance_when_parked(
        Some(end),
        Some(loop_thread),
        KEEPERS_STOP_WITHIN,
        &Deadline::after(DEADLINE),
    );
    let outcome = join_keepers(rx);
    assert!(outcome.confirmed.is_empty());
    assert!(outcome.live.is_empty());
    assert_eq!(outcome.unconfirmed, vec![wedged.clone()]);
    // Still running: its socket accepts, as a wedged keeper's does.
    assert!(UnixStream::connect(temp.run(&wedged)).is_ok());
}
