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
    start(&hub, &SessionId(id(1)), &SessionId(next.clone()));
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
    start(&hub, &SessionId(from), &SessionId(next.clone()));
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
    start(&hub, &SessionId(from.clone()), &SessionId(next.clone()));
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
            move || start(&hub, &SessionId(from), &SessionId(next))
        })
        .unwrap();
    start(&hub, &SessionId(from), &SessionId(next));
    other.join().unwrap();
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
                release.recv().unwrap_or(());
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
            move || start(&hub, &SessionId(from), &SessionId(held))
        })
        .unwrap();
    // The slow start is inside `rewind`, holding only its own session's
    // lock: no sleep, the channel says so.
    assert!(
        entered.recv_timeout(DEADLINE).is_ok(),
        "the slow start reached the starter"
    );
    start(&hub, &SessionId(from), &SessionId(other.clone()));
    assert!(
        UnixStream::connect(temp.dir.join("run").join(&other)).is_ok(),
        "the other session starts while the slow one waits"
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
    start(&hub, &SessionId(from), &SessionId(id(2)));
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
/// `next`, then closes, as a session closing with `rewound` does.
fn serve_old(socket: PathBuf, next: String) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("old-session".to_owned())
        .spawn(move || {
            let listener = UnixListener::bind(&socket).unwrap();
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
    let old_session = serve_old(temp.dir.join("run").join(&old), next.clone());
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
fn follow_leaves_a_connection_already_relaying_the_next_session() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    temp.write_log(&old, &workspace, &rewound_line(&old, &next));
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    {
        let mut held = lock(&relays);
        held.subscribed.push((
            old.clone(),
            json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
                .as_object()
                .unwrap()
                .clone(),
        ));
        let (writer, _) = UnixStream::pair().unwrap();
        let epoch = held.mint();
        held.entries.push(crate::relay::Relay {
            session: next,
            epoch,
            writer,
            kept: crate::relay::Kept::default(),
            thread: None,
        });
    }
    let (write, _) = UnixStream::pair().unwrap();
    follow(&hub, &Arc::new(Mutex::new(write)), &relays, &old);
    assert!(starter.rewound().is_empty(), "no second start");
    assert!(starter.resumed().is_empty(), "no resume either");
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
