//! Tests for a running session closing the relayed connection: the
//! `stream_closed` line after every relayed line, the dropped kept level,
//! and every case that keeps the level instead.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::SessionId;
use contract::clock::wall_ms;
use serde_json::{Map, Value, json};

use super::*;
use crate::Starter as _;
use crate::connection::{Hub, lock};
use crate::fake::{FakeSession, FakeStarter};
use crate::relay::{Kept, Relay, Replayed};

/// One hang-guard deadline per wait: every close lands before it.
const DEADLINE: Duration = Duration::from_secs(5);

/// How long one read inside a multi-line wait blocks, so the reader
/// notices the wait's end within this bound instead of one more
/// [`DEADLINE`].
const SLICE: Duration = Duration::from_secs(1);

const SID: &str = "s_0123456789abcdef";
const OTHER: &str = "s_ffffffffffffffff";
const NEXT: &str = "s_eeeeeeeeeeeeeeee";

/// A hub home under a held temporary directory.
struct Fixture {
    home: std::path::PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let held = fakes::TempDir::new(tag);
        let home = held.path().join("h");
        std::fs::create_dir_all(&home).unwrap();
        Self { home, held }
    }
}

/// A hub at `home` whose sessions start through `starter`.
fn hub(home: &std::path::Path, starter: FakeStarter) -> Arc<Hub> {
    let clock = fakes::clock::FakeClock::new();
    let timed: Arc<dyn contract::clock::Clock> = clock;
    Arc::new(Hub::new(
        home,
        "0.0.0",
        Arc::new(starter),
        Arc::clone(&timed),
        crate::diag::Diag::open(home, timed),
    ))
}

/// A kept `subscribe` line with `id` at `level`.
fn subscribe(id: &str, level: &str) -> Map<String, Value> {
    json!({"id": id, "command": "subscribe", "args": {"level": level}})
        .as_object()
        .unwrap()
        .clone()
}

/// A durable `session_status` line for [`SID`] with `seq`, as the session
/// sends it, newline included.
fn durable(seq: u64, state: &str) -> String {
    let line = json!({"kind": "session_status", "session_id": SID, "seq": seq,
        "ts": 5, "schema_version": 1,
        "payload": {"name": "n", "workspace": "/w", "project": "-w", "model": "p/m",
            "state": state, "since": 7,
            "spend": {"tokens": {"input": 0, "cache_read": 0, "cache_write": {},
                "output": 0}, "cost": 0.0, "subscription_cost": 0.0},
            "delegates": 0, "jobs": 0, "clients": 0}});
    format!("{}\n", serde_json::to_string(&line).unwrap())
}

/// Writes `id`'s log: a `session_started` recording `workspace`, then each
/// of `last` in order.
fn write_log(home: &std::path::Path, id: &str, workspace: &str, last: &[Value]) {
    let dir = home.join("projects").join("-w").join("sessions").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = format!(
        "{}\n",
        json!({"kind": "session_started", "session_id": id,
            "payload": {"workspace": workspace}})
    );
    for line in last {
        text.push_str(&format!("{}\n", serde_json::to_string(line).unwrap()));
    }
    std::fs::write(dir.join("events.jsonl"), text).unwrap();
}

fn fiber_exited(id: &str) -> Value {
    json!({"kind": "fiber_exited", "session_id": id, "ts": 3, "schema_version": 1,
        "payload": {}})
}

fn rewound(id: &str, next: &str) -> Value {
    json!({"kind": "rewound", "session_id": id, "ts": 3, "schema_version": 1,
        "payload": {"new_session_id": next, "seq": 3, "jobs": []}})
}

/// What one relay under test owns: its hub, its connection's relays, the
/// client writer, its epoch and its shared unacknowledged commands.
struct Rig {
    hub: Arc<Hub>,
    relays: Arc<Mutex<Relays>>,
    client: Arc<Mutex<UnixStream>>,
    epoch: u64,
    kept: Kept,
}

