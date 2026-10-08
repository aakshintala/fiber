//! Tests for starting the session a `rewound` line names: reading the
//! pointer, starting it from the relay before the acknowledgement, the
//! redirect after the old socket closes, the per-session start lock, and a
//! failed start's diagnostic.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
use crate::Started;
use crate::Starter;
use crate::connection::serve_connection;
use crate::diag::Diag;
use crate::fake::{FakeStarter, Handshake};

/// One named deadline per wait: every start and acknowledgement lands
/// before it.
const DEADLINE: Duration = Duration::from_secs(10);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hr");
        let dir = held.path().join("h");
        fs::create_dir_all(&dir).unwrap();
        Self { dir, held }
    }

    fn hub(&self, starter: impl Starter + 'static) -> Arc<Hub> {
        let timed: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
        Arc::new(Hub::new(
            &self.dir,
            "0.0.0",
            Arc::new(starter),
            Arc::clone(&timed),
            Diag::open(&self.dir, timed),
        ))
    }

    fn workspace(&self) -> String {
        let workspace = self.dir.join("w");
        fs::create_dir_all(&workspace).unwrap();
        workspace.to_string_lossy().into_owned()
    }

    /// Writes `id`'s log: a `session_started` recording `workspace`,
    /// then `last` as the final line.
    fn write_log(&self, id: &str, workspace: &str, last: &Value) {
        let dir = self
            .dir
            .join("projects")
            .join("-w")
            .join("sessions")
            .join(id);
        fs::create_dir_all(&dir).unwrap();
        let first = json!({
            "kind": "session_started", "session_id": id, "ts": 1, "schema_version": 1,
            "payload": {"workspace": workspace},
        });
        fs::write(
            dir.join("events.jsonl"),
            format!(
                "{first}\n{last}\n",
                first = serde_json::to_string(&first).unwrap(),
                last = serde_json::to_string(last).unwrap(),
            ),
        )
        .unwrap();
    }

    fn hub_log(&self) -> String {
        fs::read_to_string(self.dir.join("logs").join("hub.log")).unwrap_or_default()
    }
}

fn id(n: u64) -> String {
    format!("s_{n:016x}")
}

fn rewound_line(old: &str, next: &str) -> Value {
    json!({
        "kind": "rewound", "session_id": old, "ts": 2, "schema_version": 1, "seq": 5,
        "payload": {"new_session_id": next, "seq": 3, "jobs": []},
    })
}

fn exited_line(old: &str) -> Value {
    json!({
        "kind": "fiber_exited", "session_id": old, "ts": 2, "schema_version": 1, "seq": 5,
        "payload": {},
    })
}

#[test]
fn a_next_session_has_one_start_lock_and_each_session_its_own() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let first = SessionId(id(2));
    let other = SessionId(id(3));
    let lock = guard_for(&hub, &first);
    assert!(Arc::ptr_eq(&lock, &guard_for(&hub, &first)));
    assert!(!Arc::ptr_eq(&lock, &guard_for(&hub, &other)));
}

#[test]
fn continued_names_the_rewound_session() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let old = id(1);
    let next = id(2);
    temp.write_log(&old, &workspace, &rewound_line(&old, &next));
    assert_eq!(continued(&temp.dir, &SessionId(old)), Some(SessionId(next)));
}

#[test]
fn continued_ignores_anything_but_a_well_formed_rewound() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    // An exit starts nothing.
    let exited = id(1);
    temp.write_log(&exited, &workspace, &exited_line(&exited));
    assert_eq!(continued(&temp.dir, &SessionId(exited)), None);
    // A missing log starts nothing.
    assert_eq!(continued(&temp.dir, &SessionId(id(2))), None);
    // A `rewound` naming no minted session starts nothing.
    let odd = id(3);
    temp.write_log(
        &odd,
        &workspace,
        &json!({
            "kind": "rewound", "session_id": odd.clone(), "ts": 2, "schema_version": 1, "seq": 5,
            "payload": {"new_session_id": "nope", "seq": 3, "jobs": []},
        }),
    );
    assert_eq!(continued(&temp.dir, &SessionId(odd)), None);
}

