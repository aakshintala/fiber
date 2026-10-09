//! Tests for rejoining a resumed session at the level the connection last
//! held: the per-connection duplicate guard, the incremental log read, and
//! the sweep off the feed's rescan.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::*;
use crate::connection::{Hub, lock, serve_connection};
use crate::diag::Diag;
use crate::fake::FakeStarter;
use crate::feed::RUN_SCAN;

/// One named deadline per wait: every rejoin lands before it.
const DEADLINE: Duration = Duration::from_secs(5);

/// How long, in real time, the registry wait holds a gone connection: the
/// serving thread unregisters after its read loop sees EOF, so the test
/// waits on the hub's tick instead of asserting immediately.
const UNREGISTERED: Duration = Duration::from_secs(3);

/// How long one read inside a multi-line wait blocks, so the reader
/// notices the wait's end within this bound instead of one more
/// [`DEADLINE`], as `SLICE` in `crates/doors/tests/socket.rs` does.
const SLICE: Duration = Duration::from_secs(1);
struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hj");
        let dir = held.path().join("h");
        fs::create_dir_all(dir.join("run")).unwrap();
        Self { dir, held }
    }

    fn hub(&self, starter: impl crate::Starter + 'static) -> (Arc<Hub>, Arc<FakeClock>) {
        let clock = FakeClock::new();
        let timed: Arc<dyn contract::clock::Clock> =
            Arc::clone(&clock) as Arc<dyn contract::clock::Clock>;
        let hub = Arc::new(Hub::new(
            &self.dir,
            "0.0.0",
            Arc::new(starter),
            Arc::clone(&timed),
            Diag::open(&self.dir, timed),
        ));
        (hub, clock)
    }

    fn workspace(&self) -> String {
        let workspace = self.dir.join("w");
        fs::create_dir_all(&workspace).unwrap();
        workspace.to_string_lossy().into_owned()
    }

    fn log_dir(&self, id: &str) -> PathBuf {
        self.dir
            .join("projects")
            .join("-w")
            .join("sessions")
            .join(id)
    }

    /// Writes `id`'s log: a `session_started` recording `workspace`, then
    /// each of `last` in order.
    fn write_log(&self, id: &str, workspace: &str, parent: Option<&str>, last: &[Value]) {
        fs::create_dir_all(self.log_dir(id)).unwrap();
        let mut text = format!("{}\n", started_line(id, workspace, parent));
        for line in last {
            text.push_str(&format!("{}\n", serde_json::to_string(line).unwrap()));
        }
        fs::write(self.log_dir(id).join("events.jsonl"), text).unwrap();
    }

    fn append(&self, id: &str, line: &Value) {
        let mut text = fs::read_to_string(self.log_dir(id).join("events.jsonl")).unwrap();
        text.push_str(&format!("{}\n", serde_json::to_string(line).unwrap()));
        fs::write(self.log_dir(id).join("events.jsonl"), text).unwrap();
    }

    /// Appends raw text without its newline: a torn tail.
    fn append_raw(&self, id: &str, text: &str) {
        let mut log = fs::read(self.log_dir(id).join("events.jsonl")).unwrap();
        log.extend_from_slice(text.as_bytes());
        fs::write(self.log_dir(id).join("events.jsonl"), log).unwrap();
    }
}

fn id(n: u64) -> String {
    format!("s_{n:016x}")
}

fn started_line(id: &str, workspace: &str, parent: Option<&str>) -> String {
    let mut payload = json!({"workspace": workspace});
    if let Some(parent) = parent {
        payload["parent"] = Value::String(parent.to_owned());
    }
    serde_json::to_string(&json!({
        "kind": "session_started", "session_id": id, "ts": 1, "schema_version": 1,
        "payload": payload,
    }))
    .unwrap()
}

fn fiber_started(id: &str) -> Value {
    json!({"kind": "fiber_started", "session_id": id, "ts": 2, "schema_version": 1, "payload": {}})
}

fn fiber_exited(id: &str) -> Value {
    json!({"kind": "fiber_exited", "session_id": id, "ts": 3, "schema_version": 1, "payload": {}})
}

fn rewound(id: &str, next: &str) -> Value {
    json!({"kind": "rewound", "session_id": id, "ts": 3, "schema_version": 1,
        "payload": {"new_session_id": next, "seq": 3, "jobs": []}})
}

/// A `session_status` payload: `state` unless said otherwise; `parent`
/// marks a delegate.
fn payload(state: &str, parent: Option<&str>) -> Value {
    let usage = json!({
        "tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0},
        "cost": 0.0, "subscription_cost": 0.0,
    });
    let mut payload = json!({
        "name": "n", "workspace": "/w", "project": "-w", "model": "p/m",
        "state": state, "since": 7, "spend": usage,
        "delegates": 0, "jobs": 0, "clients": 0,
    });
    if let Some(parent) = parent {
        payload["parent"] = Value::String(parent.to_owned());
    }
    payload
}

/// A `session_status` line for session `id`, as the session sends it.
fn status_line(id: &str, state: &str, parent: Option<&str>) -> String {
    let line = json!({
        "kind": "session_status", "session_id": id, "ts": 5, "schema_version": 1,
        "payload": payload(state, parent),
    });
    format!("{}\n", serde_json::to_string(&line).unwrap())
}

/// Whether `line` is a hub-minted `subscribe`: an id of the hub's own, not
/// the client's and not the feed's.
fn is_rejoin(line: &str) -> bool {
    let Ok(line): Result<Value, _> = serde_json::from_str(line) else {
        return false;
    };
    line.get("command").and_then(Value::as_str) == Some("subscribe")
        && line
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.len() == 18 && id.starts_with("c_"))
}

/// One side of a client connection: the hub serves the other end.
struct Client {
    write: UnixStream,
    read: BufReader<UnixStream>,
}

impl Client {
    fn connect(hub: &Arc<Hub>) -> Self {
        let (a, b) = UnixStream::pair().unwrap();
        let hub = Arc::clone(hub);
        thread::Builder::new()
            .name("hub-conn".to_owned())
            .spawn(move || serve_connection(a, hub))
            .unwrap();
        b.set_read_timeout(Some(DEADLINE)).unwrap();
        b.set_write_timeout(Some(DEADLINE)).unwrap();
        let reader = b.try_clone().unwrap();
        Self {
            write: b,
            read: BufReader::new(reader),
        }
    }

    fn send(&mut self, line: &Value) {
        let mut bytes = serde_json::to_vec(line).unwrap();
        bytes.push(b'\n');
        self.write.write_all(&bytes).unwrap();
        self.write.flush().unwrap();
    }

    fn next(&mut self, what: &str) -> Value {
        self.read
            .get_ref()
            .set_read_timeout(Some(DEADLINE))
            .unwrap();
        let mut text = String::new();
        self.read
            .read_line(&mut text)
            .unwrap_or_else(|_| panic!("never received {what}"));
        assert!(!text.is_empty(), "the hub closed before {what}");
        serde_json::from_str(&text).unwrap()
    }

    /// The first line `done` accepts: a scoped thread reads the lines in
    /// short slices and sends it over a channel, and this takes it with
    /// one `recv_timeout` for the whole wait, never one deadline per
    /// line. A closed socket sends nothing, failing naming `what` too.
    fn wait_for(&mut self, what: &str, mut done: impl FnMut(&Value) -> bool + Send) -> Value {
        let stop = AtomicBool::new(false);
        thread::scope(|scope| {
            let (tx, rx) = mpsc::channel();
            let stop = &stop;
            scope.spawn(move || {
                self.read.get_ref().set_read_timeout(Some(SLICE)).unwrap();
                loop {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let mut text = String::new();
                    match self.read.read_line(&mut text) {
                        Ok(_) if text.is_empty() => {
                            tx.send(None).unwrap_or(());
                            return;
                        }
                        Ok(_) => {
                            let line: Value = serde_json::from_str(&text).unwrap();
                            if done(&line) {
                                tx.send(Some(line)).unwrap_or(());
                                return;
                            }
                        }
                        Err(error)
                            if error.kind() == std::io::ErrorKind::TimedOut
                                || error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => {
                            tx.send(None).unwrap_or(());
                            return;
                        }
                    }
                }
            });
            let got = rx.recv_timeout(DEADLINE);
            stop.store(true, Ordering::SeqCst);
            match got {
                Ok(Some(line)) => line,
                Ok(None) => panic!("the hub closed before {what}"),
                Err(_) => panic!("never received {what}"),
            }
        })
    }