/// A connection's relays with one entry for `session`, the client pair and
/// the session pair: the entry's writer and the thread's reader share the
/// hub side, so dropping the session peer ends the relay's read and fails
/// the entry's write. With `park`, the entry holds a parked live thread,
/// so a routed command queues on it instead of recovering; dropping the
/// returned sender releases it.
fn rig(
    home: &std::path::Path,
    starter: FakeStarter,
    session: &str,
    park: bool,
) -> (
    Rig,
    UnixStream,
    UnixStream,
    UnixStream,
    Option<mpsc::Sender<()>>,
) {
    let hub = hub(home, starter);
    let relays = Arc::new(Mutex::new(Relays::default()));
    let epoch = lock(&relays).mint();
    let kept: Kept = Arc::default();
    let (client_hub, client_peer) = UnixStream::pair().unwrap();
    let client = Arc::new(Mutex::new(client_hub));
    let (hub_side, session_peer) = UnixStream::pair().unwrap();
    let reader = hub_side.try_clone().unwrap();
    let entry = hub_side;
    let (thread, park_tx) = park
        .then(|| {
            let (park_tx, park_rx) = mpsc::channel::<()>();
            let parked = thread::Builder::new()
                .name("parked-relay".to_owned())
                .spawn(move || {
                    let _released = park_rx.recv();
                })
                .unwrap();
            (parked, park_tx)
        })
        .unzip();
    lock(&relays).entries.push(Relay {
        session: session.to_owned(),
        epoch,
        writer: entry,
        kept: Arc::clone(&kept),
        replayed: Replayed::default(),
        thread,
        retiring: None,
    });
    let rig = Rig {
        hub,
        relays,
        client,
        epoch,
        kept,
    };
    (rig, client_peer, session_peer, reader, park_tx)
}

/// Starts the relay thread for `rig`'s entry, reading `reader`: the test
/// holds the session peer and the client peer.
fn spawn(rig: &Rig, session: &str, reader: UnixStream) -> thread::JoinHandle<()> {
    crate::relay::spawn_for_test(
        session.to_owned(),
        rig.epoch,
        reader,
        Arc::clone(&rig.hub),
        Arc::clone(&rig.client),
        Arc::clone(&rig.relays),
        Arc::clone(&rig.kept),
    )
}

/// Joins `thread` on a thread of its own: code that blocks is a wait, so
/// the join is received with the deadline.
fn join(thread: thread::JoinHandle<()>, what: &str) {
    let (done_tx, done) = mpsc::channel();
    thread::spawn(move || {
        thread.join().unwrap();
        done_tx.send(()).unwrap_or(());
    });
    assert!(done.recv_timeout(DEADLINE).is_ok(), "{what}");
}

/// Reads `reader` to the first line `done` accepts: a scoped thread reads
/// the lines in short slices, and this takes them with one `recv_timeout`
/// for the whole wait, never one deadline per line. Raw lines, newlines
/// included. A closed socket fails naming `what` too.
fn collect_until(
    reader: UnixStream,
    what: &str,
    mut done: impl FnMut(&Value) -> bool + Send,
) -> Vec<String> {
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let (lines_tx, lines) = mpsc::channel();
        let stop = &stop;
        scope.spawn(move || {
            let mut reader = BufReader::new(reader);
            reader.get_ref().set_read_timeout(Some(SLICE)).unwrap();
            let mut got = Vec::new();
            loop {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let mut text = String::new();
                match reader.read_line(&mut text) {
                    Ok(_) if text.is_empty() => {
                        lines_tx.send(None).unwrap_or(());
                        return;
                    }
                    Ok(_) => {
                        let line: Value = serde_json::from_str(text.trim_end()).unwrap();
                        let stop_here = done(&line);
                        got.push(text);
                        if stop_here {
                            lines_tx.send(Some(got)).unwrap_or(());
                            return;
                        }
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::TimedOut
                            || error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => {
                        lines_tx.send(None).unwrap_or(());
                        return;
                    }
                }
            }
        });
        let got = lines.recv_timeout(DEADLINE);
        stop.store(true, Ordering::SeqCst);
        match got {
            Ok(Some(got)) => got,
            Ok(None) => panic!("the hub closed before {what}"),
            Err(_) => panic!("never received {what}"),
        }
    })
}

/// Reads `reader` to end of file on a thread of its own, received with one
/// deadline for the whole wait: for asserting nothing more arrives. The
/// caller drops every writer once the relay thread joined, so the end
/// always comes.
fn eof_reader(reader: UnixStream) -> mpsc::Receiver<Vec<String>> {
    let (lines_tx, lines) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut got = Vec::new();
        loop {
            let mut text = String::new();
            match reader.read_line(&mut text) {
                Ok(_) if text.is_empty() => break,
                Ok(_) => got.push(text),
                Err(_) => break,
            }
        }
        lines_tx.send(got).unwrap_or(());
    });
    lines
}