#[test]
fn start_connects_when_the_next_session_runs() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let next = id(2);
    let bound = crate::fake::FakeSession::bind(&temp.dir, &next);
    assert!(reach(&hub, &SessionId(id(1)), &SessionId(next.clone())).is_some());
    assert!(
        starter.rewound().is_empty(),
        "a running session is attached to"
    );
    assert!(
        starter.resumed().is_empty(),
        "no resume for a running session"
    );
    assert!(
        UnixStream::connect(temp.dir.join("run").join(&next)).is_ok(),
        "the socket accepts"
    );
    bound.close();
}

#[test]
fn start_resumes_a_next_session_with_a_log() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let from = id(1);
    let next = id(2);
    temp.write_log(&from, &workspace, &exited_line(&from));
    temp.write_log(&next, &workspace, &exited_line(&next));
    assert!(reach(&hub, &SessionId(from), &SessionId(next.clone())).is_some());
    assert!(starter.rewound().is_empty(), "a logged session is resumed");
    assert_eq!(
        starter.resumed(),
        vec![(SessionId(next), PathBuf::from(workspace))],
        "resumed in the workspace its own log recorded"
    );
}

#[test]
fn start_starts_a_next_session_without_a_log() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let from = id(1);
    let next = id(2);
    temp.write_log(&from, &workspace, &exited_line(&from));
    assert!(reach(&hub, &SessionId(from.clone()), &SessionId(next.clone())).is_some());
    assert_eq!(
        starter.rewound(),
        vec![(
            SessionId(next.clone()),
            PathBuf::from(workspace),
            SessionId(from)
        )],
        "started in the old session's workspace, for its rewind"
    );
    assert!(
        UnixStream::connect(temp.dir.join("run").join(&next)).is_ok(),
        "the new session's socket accepts"
    );
}

#[test]
fn two_starts_of_one_session_start_once() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let from = id(1);
    let next = id(2);
    temp.write_log(&from, &workspace, &exited_line(&from));
    let other = thread::Builder::new()
        .name("second-start".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let (from, next) = (from.clone(), next.clone());
            move || reach(&hub, &SessionId(from), &SessionId(next))
        })
        .unwrap();
    assert!(reach(&hub, &SessionId(from), &SessionId(next)).is_some());
    assert!(other.join().unwrap().is_some());
    assert_eq!(starter.rewound().len(), 1, "one starter call");
}

/// A starter whose `rewind` of one session waits for a release: every
/// other session still starts.
struct GateStarter {
    inner: FakeStarter,
    held: SessionId,
    entered: mpsc::Sender<()>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Starter for GateStarter {
    fn start(
        &self,
        id: &SessionId,
        workspace: &std::path::Path,
        model: Option<&str>,
    ) -> std::io::Result<Box<dyn crate::Started>> {
        self.inner.start(id, workspace, model)
    }

    fn resume(
        &self,
        id: &SessionId,
        workspace: &std::path::Path,
    ) -> std::io::Result<Box<dyn Started>> {
        self.inner.resume(id, workspace)
    }

    fn rewind(
        &self,
        id: &SessionId,
        workspace: &std::path::Path,
        from: &SessionId,
    ) -> std::io::Result<Box<dyn Started>> {
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
fn a_slow_start_of_one_session_does_not_block_another() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let from = id(1);
    let held = id(2);
    let other = id(3);
    temp.write_log(&from, &workspace, &exited_line(&from));
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let starter = GateStarter {
        inner: FakeStarter::bind_and_hold(&temp.dir),
        held: SessionId(held.clone()),
        entered: entered_tx,
        release: Mutex::new(Some(release)),
    };
    let hub = temp.hub(starter);
    let slow = thread::Builder::new()
        .name("slow-rewind".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let from = from.clone();
            move || drop(reach(&hub, &SessionId(from), &SessionId(held)))
        })
        .unwrap();
    // The slow start is inside `rewind`, holding only its own session's
    // lock: no sleep, the channel says so.
    assert!(
        entered.recv_timeout(DEADLINE).is_ok(),
        "the slow start reached the starter"
    );
    // On a thread with a named deadline: a shared-lock regression would
    // block it past the deadline instead of hanging the test.
    let (done_tx, done) = mpsc::channel();
    thread::Builder::new()
        .name("rival-start".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let (from, other) = (from.clone(), other.clone());
            move || {
                done_tx
                    .send(reach(&hub, &SessionId(from), &SessionId(other)))
                    .unwrap_or(());
            }
        })
        .unwrap();
    let stream = done
        .recv_timeout(DEADLINE)
        .expect("the other session starts while the slow one waits");
    assert!(stream.is_some());
    assert!(
        UnixStream::connect(temp.dir.join("run").join(&other)).is_ok(),
        "the other session's socket accepts"
    );
    drop(release_tx);
    slow.join().unwrap();
}