    /// The next `session_status`, skipping `attention` hub lines.
    fn next_status(&mut self, what: &str) -> Value {
        self.wait_for(what, |line| {
            line.get("kind").and_then(Value::as_str) == Some("session_status")
        })
    }

    /// The acknowledgement for command `id`, skipping statuses and
    /// `attention` other sessions' relays forward meanwhile.
    fn next_ack(&mut self, id: &str, what: &str) -> Value {
        self.wait_for(what, |line| {
            line.get("kind").and_then(Value::as_str) == Some("command_accepted")
                && line["payload"]["command_id"] == id
        })
    }

    /// A line if one is already waiting, else `None`: never blocks.
    fn try_next(&mut self) -> Option<Value> {
        self.read.get_ref().set_nonblocking(true).unwrap();
        let mut text = String::new();
        let got = match self.read.read_line(&mut text) {
            Ok(_) if text.is_empty() => None,
            Ok(_) => Some(serde_json::from_str(&text).unwrap()),
            Err(_) => None,
        };
        self.read.get_ref().set_nonblocking(false).unwrap();
        got
    }

    /// Every line already waiting: never blocks.
    fn drain(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(line) = self.try_next() {
            out.push(line);
        }
        out
    }
}

/// A fake running session at `run/<id>`: each connection's lines are
/// recorded and every line answered, `command_accepted`, or
/// `command_rejected` for a `subscribe` when rejecting. `say` queues a
/// line for every subscriber now and every later one. `shutdown_write`
/// ends the relay's read without unlinking; `await_closed` waits until
/// every connection's far end went away, proving the relay thread dropped
/// its entry.
struct Fake {
    socket: PathBuf,
    shared: Arc<FakeShared>,
    accept: Mutex<Option<thread::JoinHandle<()>>>,
}

struct FakeShared {
    state: Mutex<FakeState>,
    changed: Condvar,
}

struct FakeState {
    said: Vec<String>,
    conns: Vec<ConnRec>,
    sinks: Vec<UnixStream>,
    closed: bool,
    reject_subscribe: bool,
}

struct ConnRec {
    lines: Vec<String>,
    eof: bool,
}

impl Fake {
    fn bind(home: &Path, id: &str) -> Self {
        Self::bind_rejecting(home, id, false)
    }

    fn bind_rejecting(home: &Path, id: &str, reject_subscribe: bool) -> Self {
        let run = home.join("run");
        fs::create_dir_all(&run).unwrap();
        let socket = run.join(id);
        let shared = Arc::new(FakeShared {
            state: Mutex::new(FakeState {
                said: Vec::new(),
                conns: Vec::new(),
                sinks: Vec::new(),
                closed: false,
                reject_subscribe,
            }),
            changed: Condvar::new(),
        });
        let accept = UnixListener::bind(&socket).ok().and_then(|listener| {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("fake-rejoin-session".to_owned())
                .spawn(move || {
                    loop {
                        let Ok((stream, _)) = listener.accept() else {
                            return;
                        };
                        if lock(&shared.state).closed {
                            return;
                        }
                        let shared = Arc::clone(&shared);
                        thread::Builder::new()
                            .name("fake-rejoin-conn".to_owned())
                            .spawn(move || serve(stream, &shared))
                            .map(drop)
                            .unwrap_or(());
                    }
                })
                .ok()
        });
        Self {
            socket,
            shared,
            accept: Mutex::new(accept),
        }
    }

    /// Sends `line` to every subscriber now and every later one.
    fn say(&self, line: &str) {
        let mut state = lock(&self.shared.state);
        state.said.push(line.to_owned());
        for sink in &mut state.sinks {
            sink.write_all(line.as_bytes()).unwrap_or(());
            sink.flush().unwrap_or(());
        }
    }

    /// Stops the listener; the socket file stays until `unlink`. The
    /// wake-up connect and the join run on a thread whose end this takes
    /// with one `recv_timeout`, so a stalled listener fails the test
    /// instead of reaching the runner's kill.
    fn stop_listening(&self) {
        {
            lock(&self.shared.state).closed = true;
        }
        let socket = self.socket.clone();
        let accept = lock(&self.accept).take();
        let (stopped_tx, stopped) = mpsc::channel();
        thread::Builder::new()
            .name("fake-rejoin-stop".to_owned())
            .spawn(move || {
                drop(UnixStream::connect(&socket));
                if let Some(accept) = accept {
                    accept.join().unwrap_or(());
                }
                stopped_tx.send(()).unwrap_or(());
            })
            .unwrap();
        assert!(
            stopped.recv_timeout(DEADLINE).is_ok() || thread::panicking(),
            "the fake session's listener never stopped"
        );
    }

    fn unlink(&self) {
        fs::remove_file(&self.socket).unwrap_or(());
    }

    /// Ends the relay's read on every connection: the relay thread sees
    /// EOF. The socket file and listener are untouched.
    fn shutdown_write(&self) {
        for sink in lock(&self.shared.state).sinks.iter_mut() {
            sink.shutdown(std::net::Shutdown::Write).unwrap_or(());
        }
    }

    /// Waits until every connection so far saw its far end go away.
    fn await_closed(&self) {
        let state = lock(&self.shared.state);
        let (state, _) = self
            .shared
            .changed
            .wait_timeout_while(state, DEADLINE, |state| {
                state.conns.iter().any(|conn| !conn.eof)
            })
            .unwrap();
        assert!(
            state.conns.iter().all(|conn| conn.eof),
            "the relay never dropped its entry: {}",
            conns_seen(&state)
        );
    }

    /// The first connection whose lines and end match `pred`, with its lines.
    fn await_conn_where(
        &self,
        mut pred: impl FnMut(&[String], bool) -> bool,
        what: &str,
    ) -> (usize, Vec<String>) {
        let state = lock(&self.shared.state);
        let (state, _) = self
            .shared
            .changed
            .wait_timeout_while(state, DEADLINE, |state| {
                !state.conns.iter().any(|conn| pred(&conn.lines, conn.eof))
            })
            .unwrap();
        let (at, conn) = state
            .conns
            .iter()
            .enumerate()
            .find(|(_, conn)| pred(&conn.lines, conn.eof))
            .unwrap_or_else(|| panic!("never received {what}: {}", conns_seen(&state)));
        (at, conn.lines.clone())
    }

    /// Every connection's lines, for asserting absence after a pass.
    fn all_lines(&self) -> Vec<Vec<String>> {
        lock(&self.shared.state)
            .conns
            .iter()
            .map(|conn| conn.lines.clone())
            .collect()
    }

    /// How many connections' lines hold a hub-minted subscribe: the feed's
    /// own connection carries none.
    fn rejoin_count(&self) -> usize {
        self.all_lines()
            .iter()
            .filter(|lines| lines.iter().any(|line| is_rejoin(line)))
            .count()
    }

    /// Whether any connection's lines hold a hub-minted subscribe.
    fn has_rejoin(&self) -> bool {
        self.all_lines()
            .iter()
            .any(|lines| lines.iter().any(|line| is_rejoin(line)))
    }