/// Every line already waiting on `peer`: never blocks.
fn drain_now(peer: &UnixStream) -> Vec<String> {
    peer.set_nonblocking(true).unwrap();
    let mut reader = BufReader::new(peer);
    let mut got = Vec::new();
    loop {
        let mut text = String::new();
        match reader.read_line(&mut text) {
            Ok(_) if text.is_empty() => break,
            Ok(_) => got.push(text),
            Err(_) => break,
        }
    }
    peer.set_nonblocking(false).unwrap_or(());
    got
}

/// Whether `text` acknowledges `command`.
fn is_ack(text: &str, command: &str) -> bool {
    serde_json::from_str::<Value>(text.trim_end()).is_ok_and(|line| {
        line.get("kind").and_then(Value::as_str) == Some("command_accepted")
            && line
                .get("payload")
                .and_then(|payload| payload.get("command_id"))
                .and_then(Value::as_str)
                == Some(command)
    })
}

/// The `id` of the hub command `text` carries, when it parses.
fn id_of(text: &str) -> String {
    serde_json::from_str::<Value>(text.trim_end())
        .ok()
        .and_then(|line| line.get("id").cloned())
        .and_then(|id| id.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Asserts `lines` holds the two durable lines byte for byte, then one
/// `stream_closed` for [`SID`] with `session_id` only in the payload, no
/// `seq` and no envelope `session_id`.
fn assert_stream_closed(lines: &[String], first: &str, second: &str, ts: u64) {
    assert_eq!(lines.len(), 3, "two lines, then stream_closed: {lines:?}");
    assert_eq!(lines[0], first, "the first line arrives byte for byte");
    assert_eq!(lines[1], second, "the second line arrives byte for byte");
    let closed: Value = serde_json::from_str(lines[2].trim_end()).unwrap();
    assert_eq!(closed["kind"], "stream_closed", "{closed}");
    assert_eq!(
        closed["payload"],
        json!({"session_id": SID}),
        "payload holds session_id only: {closed}"
    );
    assert_eq!(closed["schema_version"], 1, "{closed}");
    assert_eq!(closed["ts"], ts, "{closed}");
    assert!(closed.get("seq").is_none(), "no seq: {closed}");
    assert!(
        closed.get("session_id").is_none(),
        "no envelope session_id: {closed}"
    );
}

/// Routes `stripped` for [`SID`] on a thread of its own, received with the
/// deadline: code that blocks is a wait too.
fn route_on_thread(rig: &Rig, id: &str, stripped: Map<String, Value>) {
    let (done_tx, done) = mpsc::channel();
    let hub = Arc::clone(&rig.hub);
    let writer = Arc::clone(&rig.client);
    let relays = Arc::clone(&rig.relays);
    let id = id.to_owned();
    thread::spawn(move || {
        crate::relay::route(
            &contract::CommandId(id),
            SID,
            stripped,
            &hub,
            &writer,
            &relays,
            None,
            false,
        );
        done_tx.send(()).unwrap_or(());
    });
    assert!(
        done.recv_timeout(DEADLINE).is_ok(),
        "the routed command is queued before its deadline"
    );
}

fn tools(id: &str) -> Map<String, Value> {
    json!({"id": id, "command": "tools", "args": {}})
        .as_object()
        .unwrap()
        .clone()
}

#[test]
fn a_running_session_that_closes_its_relay_sends_stream_closed_after_every_line() {
    let fixture = Fixture::new("cs1");
    let bound = FakeSession::bind(&fixture.home, SID);
    let (rig, client_peer, mut session_peer, reader, _park) =
        rig(&fixture.home, FakeStarter::hang(&fixture.home), SID, false);
    lock(&rig.relays)
        .subscribed
        .push((SID.to_owned(), subscribe("c_sub", "full")));
    lock(&rig.relays)
        .subscribed
        .push((OTHER.to_owned(), subscribe("c_o", "summary")));
    let first = durable(1, "idle");
    let second = durable(2, "streaming");
    let thread = spawn(&rig, SID, reader);
    session_peer.write_all(first.as_bytes()).unwrap();
    session_peer.write_all(second.as_bytes()).unwrap();
    session_peer.flush().unwrap();
    drop(session_peer);
    let lines = collect_until(
        client_peer,
        "stream_closed for the closed session",
        |line| line.get("kind").and_then(Value::as_str) == Some("stream_closed"),
    );
    assert_stream_closed(&lines, &first, &second, wall_ms(rig.hub.clock.wall()));
    join(thread, "the relay thread to end");
    assert_eq!(
        lock(&rig.relays).subscription(SID),
        None,
        "the closed session loses its kept level"
    );
    assert_eq!(
        lock(&rig.relays).subscription(OTHER),
        Some(subscribe("c_o", "summary")),
        "another session's level is untouched"
    );
    drop(bound);
}

#[test]
fn a_session_whose_socket_refuses_keeps_the_level() {
    let fixture = Fixture::new("cs2");
    let bound = FakeSession::bind(&fixture.home, SID);
    let (rig, client_peer, mut session_peer, reader, _park) =
        rig(&fixture.home, FakeStarter::hang(&fixture.home), SID, false);
    lock(&rig.relays)
        .subscribed
        .push((SID.to_owned(), subscribe("c_sub", "full")));
    let first = durable(1, "idle");
    let second = durable(2, "streaming");
    let thread = spawn(&rig, SID, reader);
    session_peer.write_all(first.as_bytes()).unwrap();
    session_peer.write_all(second.as_bytes()).unwrap();
    session_peer.flush().unwrap();
    // The process died: the socket file stays but refuses connections.
    bound.kill();
    drop(session_peer);
    let eof = eof_reader(client_peer);
    join(thread, "the relay thread to end");
    drop(rig.client);
    drop(rig.hub);
    let lines = eof
        .recv_timeout(DEADLINE)
        .expect("the client read ends after the relay");
    assert_eq!(
        lines,
        [first, second],
        "only the two lines arrive, never stream_closed"
    );
    assert_eq!(
        lock(&rig.relays).subscription(SID),
        Some(subscribe("c_sub", "full")),
        "the crashed session keeps its level"
    );
}

#[test]
fn a_log_ending_fiber_exited_keeps_the_level() {
    let fixture = Fixture::new("cs3");
    let bound = FakeSession::bind(&fixture.home, SID);
    write_log(&fixture.home, SID, "/w", &[fiber_exited(SID)]);
    let (rig, client_peer, mut session_peer, reader, _park) =
        rig(&fixture.home, FakeStarter::hang(&fixture.home), SID, false);
    lock(&rig.relays)
        .subscribed
        .push((SID.to_owned(), subscribe("c_sub", "full")));
    let first = durable(1, "idle");
    let second = durable(2, "streaming");
    let thread = spawn(&rig, SID, reader);
    session_peer.write_all(first.as_bytes()).unwrap();
    session_peer.write_all(second.as_bytes()).unwrap();
    session_peer.flush().unwrap();
    drop(session_peer);
    let eof = eof_reader(client_peer);
    join(thread, "the relay thread to end");
    drop(rig.client);
    drop(rig.hub);
    let lines = eof
        .recv_timeout(DEADLINE)
        .expect("the client read ends after the relay");
    assert_eq!(
        lines,
        [first, second],
        "only the two lines arrive, never stream_closed"
    );
    assert_eq!(
        lock(&rig.relays).subscription(SID),
        Some(subscribe("c_sub", "full")),
        "the exited session keeps its level"
    );
    drop(bound);
}

#[test]
fn a_log_ending_rewound_keeps_the_level() {
    let fixture = Fixture::new("cs4");
    let bound = FakeSession::bind(&fixture.home, SID);
    write_log(&fixture.home, SID, "/w", &[rewound(SID, NEXT)]);
    let (rig, client_peer, mut session_peer, reader, _park) = rig(
        &fixture.home,
        FakeStarter::bind_and_hold(&fixture.home),
        SID,
        false,
    );
    lock(&rig.relays)
        .subscribed
        .push((SID.to_owned(), subscribe("c_sub", "full")));
    let first = durable(1, "idle");
    let second = durable(2, "streaming");
    let thread = spawn(&rig, SID, reader);
    session_peer.write_all(first.as_bytes()).unwrap();
    session_peer.write_all(second.as_bytes()).unwrap();
    session_peer.flush().unwrap();
    drop(session_peer);
    let mut seen = 0;
    let lines = collect_until(client_peer, "the two relayed lines", |_| {
        seen += 1;
        seen == 2
    });
    assert_eq!(lines, [first, second], "only the two lines arrive");
    join(thread, "the relay thread to end");
    assert_eq!(
        lock(&rig.relays).subscription(SID),
        Some(subscribe("c_sub", "full")),
        "the rewound session keeps its level"
    );
    drop(bound);
}

#[test]
fn a_relay_that_saw_the_exited_window_sends_no_stream_closed() {
    let fixture = Fixture::new("cs5");
    let starter = FakeStarter::bind_hold_and_append_started(&fixture.home);
    let (rig, client_peer, mut session_peer, reader, _park) =
        rig(&fixture.home, starter.clone(), SID, false);
    write_log(&fixture.home, SID, "/w", &[fiber_exited(SID)]);
    lock(&rig.relays)
        .subscribed
        .push((SID.to_owned(), subscribe("c_sub", "full")));
    lock(&rig.kept).push(("c_x".to_owned(), tools("c_x"), true));
    lock(&rig.relays).order.enqueue(SID, "c_x");
    let thread = spawn(&rig, SID, reader);
    let closing = json!({"kind": "command_rejected",
        "payload": {"command_id": "c_x", "code": "closing",
            "message": "The session is closing."}});
    session_peer
        .write_all(format!("{closing}\n").as_bytes())
        .unwrap();
    session_peer.flush().unwrap();
    assert!(
        starter.await_received(2, DEADLINE),
        "the rerouted command reached the resumed session"
    );
    drop(session_peer);
    let lines = collect_until(client_peer, "the rerouted acknowledgement", |line| {
        line.get("kind").and_then(Value::as_str) == Some("command_accepted")
            && line
                .get("payload")
                .and_then(|payload| payload.get("command_id"))
                .and_then(Value::as_str)
                == Some("c_x")
    });
    assert!(
        lines.iter().all(|text| !text.contains("stream_closed")),
        "no stream_closed arrives: {lines:?}"
    );
    join(thread, "the relay thread to end");
    assert_eq!(
        lock(&rig.relays).subscription(SID),
        Some(subscribe("c_sub", "full")),
        "the re-routed session keeps its level"
    );
}

#[test]
fn a_command_racing_buffered_lines_waits_for_the_end_and_reaches_the_session_unsubscribed() {
    let fixture = Fixture::new("cs6a");
    let starter = FakeStarter::bind_and_hold(&fixture.home);
    starter
        .resume(&SessionId(SID.to_owned()), std::path::Path::new("/w"))
        .unwrap();
    let (rig, client_peer, mut session_peer, reader, park_tx) =
        rig(&fixture.home, starter.clone(), SID, true);
    lock(&rig.relays)
        .subscribed
        .push((SID.to_owned(), subscribe("c_sub", "full")));
    // The session's last lines sit unread in the socket buffer.
    let first = durable(1, "idle");
    let second = durable(2, "streaming");
    session_peer.write_all(first.as_bytes()).unwrap();
    session_peer.write_all(second.as_bytes()).unwrap();
    session_peer.flush().unwrap();
    drop(session_peer);
    // The relay pauses before reading its first line, holding no lock.
    let (reached_tx, reached) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    *lock(&lock(&rig.relays).order.before_read) = Some(Box::new(move || {
        reached_tx.send(()).unwrap_or(());
        release.recv().unwrap_or(());
    }));
    let thread = spawn(&rig, SID, reader);
    assert!(
        reached.recv_timeout(DEADLINE).is_ok(),
        "the relay paused before its first read"
    );
    // The racing command's write fails: it queues unsent on the dead relay.
    route_on_thread(&rig, "c_t", tools("c_t"));
    drop(release_tx);
    let _ = release;
    let lines = collect_until(client_peer, "the racing command's answer", |line| {
        line.get("kind").and_then(Value::as_str) == Some("command_accepted")
            && line
                .get("payload")
                .and_then(|payload| payload.get("command_id"))
                .and_then(Value::as_str)
                == Some("c_t")
    });
    assert_eq!(
        lines.len(),
        4,
        "two lines, stream_closed, then the answer: {lines:?}"
    );
    assert_stream_closed(&lines[..3], &first, &second, wall_ms(rig.hub.clock.wall()));
    assert!(
        is_ack(&lines[3], "c_t"),
        "the racing command is answered: {}",
        lines[3]
    );
    assert!(
        !lines[3].contains("\"seq\""),
        "no durable line follows stream_closed: {}",
        lines[3]
    );
    let carrying = starter
        .received_by_connection()
        .into_iter()
        .find(|lines| lines.iter().any(|text| id_of(text) == "c_t"))
        .expect("the racing command reached a session connection");
    assert_eq!(
        carrying.len(),
        1,
        "no replayed subscribe goes first: {carrying:?}"
    );
    assert_eq!(id_of(&carrying[0]), "c_t", "{carrying:?}");
    join(thread, "the relay thread to end");
    assert_eq!(
        lock(&rig.relays).subscription(SID),
        None,
        "the closed session loses its kept level"
    );
    drop(park_tx);
}

#[test]
fn a_command_racing_the_end_is_answered_after_stream_closed() {
    let fixture = Fixture::new("cs6b");
    let starter = FakeStarter::bind_and_hold(&fixture.home);
    starter
        .resume(&SessionId(SID.to_owned()), std::path::Path::new("/w"))
        .unwrap();
    let (rig, client_peer, session_peer, reader, park_tx) =
        rig(&fixture.home, starter.clone(), SID, true);
    lock(&rig.relays)
        .subscribed
        .push((SID.to_owned(), subscribe("c_sub", "full")));
    // The relay pauses in `on_end` after the decision, before the drop.
    let (reached_tx, reached) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    lock(&rig.relays).at_close = Some(Box::new(move || {
        reached_tx.send(()).unwrap_or(());
        release.recv().unwrap_or(());
    }));
    drop(session_peer);
    let thread = spawn(&rig, SID, reader);
    assert!(
        reached.recv_timeout(DEADLINE).is_ok(),
        "the relay paused at its end"
    );
    // The racing command's write fails: it queues unsent on the dead relay.
    route_on_thread(&rig, "c_t", tools("c_t"));
    drop(release_tx);
    let _ = release;
    let lines = collect_until(client_peer, "the racing command's answer", |line| {
        line.get("kind").and_then(Value::as_str) == Some("command_accepted")
            && line
                .get("payload")
                .and_then(|payload| payload.get("command_id"))
                .and_then(Value::as_str)
                == Some("c_t")
    });
    assert_eq!(lines.len(), 2, "stream_closed, then the answer: {lines:?}");
    let closed: Value = serde_json::from_str(lines[0].trim_end()).unwrap();
    assert_eq!(closed["kind"], "stream_closed", "{closed}");
    assert_eq!(closed["payload"], json!({"session_id": SID}), "{closed}");
    assert!(
        is_ack(&lines[1], "c_t"),
        "the racing command is answered: {}",
        lines[1]
    );
    let carrying = starter
        .received_by_connection()
        .into_iter()
        .find(|lines| lines.iter().any(|text| id_of(text) == "c_t"))
        .expect("the racing command reached a session connection");
    assert_eq!(
        carrying.len(),
        1,
        "no replayed subscribe goes first: {carrying:?}"
    );
    assert_eq!(id_of(&carrying[0]), "c_t", "{carrying:?}");
    join(thread, "the relay thread to end");
    assert_eq!(
        lock(&rig.relays).subscription(SID),
        None,
        "the closed session loses its kept level"
    );
    drop(park_tx);
}

#[test]
fn a_client_that_left_keeps_its_level() {
    let fixture = Fixture::new("cs7");
    let bound = FakeSession::bind(&fixture.home, SID);
    let (rig, client_peer, mut session_peer, reader, _park) =
        rig(&fixture.home, FakeStarter::hang(&fixture.home), SID, false);
    lock(&rig.relays)
        .subscribed
        .push((SID.to_owned(), subscribe("c_sub", "full")));
    // The client is gone: the forward fails and the relay ends unclosed.
    drop(client_peer);
    let thread = spawn(&rig, SID, reader);
    session_peer
        .write_all(durable(1, "idle").as_bytes())
        .unwrap();
    session_peer.flush().unwrap();
    drop(session_peer);
    join(thread, "the relay thread to end");
    assert_eq!(
        lock(&rig.relays).subscription(SID),
        Some(subscribe("c_sub", "full")),
        "the level stays when the client left"
    );
    drop(bound);
}

#[test]
fn a_disconnected_connection_skips_the_check() {
    let fixture = Fixture::new("cs8");
    let bound = FakeSession::bind(&fixture.home, SID);
    let (rig, client_peer, _session_peer, reader, _park) =
        rig(&fixture.home, FakeStarter::hang(&fixture.home), SID, false);
    lock(&rig.relays)
        .subscribed
        .push((SID.to_owned(), subscribe("c_sub", "full")));
    let thread = spawn(&rig, SID, reader);
    lock(&rig.relays).close_all();
    join(thread, "the relay thread to end");
    assert_eq!(
        lock(&rig.relays).subscription(SID),
        Some(subscribe("c_sub", "full")),
        "the disconnected connection keeps its level"
    );
    assert!(
        drain_now(&client_peer).is_empty(),
        "the gone client receives nothing"
    );
    drop(bound);
}