/// A starter that fails every `rewind` with `message` as its io error text.
struct FailRewind {
    message: String,
}

impl Starter for FailRewind {
    fn start(
        &self,
        _id: &SessionId,
        _workspace: &std::path::Path,
        _model: Option<&str>,
    ) -> std::io::Result<Box<dyn crate::Started>> {
        Err(std::io::Error::other("unused"))
    }

    fn resume(
        &self,
        _id: &SessionId,
        _workspace: &std::path::Path,
    ) -> std::io::Result<Box<dyn Started>> {
        Err(std::io::Error::other("unused"))
    }

    fn rewind(
        &self,
        _id: &SessionId,
        _workspace: &std::path::Path,
        _from: &SessionId,
    ) -> std::io::Result<Box<dyn Started>> {
        Err(std::io::Error::other(self.message.clone()))
    }
}

#[test]
fn a_failed_start_writes_a_diagnostic_without_the_detail() {
    const SECRET: &str = "the-volume-of-the-meeting-room";
    let temp = Temp::new();
    let workspace = temp.workspace();
    let hub = temp.hub(FailRewind {
        message: format!("starter blew up on {SECRET}."),
    });
    let from = id(1);
    temp.write_log(&from, &workspace, &exited_line(&from));
    assert!(reach(&hub, &SessionId(from), &SessionId(id(2))).is_none());
    let log = temp.hub_log();
    assert!(log.contains("\"code\":\"io_failed\""), "{log}");
    assert!(log.contains("could not start."), "{log}");
    assert!(!log.contains(SECRET), "no starter text in the log: {log}");
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
        let mut text = String::new();
        self.read
            .read_line(&mut text)
            .unwrap_or_else(|_| panic!("never received {what}"));
        assert!(!text.is_empty(), "the hub closed before {what}");
        serde_json::from_str(&text).unwrap()
    }
}

/// Serves one old session: answers `subscribe`, then `rewind` naming
/// `next`, then holds the socket open until `release` before closing, as
/// a session closing with `rewound` does. The listener binds before the
/// thread starts, so a client routing first always finds it.
fn serve_old(
    listener: UnixListener,
    next: String,
    release: mpsc::Receiver<()>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("old-session".to_owned())
        .spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut read = BufReader::new(stream.try_clone().unwrap());
            let mut write = stream;
            let mut buf = String::new();
            read.read_line(&mut buf).unwrap();
            let id: Value = serde_json::from_str(&buf).unwrap();
            accept(&mut write, &id);
            buf.clear();
            read.read_line(&mut buf).unwrap();
            let rewind: Value = serde_json::from_str(&buf).unwrap();
            let id = rewind.get("id").cloned().unwrap();
            let ack = json!({
                "kind": "command_accepted", "ts": 1, "schema_version": 1,
                "payload": {"command_id": id, "result": {"new_session_id": next}},
            });
            let mut bytes = serde_json::to_vec(&ack).unwrap();
            bytes.push(b'\n');
            write.write_all(&bytes).unwrap();
            write.flush().unwrap();
            // Hold the socket open: the client commands the new session
            // while this relay still waits for EOF.
            release.recv_timeout(DEADLINE).unwrap_or(());
        })
        .unwrap()
}

fn accept(write: &mut UnixStream, command: &Value) {
    let id = command.get("id").cloned().unwrap();
    let ack = json!({
        "kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": id},
    });
    let mut bytes = serde_json::to_vec(&ack).unwrap();
    bytes.push(b'\n');
    write.write_all(&bytes).unwrap();
    write.flush().unwrap();
}