    /// Exits: stops the listener, unlinks the socket, then shuts every
    /// connection, as a session's exit does.
    fn exit(&self) {
        self.stop_listening();
        self.unlink();
        for sink in lock(&self.shared.state).sinks.drain(..) {
            sink.shutdown(std::net::Shutdown::Both).unwrap_or(());
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.exit();
    }
}

/// Every connection's lines and whether it ended, for a failed wait.
fn conns_seen(state: &FakeState) -> String {
    state
        .conns
        .iter()
        .map(|conn| format!("{:?} ended={}", conn.lines, conn.eof))
        .collect::<Vec<_>>()
        .join("; ")
}

fn serve(stream: UnixStream, shared: &Arc<FakeShared>) {
    // Every write to the relay is bounded: `say` writes under the state
    // lock on the test's thread.
    stream.set_write_timeout(Some(DEADLINE)).unwrap_or(());
    let mut writer = stream.try_clone().unwrap();
    let mut read = BufReader::new(stream);
    let at = {
        let mut state = lock(&shared.state);
        if state.closed {
            return;
        }
        state.sinks.push(writer.try_clone().unwrap());
        state.conns.push(ConnRec {
            lines: Vec::new(),
            eof: false,
        });
        let at = state.conns.len() - 1;
        shared.changed.notify_all();
        at
    };
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&buf).into_owned();
                let command: Value = serde_json::from_str(&text).unwrap();
                let reject = command.get("command").and_then(Value::as_str) == Some("subscribe")
                    && lock(&shared.state).reject_subscribe;
                let ack = if reject {
                    json!({"kind": "command_rejected", "ts": 1, "schema_version": 1,
                        "payload": {"command_id": command["id"],
                        "code": "invalid_arguments", "message": "m"}})
                } else {
                    json!({"kind": "command_accepted", "ts": 1, "schema_version": 1,
                        "payload": {"command_id": command["id"]}})
                };
                let mut bytes = serde_json::to_vec(&ack).unwrap();
                bytes.push(b'\n');
                {
                    let mut state = lock(&shared.state);
                    if state.closed {
                        return;
                    }
                    state.conns[at].lines.push(text);
                    for line in &state.said {
                        writer.write_all(line.as_bytes()).unwrap_or(());
                    }
                    writer.write_all(&bytes).unwrap_or(());
                    writer.flush().unwrap_or(());
                    shared.changed.notify_all();
                }
            }
        }
    }
    {
        let mut state = lock(&shared.state);
        if let Some(conn) = state.conns.get_mut(at) {
            conn.eof = true;
        }
        shared.changed.notify_all();
    }
}

/// Waits until the scanner parks for the scan after now.
fn await_scanner(clock: &FakeClock) {
    assert!(
        clock.await_parked(clock.now() + RUN_SCAN, DEADLINE),
        "the scanner parks until the next scan"
    );
}

/// Starts `feed`, and waits until its scanner parks for the next scan.
fn start(feed: &Arc<crate::feed::Feed>, clock: &FakeClock) {
    feed.start();
    await_scanner(clock);
}

/// Arms the worker's pass-done notice; the caller waits on `done`.
fn arm_pass(hub: &Arc<Hub>) -> mpsc::Receiver<()> {
    let (tx, done) = mpsc::channel();
    *lock(&hub.rejoins.pass_done) = Some(tx);
    done
}

fn await_pass(done: &mpsc::Receiver<()>, what: &str) {
    assert!(
        done.recv_timeout(DEADLINE).is_ok(),
        "the worker's pass never ended for {what}"
    );
}

/// Waits, at most [`UNREGISTERED`] of real time, until the registry drops
/// every connection: a scoped thread waits on the hub's tick, which
/// `serve_connection` wakes after it unregisters, and this takes its
/// notice with one `recv_timeout`. The fake clock never moves during the
/// wait, so the bound is the channel's, never the clock's. Fails naming
/// `what` when the deadline passes first.
fn await_unregistered(hub: &Hub, clock: &FakeClock, what: &str) {
    let given_up = AtomicBool::new(false);
    thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        let given_up = &given_up;
        scope.spawn(move || {
            hub.tick.wait_for(clock, &mut |_| {
                if hub.rejoins.ids().is_empty() || given_up.load(Ordering::SeqCst) {
                    crate::connection::Wait::Done
                } else {
                    crate::connection::Wait::Until(None)
                }
            });
            tx.send(()).unwrap_or(());
        });
        if rx.recv_timeout(UNREGISTERED).is_err() {
            // The wake takes the tick lock, so the waiter sees the flag.
            given_up.store(true, Ordering::SeqCst);
            hub.tick.wake();
        }
    });
    assert!(
        hub.rejoins.ids().is_empty(),
        "the registry no longer holds it for {what}"
    );
}

/// A relay entry for `session` at `epoch`, with no thread.
fn entry(session: &str, epoch: u64) -> crate::relay::Relay {
    let (writer, _) = UnixStream::pair().unwrap();
    crate::relay::Relay {
        session: session.to_owned(),
        epoch,
        writer,
        kept: crate::relay::Kept::default(),
        replayed: crate::relay::Replayed::default(),
        thread: None,
        retiring: None,
    }
}

fn subscribe_line(id: &str, level: &str) -> Map<String, Value> {
    json!({"id": id, "command": "subscribe", "args": {"level": level}})
        .as_object()
        .unwrap()
        .clone()
}

fn mark_for(log: Option<PathBuf>, read: u64, live: bool, epoch: u64) -> Mark {
    Mark {
        log,
        read,
        live,
        epoch,
    }
}

#[test]
fn an_exclusive_admit_with_a_relay_for_the_session_gives_up() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = crate::relay::Relays::default();
    let epoch = relays.mint();
    relays.entries.push(entry(sid, epoch));
    let fresh = relays.mint();
    assert!(
        !admit(&mut relays, sid, true, mark_for(None, 0, false, 0), fresh),
        "an exclusive attach never duplicates a relay a client command opened"
    );
    assert!(
        !relays.rejoin.marks.contains_key(sid),
        "a refused attach records nothing"
    );
}

#[test]
fn an_exclusive_admit_while_the_session_is_opening_gives_up() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    let epoch = lock(&relays).mint();
    let _opening = Opening::mark(&mut lock(&relays), &relays, sid);
    assert!(
        !admit(
            &mut lock(&relays),
            sid,
            true,
            mark_for(None, 0, false, 0),
            epoch
        ),
        "an exclusive attach never opens a session a client command is opening"
    );
}

#[test]
fn an_exclusive_admit_after_close_gives_up() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = crate::relay::Relays::default();
    relays.rejoin.close();
    let epoch = relays.mint();
    assert!(
        !admit(&mut relays, sid, true, mark_for(None, 0, false, 0), epoch),
        "an exclusive attach never opens for a disconnected connection"
    );
}

#[test]
fn an_exclusive_admit_with_none_of_those_records_the_mark() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = crate::relay::Relays::default();
    let epoch = relays.mint();
    assert!(admit(
        &mut relays,
        sid,
        true,
        mark_for(None, 12, true, 0),
        epoch
    ));
    let stored = relays.rejoin.marks.get(sid).cloned().unwrap();
    assert_eq!(stored.read, 12);
    assert!(stored.live);
    assert_eq!(stored.epoch, epoch);
}

#[test]
fn a_shared_admit_with_a_relay_present_still_records() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = crate::relay::Relays::default();
    let epoch = relays.mint();
    relays.entries.push(entry(sid, epoch));
    let fresh = relays.mint();
    assert!(admit(
        &mut relays,
        sid,
        false,
        mark_for(None, 12, false, 0),
        fresh
    ));
    assert_eq!(relays.rejoin.marks.get(sid).unwrap().epoch, fresh);
}

#[test]
fn opening_marks_count_each_guard() {
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    let sid = "s_aaaaaaaaaaaaaaaa";
    let other = "s_bbbbbbbbbbbbbbbb";
    let first = Opening::mark(&mut lock(&relays), &relays, sid);
    let second = Opening::mark(&mut lock(&relays), &relays, sid);
    let unrelated = Opening::mark(&mut lock(&relays), &relays, other);
    let epoch = lock(&relays).mint();
    assert!(
        !admit(
            &mut lock(&relays),
            sid,
            true,
            mark_for(None, 0, false, 0),
            epoch
        ),
        "one dropped guard of two still holds the session opening"
    );
    drop(first);
    assert!(
        !admit(
            &mut lock(&relays),
            other,
            true,
            mark_for(None, 0, false, 0),
            epoch
        ),
        "a mark for another session never blocks this one"
    );
    assert!(
        admit(
            &mut lock(&relays),
            "s_cccccccccccccccc",
            true,
            mark_for(None, 0, false, 0),
            epoch
        ),
        "an unrelated session stays free"
    );
    drop(second);
    drop(unrelated);
    assert!(
        admit(
            &mut lock(&relays),
            sid,
            true,
            mark_for(None, 0, false, 0),
            epoch
        ),
        "both guards dropped frees the session"
    );
}