#[test]
fn a_relayed_rewind_starts_and_redirects_before_the_next_command() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::with_handshake(
        &temp.dir,
        Handshake {
            accept: true,
            code: String::new(),
            message: String::new(),
        },
    );
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    // The old session's log already ends `rewound`: the redirect reads it
    // once the old socket closes.
    temp.write_log(&old, &workspace, &rewound_line(&old, &next));
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    let listener = UnixListener::bind(temp.dir.join("run").join(&old)).unwrap();
    let (release_tx, release) = mpsc::channel::<()>();
    let old_session = serve_old(listener, next.clone(), release);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub1", "session_id": old, "command": "subscribe", "args": {"level": "full"},
    }));
    let subscribed = client.next("the subscribe acknowledgement");
    assert_eq!(subscribed["kind"], "command_accepted");
    client.send(&json!({
        "id": "c_rw1", "session_id": old, "command": "rewind", "args": {},
    }));
    let accepted = client.next("the rewind acknowledgement");
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(
        accepted["payload"]["result"]["new_session_id"], next,
        "the client learns the new session"
    );
    assert_eq!(
        starter.rewound(),
        vec![(
            SessionId(next.clone()),
            PathBuf::from(workspace),
            SessionId(old.clone())
        )],
        "started once, in the old workspace, for its rewind"
    );
    drop(release_tx);
    old_session.join().unwrap();
    // The old socket closed: the connection is subscribed to the new
    // session at its kept level, under a hub-minted id it never sees.
    client.send(&json!({
        "id": "c_p1", "session_id": next, "command": "prompt",
        "args": {"content": [{"type": "text", "text": "three"}]},
    }));
    let prompted = client.next("the prompt acknowledgement");
    assert_eq!(prompted["kind"], "command_accepted");
    assert_eq!(prompted["payload"]["command_id"], "c_p1");
}

#[test]
fn a_command_for_the_new_session_first_still_gets_its_level() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    temp.write_log(&old, &workspace, &rewound_line(&old, &next));
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    let listener = UnixListener::bind(temp.dir.join("run").join(&old)).unwrap();
    let (release_tx, release) = mpsc::channel::<()>();
    let old_session = serve_old(listener, next.clone(), release);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub1", "session_id": old, "command": "subscribe", "args": {"level": "full"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    client.send(&json!({
        "id": "c_rw1", "session_id": old, "command": "rewind", "args": {},
    }));
    let accepted = client.next("the rewind acknowledgement");
    assert_eq!(accepted["payload"]["result"]["new_session_id"], next);
    // The interleaving: the client commands the new session while the old
    // relay still waits for EOF, so it relays unsubscribed. Any other
    // command is accepted and held open, as a session that takes it does.
    client.send(&json!({
        "id": "c_t1", "session_id": next, "command": "tools", "args": {},
    }));
    let tools = client.next("the tools acknowledgement");
    assert_eq!(tools["kind"], "command_accepted");
    assert_eq!(tools["payload"]["command_id"], "c_t1");
    // Only now does the old socket close: the redirect transfers the kept
    // level onto that relay instead of starting a second one.
    drop(release_tx);
    old_session.join().unwrap();
    // Both the tools command and the transfer's subscribe reached the new
    // session before this returns.
    assert!(
        starter.await_received(2, DEADLINE),
        "the tools command and the transfer arrive"
    );
    let sent: Vec<String> = starter
        .received()
        .into_iter()
        .filter(|line| line.contains("\"subscribe\""))
        .collect();
    assert_eq!(sent.len(), 1, "the transfer subscribes once: {sent:?}");
    let replay: Value = serde_json::from_str(&sent[0]).unwrap();
    assert_eq!(replay["args"], json!({"level": "full"}));
    assert_ne!(
        replay["id"], "c_sub1",
        "under a hub-minted id the client never sent"
    );
    // The transfer's acknowledgement never reaches the client: every
    // acknowledgement it reads names a command it sent.
    client.send(&json!({
        "id": "c_t2", "session_id": next, "command": "tools", "args": {},
    }));
    let mut seen = Vec::new();
    let acknowledged = loop {
        let line = client.next("the second tools acknowledgement");
        if line
            .get("payload")
            .and_then(|payload| payload.get("command_id"))
            == Some(&Value::String("c_t2".to_owned()))
        {
            break line;
        }
        seen.push(line);
    };
    assert_eq!(acknowledged["kind"], "command_accepted");
    assert!(
        seen.iter().all(|line| line["kind"] != "command_accepted"),
        "no other acknowledgement in between: {seen:?}"
    );
}