#[test]
fn dropping_one_of_two_openings_keeps_one_and_the_last_clears() {
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    let sid = "s_aaaaaaaaaaaaaaaa";
    let first = Opening::mark(&mut lock(&relays), &relays, sid);
    let second = Opening::mark(&mut lock(&relays), &relays, sid);
    assert_eq!(
        lock(&relays).rejoin.opening.get(sid).copied(),
        Some(2),
        "two marks count twice"
    );
    drop(first);
    assert_eq!(
        lock(&relays).rejoin.opening.get(sid).copied(),
        Some(1),
        "dropping one of two keeps exactly one, never clears"
    );
    let epoch = lock(&relays).mint();
    assert!(
        !admit(
            &mut lock(&relays),
            sid,
            true,
            mark_for(None, 0, false, 0),
            epoch
        ),
        "one guard of two still holds the session opening"
    );
    drop(second);
    assert!(
        !lock(&relays).rejoin.opening.contains_key(sid),
        "dropping the last guard removes its entry, never a zero"
    );
    assert!(
        admit(
            &mut lock(&relays),
            sid,
            true,
            mark_for(None, 0, false, 0),
            epoch
        ),
        "both guards dropped frees the session"
    );
}

#[test]
fn a_write_back_for_an_older_epoch_changes_nothing() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = crate::relay::Relays::default();
    let first = relays.mint();
    assert!(admit(
        &mut relays,
        sid,
        true,
        mark_for(None, 10, false, 0),
        first
    ));
    let second = relays.mint();
    assert!(admit(
        &mut relays,
        sid,
        true,
        mark_for(None, 20, true, 0),
        second
    ));
    store_back(&mut relays, sid, mark_for(None, 30, false, first));
    let stored = relays.rejoin.marks.get(sid).cloned().unwrap();
    assert_eq!((stored.read, stored.live, stored.epoch), (20, true, second));
    store_back(&mut relays, sid, mark_for(None, 30, false, second));
    let stored = relays.rejoin.marks.get(sid).cloned().unwrap();
    assert_eq!(
        (stored.read, stored.live, stored.epoch),
        (30, false, second)
    );
}

#[test]
fn a_write_back_for_a_newer_epoch_than_stored_changes_nothing() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = crate::relay::Relays::default();
    let first = relays.mint();
    assert!(admit(
        &mut relays,
        sid,
        true,
        mark_for(None, 10, false, 0),
        first
    ));
    store_back(&mut relays, sid, mark_for(None, 30, true, first + 1));
    let stored = relays.rejoin.marks.get(sid).cloned().unwrap();
    assert_eq!((stored.read, stored.live, stored.epoch), (10, false, first));
}

/// Relays holding `sid`'s kept summary level and a mark at the returned epoch.
fn kept_and_marked(sid: &str) -> (crate::relay::Relays, u64) {
    let mut relays = crate::relay::Relays::default();
    relays
        .subscribed
        .push((sid.to_owned(), subscribe_line("c_sub", "summary")));
    let epoch = relays.mint();
    assert!(admit(
        &mut relays,
        sid,
        false,
        mark_for(None, 0, false, 0),
        epoch
    ));
    (relays, epoch)
}

#[test]
fn a_candidate_at_the_stored_epoch_gets_the_kept_level() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let (relays, epoch) = kept_and_marked(sid);
    assert_eq!(
        kept_for_candidate(&relays, sid, epoch),
        Some(subscribe_line("c_sub", "summary"))
    );
}

#[test]
fn a_candidate_older_than_the_stored_epoch_is_stale() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let (relays, epoch) = kept_and_marked(sid);
    assert_eq!(kept_for_candidate(&relays, sid, epoch - 1), None);
}

#[test]
fn a_candidate_newer_than_the_stored_epoch_is_stale() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let (relays, epoch) = kept_and_marked(sid);
    assert_eq!(kept_for_candidate(&relays, sid, epoch + 1), None);
}

#[test]
fn a_candidate_with_no_stored_mark_is_stale() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = crate::relay::Relays::default();
    relays
        .subscribed
        .push((sid.to_owned(), subscribe_line("c_sub", "summary")));
    let epoch = relays.mint();
    assert_eq!(kept_for_candidate(&relays, sid, epoch), None);
}

/// What a rejoin at the candidate's own epoch writes to the session for a
/// connection that `hold` puts in a state the sweep must not open: the
/// session's end reads to EOF, since the stream is dropped either way.
fn rejoin_writes(
    hold: impl FnOnce(&mut crate::relay::Relays, &Arc<Mutex<crate::relay::Relays>>) -> Option<Opening>,
) -> String {
    let temp = Temp::new();
    let (hub, _clock) = temp.hub(FakeStarter::hang(&temp.dir));
    let sid = "s_aaaaaaaaaaaaaaaa";
    let (held, epoch) = kept_and_marked(sid);
    let relays = Arc::new(Mutex::new(held));
    let opening = {
        let mut held = lock(&relays);
        hold(&mut held, &relays)
    };
    let (client, _client_peer) = UnixStream::pair().unwrap();
    let writer = Arc::new(Mutex::new(client));
    let (stream, mut session) = UnixStream::pair().unwrap();
    session.set_read_timeout(Some(DEADLINE)).unwrap();
    crate::relay::attach_rejoin(sid, stream, &hub, &writer, &relays, epoch);
    drop(opening);
    let mut sent = String::new();
    std::io::Read::read_to_string(&mut session, &mut sent).unwrap();
    sent
}

#[test]
fn a_rejoin_for_a_closed_connection_writes_nothing_to_the_session() {
    let sent = rejoin_writes(|held, _| {
        held.rejoin.close();
        None
    });
    assert_eq!(sent, "", "no subscribe reaches the session");
}

#[test]
fn a_rejoin_for_a_relayed_session_writes_nothing_to_the_session() {
    let sent = rejoin_writes(|held, _| {
        held.entries.push(entry("s_aaaaaaaaaaaaaaaa", 0));
        None
    });
    assert_eq!(sent, "", "no subscribe reaches the session");
}

#[test]
fn a_rejoin_for_an_opening_session_writes_nothing_to_the_session() {
    let sent =
        rejoin_writes(|held, relays| Some(Opening::mark(held, relays, "s_aaaaaaaaaaaaaaaa")));
    assert_eq!(sent, "", "no subscribe reaches the session");
}

#[test]
fn candidates_lists_only_a_kept_unopened_marked_listed_unopening_open_session() {
    let sid = "s_aaaaaaaaaaaaaaaa";
    let names: BTreeSet<String> = BTreeSet::from([sid.to_owned()]);
    let kept = subscribe_line("c_sub", "summary");
    // The valid shape: a kept subscription, no relay, a mark, a name in
    // the listing, not opening, not closed.
    let mut relays = crate::relay::Relays::default();
    relays.subscribed.push((sid.to_owned(), kept.clone()));
    let epoch = relays.mint();
    assert!(admit(
        &mut relays,
        sid,
        false,
        mark_for(None, 0, false, 0),
        epoch
    ));
    assert_eq!(candidates(&relays, &names).len(), 1);
    // No kept subscription.
    let mut relays = crate::relay::Relays::default();
    let epoch = relays.mint();
    assert!(admit(
        &mut relays,
        sid,
        false,
        mark_for(None, 0, false, 0),
        epoch
    ));
    assert!(candidates(&relays, &names).is_empty());
    // A relay is already open.
    let mut relays = crate::relay::Relays::default();
    relays.subscribed.push((sid.to_owned(), kept.clone()));
    let epoch = relays.mint();
    relays.entries.push(entry(sid, epoch));
    let fresh = relays.mint();
    assert!(admit(
        &mut relays,
        sid,
        false,
        mark_for(None, 0, false, 0),
        fresh
    ));
    assert!(candidates(&relays, &names).is_empty());
    // No mark: a level `transfer` kept with no relay ever attached.
    let mut relays = crate::relay::Relays::default();
    relays.subscribed.push((sid.to_owned(), kept.clone()));
    assert!(candidates(&relays, &names).is_empty());
    // The name is not in the scan's listing.
    let mut relays = crate::relay::Relays::default();
    relays.subscribed.push((sid.to_owned(), kept.clone()));
    let epoch = relays.mint();
    assert!(admit(
        &mut relays,
        sid,
        false,
        mark_for(None, 0, false, 0),
        epoch
    ));
    assert!(candidates(&relays, &BTreeSet::new()).is_empty());
    // The session is opening.
    let relays_arc: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    {
        let mut held = lock(&relays_arc);
        held.subscribed.push((sid.to_owned(), kept.clone()));
        let epoch = held.mint();
        admit(&mut held, sid, false, mark_for(None, 0, false, 0), epoch);
    }
    let _opening = Opening::mark(&mut lock(&relays_arc), &relays_arc, sid);
    assert!(candidates(&lock(&relays_arc), &names).is_empty());
    // The connection is closed.
    let mut relays = crate::relay::Relays::default();
    relays.subscribed.push((sid.to_owned(), kept));
    let epoch = relays.mint();
    assert!(admit(
        &mut relays,
        sid,
        false,
        mark_for(None, 0, false, 0),
        epoch
    ));
    relays.rejoin.close();
    assert!(candidates(&relays, &names).is_empty());
}

#[test]
fn the_registry_forgets_an_unregistered_connection() {
    let registry = Connections::default();
    let relays_a: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    let relays_b: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    let (write_a, _) = UnixStream::pair().unwrap();
    let (write_b, _) = UnixStream::pair().unwrap();
    let writer_a = Arc::new(Mutex::new(write_a));
    let writer_b = Arc::new(Mutex::new(write_b));
    registry.register(1, &writer_a, &relays_a);
    registry.register(2, &writer_b, &relays_b);
    registry.unregister(1);
    assert_eq!(
        registry.ids(),
        vec![2],
        "the other connection is still swept"
    );
}

fn log_bytes(lines: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
    }
    bytes
}

fn log_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn kind_line(kind: &str) -> String {
    format!("{{\"kind\":\"{kind}\",\"ts\":1}}\n")
}

#[test]
fn a_fiber_started_past_the_offset_makes_it_live() {
    let held = fakes::TempDir::new("jl");
    let log = log_file(
        held.path(),
        "events.jsonl",
        &log_bytes(&[&kind_line("session_started"), &kind_line("fiber_started")]),
    );
    let len = fs::metadata(&log).unwrap().len();
    let mark = advance(mark_for(Some(log), 0, false, 3));
    assert!(mark.live);
    assert_eq!((mark.read, mark.epoch), (len, 3));
}

#[test]
fn a_later_fiber_exited_ends_it() {
    let held = fakes::TempDir::new("jl");
    let log = log_file(
        held.path(),
        "events.jsonl",
        &log_bytes(&[&kind_line("fiber_started"), &kind_line("fiber_exited")]),
    );
    let len = fs::metadata(&log).unwrap().len();
    let mark = advance(mark_for(Some(log), 0, true, 1));
    assert!(!mark.live);
    assert_eq!(mark.read, len);
}

#[test]
fn a_later_rewound_ends_it() {
    let held = fakes::TempDir::new("jl");
    let log = log_file(
        held.path(),
        "events.jsonl",
        &log_bytes(&[&kind_line("fiber_started"), &kind_line("rewound")]),
    );
    let mark = advance(mark_for(Some(log), 0, true, 1));
    assert!(!mark.live);
}

#[test]
fn other_kinds_change_nothing() {
    let held = fakes::TempDir::new("jl");
    let log = log_file(
        held.path(),
        "events.jsonl",
        &log_bytes(&[&kind_line("session_status"), "not json\n"]),
    );
    let len = fs::metadata(&log).unwrap().len();
    let live = advance(mark_for(Some(log.clone()), 0, true, 1));
    assert!(live.live, "other lines keep a live mark live");
    assert_eq!(
        live.read, len,
        "even an invalid line is consumed past its newline"
    );
    let dead = advance(mark_for(Some(log), 0, false, 1));
    assert!(!dead.live, "other lines keep a quiet mark quiet");
}

#[test]
fn a_last_line_without_its_newline_is_not_consumed() {
    let held = fakes::TempDir::new("jl");
    let full = kind_line("fiber_started");
    let mut bytes = full.as_bytes().to_vec();
    bytes.extend_from_slice(b"{\"kind\":\"fiber_exited\"");
    let log = log_file(held.path(), "events.jsonl", &bytes);
    let mark = advance(mark_for(Some(log.clone()), 0, false, 1));
    assert!(mark.live, "the completed start counts");
    assert_eq!(
        mark.read,
        full.len() as u64,
        "the offset stays before the partial tail"
    );
    fs::write(&log, [bytes, b"}\n".to_vec()].concat()).unwrap();
    let again = advance(mark);
    assert!(!again.live, "once completed, the exit counts");
    assert_eq!(
        again.read,
        fs::metadata(&log).unwrap().len(),
        "the completed tail is consumed"
    );
}

#[test]
fn bytes_before_the_offset_are_never_read() {
    let held = fakes::TempDir::new("jl");
    let first = kind_line("fiber_started");
    let log = log_file(
        held.path(),
        "events.jsonl",
        &log_bytes(&[&first, &kind_line("session_status")]),
    );
    let mark = advance(mark_for(Some(log), first.len() as u64, false, 1));
    assert!(!mark.live, "a start before the offset leaves it quiet");
}

#[test]
fn a_missing_log_is_not_live_and_keeps_its_offset() {
    let held = fakes::TempDir::new("jl");
    let missing = held.path().join("gone.jsonl");
    let mark = advance(mark_for(Some(missing), 12, false, 1));
    assert!(!mark.live);
    assert_eq!((mark.read, mark.epoch), (12, 1));
    let mark = advance(mark_for(None, 12, false, 1));
    assert!(!mark.live);
    assert_eq!((mark.read, mark.epoch), (12, 1));
}

#[test]
fn a_log_shorter_than_the_offset_was_replaced() {
    let held = fakes::TempDir::new("jl");
    let log = log_file(
        held.path(),
        "events.jsonl",
        &log_bytes(&[&kind_line("fiber_started")]),
    );
    let mark = advance(mark_for(Some(log.clone()), 10_000, false, 1));
    assert!(mark.live, "the new file is read from its start");
    assert_eq!(mark.read, fs::metadata(&log).unwrap().len());
}

#[test]
fn a_log_exactly_as_long_as_the_offset_was_not_replaced() {
    let held = fakes::TempDir::new("jl");
    let log = log_file(
        held.path(),
        "events.jsonl",
        &log_bytes(&[&kind_line("fiber_exited")]),
    );
    let len = fs::metadata(&log).unwrap().len();
    let mark = advance(mark_for(Some(log), len, true, 1));
    assert!(mark.live, "nothing past the offset: the mark stays live");
    assert_eq!(mark.read, len, "and its offset stays put");
}

#[test]
fn a_mark_sits_on_the_last_complete_line() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let sid = id(1);
    temp.write_log(&sid, &workspace, None, &[]);
    temp.append_raw(&sid, "{\"kind\":\"session_status\",\"ts\":1");
    let mark = Mark::now(&temp.dir, &sid);
    let log = fs::read(temp.log_dir(&sid).join("events.jsonl")).unwrap();
    let complete = log.iter().rposition(|byte| *byte == b'\n').unwrap() + 1;
    assert_eq!(mark.read, complete as u64, "never the raw length");
    // A resumed writer truncates the partial line before appending, so the
    // resumed start lands where the mark sits.
    let mut truncated = log[..complete].to_vec();
    let started = serde_json::to_string(&fiber_started(&sid)).unwrap();
    truncated.extend_from_slice(format!("{started}\n").as_bytes());
    fs::write(temp.log_dir(&sid).join("events.jsonl"), truncated).unwrap();
    let mark = advance(mark);
    assert!(mark.live, "the resumed start is past the mark");
}