#[test]
fn follow_leaves_a_relay_already_holding_the_level_alone() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    temp.write_log(&old, &workspace, &rewound_line(&old, &next));
    let level = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    {
        let mut held = lock(&relays);
        held.subscribed.push((old.clone(), level.clone()));
        held.subscribed.push((next.clone(), level.clone()));
        let (writer, _) = UnixStream::pair().unwrap();
        let epoch = held.mint();
        held.entries.push(crate::relay::Relay {
            session: next.clone(),
            epoch,
            writer,
            kept: crate::relay::Kept::default(),
            replayed: crate::relay::Replayed::default(),
            thread: None,
        });
    }
    let (write, _) = UnixStream::pair().unwrap();
    follow(&hub, &Arc::new(Mutex::new(write)), &relays, &old);
    let held = lock(&relays);
    assert!(starter.rewound().is_empty(), "no second start");
    assert!(starter.resumed().is_empty(), "no resume either");
    assert_eq!(held.entries.len(), 1, "no second relay");
    assert!(
        lock(&held.entries.first().unwrap().replayed).is_empty(),
        "nothing written to the relay"
    );
    assert_eq!(held.subscription(&next), Some(level), "the level stands");
}

#[test]
fn follow_ignores_a_log_that_continues_nowhere() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let old = id(1);
    temp.write_log(&old, &workspace, &exited_line(&old));
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    lock(&relays).subscribed.push((
        old.clone(),
        json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
            .as_object()
            .unwrap()
            .clone(),
    ));
    let (write, _) = UnixStream::pair().unwrap();
    follow(&hub, &Arc::new(Mutex::new(write)), &relays, &old);
    assert!(starter.rewound().is_empty(), "an exit starts nothing");
    assert!(starter.resumed().is_empty(), "an exit resumes nothing");
}

#[test]
fn follow_without_a_kept_level_starts_nothing() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    temp.write_log(&old, &workspace, &rewound_line(&old, &next));
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    let (write, _) = UnixStream::pair().unwrap();
    follow(&hub, &Arc::new(Mutex::new(write)), &relays, &old);
    assert!(starter.rewound().is_empty(), "no level to keep");
    assert!(starter.resumed().is_empty(), "no level to keep");
}

#[test]
fn follow_transfers_the_kept_level_onto_an_unsubscribed_relay() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    temp.write_log(&old, &workspace, &rewound_line(&old, &next));
    // The new session's socket, bound before the test: the transfer
    // writes to the relay's connection, never the starter.
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    let listener = UnixListener::bind(temp.dir.join("run").join(&next)).unwrap();
    let (heard_tx, heard) = mpsc::channel();
    thread::Builder::new()
        .name("next-session".to_owned())
        .spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(DEADLINE)).unwrap();
            let mut read = BufReader::new(stream);
            let mut line = String::new();
            let got = read.read_line(&mut line).unwrap_or(0);
            heard_tx.send((got, line)).unwrap_or(());
        })
        .unwrap();
    let level = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    {
        let mut held = lock(&relays);
        held.subscribed.push((old.clone(), level.clone()));
        // The interleaving: the client commanded the new session while the
        // old relay still waited for EOF, so this relay is unsubscribed.
        let writer = UnixStream::connect(temp.dir.join("run").join(&next)).unwrap();
        let epoch = held.mint();
        held.entries.push(crate::relay::Relay {
            session: next.clone(),
            epoch,
            writer,
            kept: crate::relay::Kept::default(),
            replayed: crate::relay::Replayed::default(),
            thread: None,
        });
    }
    let (write, _) = UnixStream::pair().unwrap();
    follow(&hub, &Arc::new(Mutex::new(write)), &relays, &old);
    let held = lock(&relays);
    assert_eq!(held.entries.len(), 1, "no second relay");
    assert_eq!(
        held.subscription(&next),
        Some(level.clone()),
        "the level is kept for the new session"
    );
    assert!(
        starter.rewound().is_empty(),
        "no start for a running session"
    );
    let (got, line) = heard
        .recv_timeout(DEADLINE)
        .expect("the transfer sends the level");
    assert!(got > 0, "the relay's connection carries it");
    let sent: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(sent["command"], "subscribe");
    assert_eq!(sent["args"], json!({"level": "full"}));
    let minted = sent["id"].as_str().unwrap().to_owned();
    assert_ne!(minted, "c_sub1", "under a hub-minted id");
    assert_eq!(
        lock(&held.entries.first().unwrap().replayed).clone(),
        vec![minted],
        "the relay thread drops its acknowledgement"
    );
}