#[test]
fn a_partial_tail_is_read_again_until_it_completes() {
    let held = fakes::TempDir::new("jl");
    let first = kind_line("session_status");
    let mut bytes = first.as_bytes().to_vec();
    bytes.extend_from_slice(b"{\"kind\":\"fiber_started\"");
    let log = log_file(held.path(), "events.jsonl", &bytes);
    let len = bytes.len() as u64;
    let mark = advance(mark_for(Some(log.clone()), 0, false, 1));
    assert_eq!(mark.read, first.len() as u64);
    assert!(!mark.live);
    // No new bytes: the same mark comes back, so consumed bytes are never
    // read again.
    let same = advance(mark.clone());
    assert_eq!((same.read, same.live), (mark.read, mark.live));
    fs::write(&log, [bytes, b"}\n".to_vec()].concat()).unwrap();
    assert!(len < fs::metadata(&log).unwrap().len());
    let done = advance(mark);
    assert!(done.live);
    assert_eq!(done.read, fs::metadata(&log).unwrap().len());
}

#[test]
fn a_summary_subscription_through_the_hub_follows_a_session_into_its_later_run() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(1);
    temp.write_log(&sid, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    let ack = client.next("the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted");
    assert_eq!(ack["payload"]["command_id"], "c_sub");
    // The session exits: its log ends, its socket is unlinked, and its
    // relay thread drops the entry, which the barrier below proves.
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    // The later run binds and streams.
    temp.append(&sid, &fiber_started(&sid));
    let resumed = Fake::bind(&temp.dir, &sid);
    resumed.say(&status_line(&sid, "streaming", None));
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the rejoin");
    let (_, lines) = resumed.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "the resumed session's subscribe",
    );
    let first: Value =
        serde_json::from_str(lines.iter().find(|line| is_rejoin(line)).unwrap()).unwrap();
    assert_eq!(first["command"], "subscribe");
    assert_eq!(first["args"], json!({"level": "summary"}));
    assert_ne!(first["id"], "c_sub");
    // The client sent nothing after its subscribe: its next status is the
    // later run's, with no acknowledgement leaking.
    let status = client.next_status("the later run's status");
    assert_eq!(status["session_id"], sid);
    assert_eq!(status["payload"]["state"], "streaming");
}

#[test]
fn the_rejoin_replays_the_last_held_level() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(2);
    temp.write_log(&sid, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    for (cmd, level) in [("c_sum", "summary"), ("c_full", "full")] {
        client.send(&json!({
            "id": cmd, "session_id": sid, "command": "subscribe", "args": {"level": level},
        }));
        assert_eq!(
            client.next("the subscribe acknowledgement")["kind"],
            "command_accepted"
        );
    }
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    temp.append(&sid, &fiber_started(&sid));
    let resumed = Fake::bind(&temp.dir, &sid);
    resumed.say(&status_line(&sid, "streaming", None));
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the rejoin");
    let (_, lines) = resumed.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "the resumed session's subscribe",
    );
    let first: Value =
        serde_json::from_str(lines.iter().find(|line| is_rejoin(line)).unwrap()).unwrap();
    assert_eq!(
        first["args"],
        json!({"level": "full"}),
        "the last held level: {first}"
    );
}

#[test]
fn a_socket_that_accepts_without_a_new_start_gets_no_rejoin() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(3);
    temp.write_log(&sid, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    // The log still ends in the run's exit: no new `fiber_started`.
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    let rebound = Fake::bind(&temp.dir, &sid);
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the quiet scan");
    assert!(!rebound.has_rejoin(), "no new start, no rejoin");
}

#[test]
fn a_running_session_without_a_new_start_gets_no_rejoin() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(4);
    // A start before the mark: with a mark at the raw length it would
    // rejoin here, so the mark must sit on the last complete line.
    temp.write_log(&sid, &workspace, None, &[fiber_started(&sid)]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    // The session closes the relay connection, keeps running and stays
    // bound; the log grows with non-start lines only.
    fake.shutdown_write();
    fake.await_closed();
    temp.append(
        &sid,
        &json!({"kind": "session_status", "session_id": sid, "ts": 9, "schema_version": 1,
            "payload": payload("idle", None)}),
    );
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the quiet scan");
    assert!(
        !fake.has_rejoin(),
        "the running session is not connected again"
    );
}

#[test]
fn a_run_shorter_than_one_rescan_is_not_rejoined() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(5);
    temp.write_log(&sid, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    // A later run starts and ends between two scans: no socket, so the
    // sweep connects to nothing.
    temp.append(&sid, &fiber_started(&sid));
    temp.append(&sid, &fiber_exited(&sid));
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    assert!(client.try_next().is_none(), "the short run sent nothing");
    // A still later run binds: it rejoins.
    temp.append(&sid, &fiber_started(&sid));
    let resumed = Fake::bind(&temp.dir, &sid);
    resumed.say(&status_line(&sid, "streaming", None));
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the rejoin");
    resumed.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "the resumed session's subscribe",
    );
    let status = client.next_status("the later run's status");
    assert_eq!(status["payload"]["state"], "streaming");
}

#[test]
fn a_connection_without_a_kept_level_gets_no_rejoin() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(6);
    temp.write_log(&sid, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    // A connection that never subscribed.
    let mut quiet = Client::connect(&hub);
    assert_eq!(quiet.next("hub_hello")["kind"], "hub_hello");
    // A connection whose subscribe the session rejected keeps nothing.
    let other = id(7);
    temp.write_log(&other, &workspace, None, &[]);
    let rejecting = Fake::bind_rejecting(&temp.dir, &other, true);
    let mut refused = Client::connect(&hub);
    assert_eq!(refused.next("hub_hello")["kind"], "hub_hello");
    refused.send(&json!({
        "id": "c_sub", "session_id": other, "command": "subscribe", "args": {"level": "summary"},
    }));
    let answer = refused.next("the rejected subscribe");
    assert_eq!(answer["kind"], "command_rejected");
    temp.append(&other, &fiber_exited(&other));
    rejecting.stop_listening();
    rejecting.unlink();
    rejecting.shutdown_write();
    rejecting.await_closed();
    temp.append(&other, &fiber_started(&other));
    let rebound = Fake::bind(&temp.dir, &other);
    // The subscribed session resumes too.
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    temp.append(&sid, &fiber_started(&sid));
    let resumed = Fake::bind(&temp.dir, &sid);
    resumed.say(&status_line(&sid, "streaming", None));
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the rejoin");
    resumed.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "the resumed session's subscribe",
    );
    assert_eq!(
        client.next_status("the later run's status")["session_id"],
        sid
    );
    assert!(
        !rebound.has_rejoin(),
        "the rejected subscription rejoins nothing"
    );
    assert!(
        quiet
            .drain()
            .iter()
            .all(|line| line.get("kind").and_then(Value::as_str) != Some("session_status")),
        "the never-subscribed connection hears no status"
    );
}

#[test]
fn a_session_with_a_live_relay_is_not_connected_again() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(8);
    temp.write_log(&sid, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    temp.append(&sid, &fiber_started(&sid));
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    assert!(!fake.has_rejoin(), "the live relay is not duplicated");
}

#[test]
fn a_disconnected_connection_gets_no_rejoin() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(9);
    temp.write_log(&sid, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    drop(client);
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    temp.append(&sid, &fiber_started(&sid));
    let resumed = Fake::bind(&temp.dir, &sid);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    assert!(!resumed.has_rejoin(), "a gone connection rejoins nothing");
    await_unregistered(&hub, &clock, "the gone connection");
}

#[test]
fn a_delegates_later_run_is_rejoined_without_the_starter() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::hang(&temp.dir);
    let (hub, clock) = temp.hub(starter.clone());
    wire(&hub);
    start(&hub.feed, &clock);
    let parent = id(10);
    let sid = id(11);
    temp.write_log(&sid, &workspace, Some(&parent), &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    temp.append(&sid, &fiber_started(&sid));
    let resumed = Fake::bind(&temp.dir, &sid);
    resumed.say(&status_line(&sid, "streaming", Some(&parent)));
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the rejoin");
    resumed.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "the resumed session's subscribe",
    );
    let status = client.next_status("the later run's status");
    assert_eq!(status["payload"]["parent"], parent);
    assert!(
        starter.resumed().is_empty(),
        "the hub never calls the starter: it only connects to the bound socket"
    );
}

#[test]
fn a_rewound_last_line_never_rejoins_the_old_session() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(12);
    let next = id(13);
    temp.write_log(&sid, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    temp.append(&sid, &fiber_started(&sid));
    temp.append(&sid, &rewound(&sid, &next));
    let rebound = Fake::bind(&temp.dir, &sid);
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the quiet scan");
    assert!(!rebound.has_rejoin(), "a rewound log rejoins nothing");
}

#[test]
fn a_stale_socket_is_skipped_until_a_session_binds() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(14);
    temp.write_log(&sid, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    // A stale socket file that refuses connections, with a live log.
    temp.append(&sid, &fiber_started(&sid));
    fs::write(temp.dir.join("run").join(&sid), b"stale").unwrap();
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the quiet scan");
    // A session binds there: the next scan rejoins.
    fs::remove_file(temp.dir.join("run").join(&sid)).unwrap();
    let resumed = Fake::bind(&temp.dir, &sid);
    resumed.say(&status_line(&sid, "streaming", None));
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the rejoin");
    resumed.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "the resumed session's subscribe",
    );
    assert_eq!(
        client.next_status("the later run's status")["payload"]["state"],
        "streaming"
    );
}

/// A starter whose `rewind` of one session waits for a release.
struct GateStarter {
    inner: FakeStarter,
    held: contract::SessionId,
    entered: mpsc::Sender<()>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl crate::Starter for GateStarter {
    fn start(
        &self,
        id: &contract::SessionId,
        workspace: &Path,
        model: Option<&str>,
        overrides: &[&str],
        worktree: bool,
    ) -> std::io::Result<Box<dyn crate::Started>> {
        self.inner.start(id, workspace, model, overrides, worktree)
    }

    fn resume(
        &self,
        id: &contract::SessionId,
        workspace: &Path,
    ) -> std::io::Result<Box<dyn crate::Started>> {
        self.inner.resume(id, workspace)
    }

    fn rewind(
        &self,
        id: &contract::SessionId,
        workspace: &Path,
        from: &contract::SessionId,
    ) -> std::io::Result<Box<dyn crate::Started>> {
        if *id == self.held {
            self.entered.send(()).unwrap_or(());
            if let Some(release) = lock(&self.release).take() {
                release.recv_timeout(DEADLINE).unwrap_or(());
            }
        }
        self.inner.rewind(id, workspace, from)
    }
}

#[test]
fn route_and_the_sweep_open_only_one_relay() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    // Forward: the client's command pauses after taking the opening while
    // the sweep runs.
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(20);
    let peer = id(22);
    temp.write_log(&sid, &workspace, None, &[]);
    temp.write_log(&peer, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let fake_peer = Fake::bind(&temp.dir, &peer);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    for (cmd, session) in [("c_sub", sid.clone()), ("c_peer", peer.clone())] {
        client.send(&json!({
            "id": cmd, "session_id": session, "command": "subscribe", "args": {"level": "summary"},
        }));
        assert_eq!(
            client.next("the subscribe acknowledgement")["kind"],
            "command_accepted"
        );
    }
    for (session, running) in [(&sid, &fake), (&peer, &fake_peer)] {
        temp.append(session, &fiber_exited(session));
        running.stop_listening();
        running.unlink();
        running.shutdown_write();
        running.await_closed();
        temp.append(session, &fiber_started(session));
    }
    let resumed = Fake::bind(&temp.dir, &sid);
    let resumed_peer = Fake::bind(&temp.dir, &peer);
    resumed_peer.say(&status_line(&peer, "streaming", None));
    let (paused_tx, paused) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    *lock(&hub.before_open) = Some(Box::new(move || {
        paused_tx.send(()).unwrap_or(());
        release.recv_timeout(DEADLINE).unwrap_or(());
    }));
    client.send(&json!({
        "id": "c_cmd", "session_id": sid, "command": "prompt",
        "args": {"content": [{"type": "text", "text": "hi"}]},
    }));
    assert!(
        paused.recv_timeout(DEADLINE).is_ok(),
        "route paused after its opening"
    );
    // The sweep runs while route waits: the peer rejoins, and the paused
    // session gets no new connection.
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the sweep's pass");
    resumed_peer.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "the peer's rejoin",
    );
    assert!(
        !resumed.has_rejoin(),
        "the sweep never opens what route is opening"
    );
    drop(release_tx);
    let (_, lines) = resumed.await_conn_where(
        |lines, _| {
            lines.iter().any(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|line| line.get("id").cloned())
                    == Some(Value::String("c_cmd".to_owned()))
            })
        },
        "route's relay",
    );
    assert_eq!(lines.len(), 2, "the replay then the command: {lines:?}");
    let replay: Value = serde_json::from_str(&lines[0]).unwrap();
    let command: Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(replay["command"], "subscribe");
    assert!(
        is_rejoin(&lines[0]),
        "the replay carries an id of the hub's own"
    );
    assert_eq!(command["id"], "c_cmd");
    client.next_ack("c_cmd", "the prompt acknowledgement");

    // Reverse: the worker pauses before its connect while the client's
    // command opens through route.
    let other = id(21);
    temp.write_log(&other, &workspace, None, &[]);
    let old = Fake::bind(&temp.dir, &other);
    client.send(&json!({
        "id": "c_sub2", "session_id": other, "command": "subscribe", "args": {"level": "summary"},
    }));
    // The peer's relay forwards its statuses meanwhile, in no order
    // against this acknowledgement.
    client.next_ack("c_sub2", "the subscribe acknowledgement");
    temp.append(&other, &fiber_exited(&other));
    old.stop_listening();
    old.unlink();
    old.shutdown_write();
    old.await_closed();
    temp.append(&other, &fiber_started(&other));
    let rebound = Fake::bind(&temp.dir, &other);
    let (paused_tx, paused) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    *lock(&hub.rejoins.before_connect) = Some(Box::new(move || {
        paused_tx.send(()).unwrap_or(());
        release.recv_timeout(DEADLINE).unwrap_or(());
    }));
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    assert!(
        paused.recv_timeout(DEADLINE).is_ok(),
        "the worker paused before its connect"
    );
    client.send(&json!({
        "id": "c_cmd2", "session_id": other, "command": "prompt",
        "args": {"content": [{"type": "text", "text": "hi"}]},
    }));
    client.next_ack("c_cmd2", "the prompt acknowledgement");
    drop(release_tx);
    await_pass(&done, "the worker's pass");
    rebound.await_conn_where(
        |lines, _| {
            lines.iter().any(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|line| line.get("id").cloned())
                    == Some(Value::String("c_cmd2".to_owned()))
            })
        },
        "route's relay",
    );
    // The worker's candidate went stale while it waited: it never connects.
    assert_eq!(
        rebound.rejoin_count(),
        1,
        "only route's relay replayed the level: {:?}",
        rebound.all_lines()
    );
}

#[test]
fn a_level_changed_while_the_worker_waits_is_the_one_rejoined() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(40);
    temp.write_log(&sid, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sum", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    client.next_ack("c_sum", "the summary acknowledgement");
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    temp.append(&sid, &fiber_started(&sid));
    let resumed = Fake::bind(&temp.dir, &sid);
    let (paused_tx, paused) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    *lock(&hub.rejoins.before_connect) = Some(Box::new(move || {
        paused_tx.send(()).unwrap_or(());
        release.recv_timeout(DEADLINE).unwrap_or(());
    }));
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    assert!(
        paused.recv_timeout(DEADLINE).is_ok(),
        "the worker paused before its connect"
    );
    // While the worker waits with the summary candidate, the client moves
    // to full through a relay of its own, and that relay closes.
    client.send(&json!({
        "id": "c_full", "session_id": sid, "command": "subscribe", "args": {"level": "full"},
    }));
    client.next_ack("c_full", "the full acknowledgement");
    // The feed's own summary connection, opened at the scan, is not this
    // relay and may outlive it: only the client's relay is waited out.
    resumed.shutdown_write();
    resumed.await_conn_where(
        |lines, eof| eof && lines.iter().any(|line| line.contains("\"c_full\"")),
        "the end of the client's own relay",
    );
    drop(release_tx);
    await_pass(&done, "the worker's pass");
    assert_eq!(
        resumed.rejoin_count(),
        1,
        "the stale candidate replays nothing: {:?}",
        resumed.all_lines()
    );
    // The next run is rejoined at the level the connection now holds.
    temp.append(&sid, &fiber_started(&sid));
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the next pass");
    let (_, lines) = resumed.await_conn_where(
        |lines, eof| !eof && lines.iter().any(|line| is_rejoin(line)),
        "the rejoin at the new level",
    );
    let first: Value =
        serde_json::from_str(lines.iter().find(|line| is_rejoin(line)).unwrap()).unwrap();
    assert_eq!(
        first["args"],
        json!({"level": "full"}),
        "the level held now: {first}"
    );
}

#[test]
fn follow_and_the_sweep_open_only_one_relay() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let old = id(30);
    let next = id(31);
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let starter = GateStarter {
        inner: FakeStarter::hang(&temp.dir),
        held: contract::SessionId(next.clone()),
        entered: entered_tx,
        release: Mutex::new(Some(release)),
    };
    let (hub, clock) = temp.hub(starter);
    wire(&hub);
    start(&hub.feed, &clock);
    // The connection holds a level on the old session, and an earlier
    // relay to the next one left a mark but no kept level. A peer session
    // resumes alongside, to drive the worker's pass.
    let peer = id(32);
    temp.write_log(&old, &workspace, None, &[]);
    temp.write_log(&peer, &workspace, None, &[]);
    let old_fake = Fake::bind(&temp.dir, &old);
    let peer_fake = Fake::bind(&temp.dir, &peer);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    for (cmd, session) in [("c_old", old.clone()), ("c_peer", peer.clone())] {
        client.send(&json!({
            "id": cmd, "session_id": session, "command": "subscribe", "args": {"level": "summary"},
        }));
        assert_eq!(
            client.next("the subscribe acknowledgement")["kind"],
            "command_accepted"
        );
    }
    temp.write_log(&next, &workspace, None, &[]);
    fs::remove_file(temp.log_dir(&next).join("events.jsonl")).unwrap();
    let next_fake = Fake::bind_rejecting(&temp.dir, &next, true);
    client.send(&json!({
        "id": "c_next", "session_id": next, "command": "subscribe", "args": {"level": "summary"},
    }));
    let answer = client.next("the rejected subscribe");
    assert_eq!(answer["kind"], "command_rejected");
    next_fake.stop_listening();
    next_fake.unlink();
    next_fake.shutdown_write();
    next_fake.await_closed();
    // The peer session resumes: it will drive the worker's pass.
    temp.append(&peer, &fiber_exited(&peer));
    peer_fake.stop_listening();
    peer_fake.unlink();
    peer_fake.shutdown_write();
    peer_fake.await_closed();
    temp.append(&peer, &fiber_started(&peer));
    let resumed_peer = Fake::bind(&temp.dir, &peer);
    resumed_peer.say(&status_line(&peer, "streaming", None));
    // The old session rewinds to the next one: `follow` transfers the
    // level, then waits in its gated start holding the next session
    // opening.
    temp.append(&old, &rewound(&old, &next));
    old_fake.stop_listening();
    old_fake.unlink();
    old_fake.shutdown_write();
    assert!(
        entered.recv_timeout(DEADLINE).is_ok(),
        "follow reached its gated start"
    );
    // The next session binds meanwhile: the sweep skips it while the
    // opening holds, and rejoins the peer on the same pass.
    temp.write_log(&next, &workspace, None, &[fiber_started(&next)]);
    let rebound = Fake::bind(&temp.dir, &next);
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the sweep's pass");
    resumed_peer.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "the peer's rejoin",
    );
    assert!(
        !rebound.has_rejoin(),
        "the sweep never opens what follow is opening"
    );
    drop(release_tx);
    let (_, lines) = rebound.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "follow's relay",
    );
    assert_eq!(lines.len(), 1, "one relay, from follow: {lines:?}");
    let replay: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(
        replay["args"],
        json!({"level": "summary"}),
        "the transferred level: {replay}"
    );
}

#[test]
fn a_paused_worker_blocks_no_scan_and_starts_no_second_worker() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(40);
    temp.write_log(&sid, &workspace, None, &[]);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    temp.append(&sid, &fiber_started(&sid));
    let resumed = Fake::bind(&temp.dir, &sid);
    resumed.say(&status_line(&sid, "streaming", None));
    let (paused_tx, paused) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    *lock(&hub.rejoins.before_connect) = Some(Box::new(move || {
        paused_tx.send(()).unwrap_or(());
        release.recv_timeout(DEADLINE).unwrap_or(());
    }));
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    assert!(
        paused.recv_timeout(DEADLINE).is_ok(),
        "the worker paused before its connect"
    );
    // Further scans complete while it waits, and start no second worker:
    // the pause is one-shot, so a second worker would connect at once.
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    assert!(!resumed.has_rejoin(), "no second worker connected");
    let stopping = Arc::clone(&hub.feed);
    let (stopped_tx, stopped) = mpsc::channel();
    thread::spawn(move || {
        stopping.stop();
        stopped_tx.send(()).unwrap_or(());
    });
    assert!(
        stopped.recv_timeout(DEADLINE).is_ok(),
        "stop never waits for the worker"
    );
    drop(release_tx);
    resumed.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "the resumed session's subscribe",
    );
    let status = client.next_status("the later run's status");
    assert_eq!(status["payload"]["state"], "streaming");
}

#[test]
fn a_session_with_no_log_at_attach_rejoins_from_its_discovered_start() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(50);
    // No log when the relay opens: the mark records no log at offset 0.
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    fake.shutdown_write();
    fake.await_closed();
    fake.stop_listening();
    fake.unlink();
    // The later run writes the log the attach never saw, then binds and
    // streams. Discovery starts it at offset 0, so its start counts.
    temp.write_log(&sid, &workspace, None, &[fiber_started(&sid)]);
    let resumed = Fake::bind(&temp.dir, &sid);
    resumed.say(&status_line(&sid, "streaming", None));
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the rejoin");
    resumed.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "the resumed session's subscribe",
    );
    let status = client.next_status("the later run's status");
    assert_eq!(status["payload"]["state"], "streaming");
}

#[test]
fn a_long_consumed_prefix_still_rejoins_from_the_offset() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let (hub, clock) = temp.hub(FakeStarter::hang(&temp.dir));
    wire(&hub);
    start(&hub.feed, &clock);
    let sid = id(51);
    // Hundreds of consumed lines before the mark: the sweep reads only
    // past the offset, so the later run's start still counts.
    let prefix: Vec<Value> = (0..500)
        .map(|n| {
            json!({"kind": "session_status", "session_id": sid, "ts": n, "schema_version": 1,
                "payload": payload("idle", None)})
        })
        .collect();
    temp.write_log(&sid, &workspace, None, &prefix);
    let fake = Fake::bind(&temp.dir, &sid);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub", "session_id": sid, "command": "subscribe", "args": {"level": "summary"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    temp.append(&sid, &fiber_exited(&sid));
    fake.stop_listening();
    fake.unlink();
    fake.shutdown_write();
    fake.await_closed();
    temp.append(&sid, &fiber_started(&sid));
    let resumed = Fake::bind(&temp.dir, &sid);
    resumed.say(&status_line(&sid, "streaming", None));
    let done = arm_pass(&hub);
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    await_pass(&done, "the rejoin");
    resumed.await_conn_where(
        |lines, _| lines.iter().any(|line| is_rejoin(line)),
        "the resumed session's subscribe",
    );
    let status = client.next_status("the later run's status");
    assert_eq!(status["payload"]["state"], "streaming");
}
