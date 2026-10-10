//! Tests for one client connection: `hub_hello` first, hub-command
//! parsing, `status`, `start` and `sessions` over the wire, and the relay
//! to sessions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
use crate::fake::FakeStarter;
use crate::{Started, Starter};
use contract::events::SessionStatus;
use fakes::Deadline;

/// `wall()` on a fake clock nobody advances, in milliseconds.
const WALL: u64 = 1_700_000_000_000;

/// One named deadline per receive: the client never waits past it.
const DEADLINE: Duration = Duration::from_secs(10);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hc");
        let dir = held.path().join("h");
        fs::create_dir_all(&dir).unwrap();
        Self { dir, held }
    }

    fn hub(&self, starter: impl Starter + 'static) -> Arc<Hub> {
        let timed: Arc<dyn Clock> = fakes::clock::FakeClock::new();
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

    fn send_raw(&mut self, text: &str) {
        self.write.write_all(text.as_bytes()).unwrap();
        self.write.flush().unwrap();
    }

    /// The next line, parsed. Panics past the deadline naming the wait.
    fn next(&mut self, what: &str) -> Value {
        let mut text = String::new();
        self.read
            .read_line(&mut text)
            .unwrap_or_else(|_| panic!("never received {what}"));
        assert!(!text.is_empty(), "the hub closed before {what}");
        serde_json::from_str(&text).unwrap()
    }

    fn hello(&mut self) -> Value {
        self.next("hub_hello")
    }
}

fn command(id: &str, command: &str, args: Value) -> Value {
    json!({"id": id, "command": command, "args": args})
}

fn rejected(line: &Value) -> (String, Option<String>, String) {
    assert_eq!(line.get("kind"), Some(&json!("command_rejected")));
    let payload = line.get("payload").unwrap();
    (
        payload.get("code").unwrap().as_str().unwrap().to_owned(),
        payload
            .get("command_id")
            .map(|id| id.as_str().unwrap().to_owned()),
        payload.get("message").unwrap().as_str().unwrap().to_owned(),
    )
}

fn accepted(line: &Value) -> (String, Value) {
    assert_eq!(line.get("kind"), Some(&json!("command_accepted")));
    let payload = line.get("payload").unwrap();
    (
        payload
            .get("command_id")
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned(),
        payload.get("result").cloned().unwrap_or(Value::Null),
    )
}

#[test]
fn the_first_line_is_hub_hello() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    let hello = client.hello();
    assert_eq!(hello.get("kind"), Some(&json!("hub_hello")));
    assert_eq!(hello.get("schema_version"), Some(&json!(1)));
    assert_eq!(
        hello.get("payload"),
        Some(&json!({"fiber_version": "0.0.0"}))
    );
    assert!(hello.get("session_id").is_none());
}

#[test]
fn malformed_lines_carry_no_command_id_without_a_string_id() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    for raw in [
        "not json\n",
        "[1, 2]\n",
        "\"hub\"\n",
        "{\"id\":7,\"command\":\"status\"}\n",
        "{\"command\":\"status\"}\n",
    ] {
        client.send_raw(raw);
        let (code, id, _) = rejected(&client.next("the rejection"));
        assert_eq!(code, "malformed");
        assert_eq!(id, None);
    }
}

#[test]
fn malformed_lines_echo_a_string_id() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    for raw in [
        "{\"id\":\"c_1\"}\n",
        "{\"id\":\"c_2\",\"command\":\"status\",\"bogus\":1}\n",
        "{\"id\":\"c_3\",\"command\":\"status\",\"args\":[1]}\n",
        "{\"id\":\"c_4\",\"command\":\"status\",\"session_id\":5}\n",
        "{\"id\":\"c_5\",\"command\":7}\n",
    ] {
        client.send_raw(raw);
        let (code, id, _) = rejected(&client.next("the rejection"));
        assert_eq!(code, "malformed");
        assert!(id.unwrap().starts_with("c_"));
    }
}

#[test]
fn names_outside_the_hub_table_are_unknown_command() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    for (id, name) in [
        ("c_1", "frobnicate"),
        ("c_2", "prompt"),
        ("c_3", "subscribe"),
        ("c_4", "pair"),
        ("c_5", "authenticate"),
    ] {
        client.send(&command(id, name, json!({})));
        let (code, echoed, _) = rejected(&client.next("the rejection"));
        assert_eq!(code, "unknown_command");
        assert_eq!(echoed.as_deref(), Some(id));
    }
}

#[test]
fn status_answers_running_version_and_clients() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_1", "status", json!({})));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_1");
    assert_eq!(
        result,
        json!({"running": true, "fiber_version": "0.0.0", "clients": 1})
    );
    let reread: contract::events::CommandAccepted =
        serde_json::from_value(json!({"command_id": "c_1", "result": result})).unwrap();
    assert_eq!(
        reread.result,
        Some(contract::events::CommandResult::Status {
            running: true,
            fiber_version: "0.0.0".into(),
            clients: 1,
        })
    );
    assert_eq!(
        serde_json::to_value(&reread).unwrap(),
        json!({"command_id": "c_1", "result": result})
    );
}

#[test]
fn prompt_history_answers_a_page_and_rejects_a_bad_key() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let project = temp.dir.join("projects/k");
    fs::create_dir_all(&project).unwrap();
    let older = json!({"ts": 1, "session_id": "s_1", "content": [{"type": "text", "text": "a"}]});
    let newer = json!({"ts": 2, "session_id": "s_1", "content": [{"type": "text", "text": "b"}]});
    fs::write(project.join("history.jsonl"), format!("{older}\n{newer}\n")).unwrap();
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_1", "prompt_history", json!({"project": "k"})));
    let (id, result) = accepted(&client.next("the page"));
    assert_eq!(id, "c_1");
    assert_eq!(result, json!({"prompts": [newer, older]}));
    client.send(&command("c_2", "prompt_history", json!({"project": ".."})));
    let (code, id, message) = rejected(&client.next("the rejection"));
    assert_eq!(
        (code.as_str(), id.as_deref(), message.as_str()),
        (
            "invalid_arguments",
            Some("c_2"),
            "The arguments do not fit this command."
        )
    );
}

#[test]
fn status_counts_both_open_connections() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut first = Client::connect(&hub);
    first.hello();
    let mut second = Client::connect(&hub);
    second.hello();
    second.send(&command("c_1", "status", json!({})));
    let (_, result) = accepted(&second.next("the acknowledgement"));
    assert_eq!(result.get("clients"), Some(&json!(2)));
}

#[test]
fn start_rejects_bad_args_without_starting_a_session() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    let bad = [
        command("c_1", "start", json!({"workspace": "relative"})),
        command("c_2", "start", json!({})),
        command("c_3", "start", json!({"workspace": workspace, "bogus": 1})),
        command(
            "c_4",
            "start",
            json!({"workspace": workspace, "model": Value::Null}),
        ),
        command(
            "c_5",
            "start",
            json!({"workspace": "/absent/fiber-hub-test"}),
        ),
        command(
            "c_6",
            "start",
            json!({"workspace": workspace, "worktree": "yes"}),
        ),
        command(
            "c_7",
            "start",
            json!({"workspace": workspace, "worktree": Value::Null}),
        ),
        command("c_8", "status", json!({"level": "full"})),
    ];
    for line in &bad {
        client.send(line);
        let (code, _, _) = rejected(&client.next("the rejection"));
        assert_eq!(code, "invalid_arguments");
    }
    assert!(starter.received().is_empty());
    assert!(starter.started_worktrees().is_empty());
}

#[test]
fn start_answers_with_the_session_id() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command("c_1", "start", json!({"workspace": workspace})));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_1");
    let session = result.get("session_id").unwrap().as_str().unwrap();
    assert!(session.starts_with("s_"));
    assert!(temp.dir.join("run").join(session).exists());
    let reread: contract::events::CommandAccepted =
        serde_json::from_value(json!({"command_id": "c_1", "result": result})).unwrap();
    let Some(contract::events::CommandResult::Start { ref session_id }) = reread.result else {
        panic!("not a start");
    };
    assert_eq!(session_id.0, session);
    assert_eq!(
        serde_json::to_value(&reread).unwrap(),
        json!({"command_id": "c_1", "result": result})
    );
}

#[test]
fn start_with_worktree_true_starts_in_a_worktree() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command(
        "c_1",
        "start",
        json!({"workspace": workspace, "worktree": true}),
    ));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_1");
    let session = result.get("session_id").unwrap().as_str().unwrap();
    assert!(session.starts_with("s_"));
    assert_eq!(starter.started_worktrees(), [true]);
}

#[test]
fn start_with_worktree_false_starts_in_place() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command(
        "c_1",
        "start",
        json!({"workspace": workspace, "worktree": false}),
    ));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_1");
    assert!(result.get("session_id").is_some());
    assert_eq!(starter.started_worktrees(), [false]);
}

#[test]
fn start_without_worktree_starts_in_place() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command("c_1", "start", json!({"workspace": workspace})));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_1");
    assert!(result.get("session_id").is_some());
    assert_eq!(starter.started_worktrees(), [false]);
}

/// A starter that records each `start`'s overrides and binds through
/// [`FakeStarter`].
struct Recording {
    inner: FakeStarter,
    seen: Arc<Mutex<Vec<Vec<String>>>>,
}

impl Recording {
    fn new(home: &Path) -> Self {
        Self {
            inner: FakeStarter::bind_and_hold(home),
            seen: Arc::default(),
        }
    }
}

impl Starter for Recording {
    fn start(
        &self,
        id: &SessionId,
        workspace: &Path,
        model: Option<&str>,
        overrides: &[&str],
        worktree: bool,
    ) -> std::io::Result<Box<dyn Started>> {
        lock(&self.seen).push(overrides.iter().map(|text| text.to_string()).collect());
        self.inner.start(id, workspace, model, &[], worktree)
    }

    fn resume(&self, id: &SessionId, workspace: &Path) -> std::io::Result<Box<dyn Started>> {
        self.inner.resume(id, workspace)
    }

    fn rewind(
        &self,
        id: &SessionId,
        workspace: &Path,
        from: &SessionId,
    ) -> std::io::Result<Box<dyn Started>> {
        self.inner.rewind(id, workspace, from)
    }
}

#[test]
fn start_with_overrides_reaches_the_starter_in_order() {
    let temp = Temp::new();
    let starter = Recording::new(&temp.dir);
    let seen = starter.seen.clone();
    let hub = temp.hub(starter);
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command(
        "c_1",
        "start",
        json!({"workspace": workspace, "overrides": ["retry.attempts=2", "model=fake/m2"]}),
    ));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_1");
    assert!(result.get("session_id").is_some());
    assert_eq!(
        lock(&seen).clone(),
        [vec![
            "retry.attempts=2".to_owned(),
            "model=fake/m2".to_owned()
        ]]
    );
}

#[test]
fn start_with_empty_or_missing_overrides_starts_with_none() {
    for (id, with_empty) in [("c_1", true), ("c_2", false)] {
        let temp = Temp::new();
        let starter = Recording::new(&temp.dir);
        let seen = starter.seen.clone();
        let hub = temp.hub(starter);
        let mut client = Client::connect(&hub);
        client.hello();
        let workspace = temp.workspace();
        let mut args = json!({"workspace": workspace});
        if with_empty {
            args["overrides"] = json!([]);
        }
        client.send(&command(id, "start", args));
        let (echoed, result) = accepted(&client.next("the acknowledgement"));
        assert_eq!(echoed, id);
        assert!(result.get("session_id").is_some());
        assert_eq!(lock(&seen).clone(), [Vec::<String>::new()]);
    }
}

#[test]
fn start_with_malformed_overrides_is_invalid_arguments() {
    let temp = Temp::new();
    let starter = Recording::new(&temp.dir);
    let seen = starter.seen.clone();
    let hub = temp.hub(starter);
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    let bad = [
        json!("retry.attempts=2"),
        json!([1]),
        json!(["retry.attempts=2", 2]),
        json!([Value::Null]),
        Value::Null,
        json!({}),
    ];
    for (n, overrides) in bad.into_iter().enumerate() {
        let id = format!("c_{}", n + 1);
        client.send(&command(
            &id,
            "start",
            json!({"workspace": workspace, "overrides": overrides}),
        ));
        let (code, echoed, _) = rejected(&client.next("the rejection"));
        assert_eq!(code, "invalid_arguments", "{overrides}");
        assert_eq!(echoed.as_deref(), Some(id.as_str()), "{overrides}");
    }
    assert!(lock(&seen).is_empty());
}

/// A pre-bound fake session: it records what the hub forwards and answers
/// each line with `reply`.
struct FakeSession {
    received: Arc<Mutex<Vec<String>>>,
    left: mpsc::Receiver<()>,
}

fn bind_session(home: &Path, sid: &str, reply: Value) -> FakeSession {
    let run = home.join("run");
    fs::create_dir_all(&run).unwrap();
    let listener = UnixListener::bind(run.join(sid)).unwrap();
    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (left_tx, left_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-session".to_owned())
        .spawn({
            let received = Arc::clone(&received);
            move || {
                let reply = reply;
                loop {
                    let Ok((stream, _)) = listener.accept() else {
                        return;
                    };
                    let received = Arc::clone(&received);
                    let reply = reply.clone();
                    let left_tx = left_tx.clone();
                    thread::spawn(move || {
                        let mut read = BufReader::new(stream.try_clone().unwrap());
                        let mut buf = Vec::new();
                        loop {
                            buf.clear();
                            match read.read_until(b'\n', &mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {
                                    received
                                        .lock()
                                        .unwrap_or_else(PoisonError::into_inner)
                                        .push(String::from_utf8_lossy(&buf).into_owned());
                                    let mut bytes = serde_json::to_vec(&reply).unwrap();
                                    bytes.push(b'\n');
                                    if stream
                                        .try_clone()
                                        .and_then(|mut back| {
                                            back.write_all(&bytes)?;
                                            back.flush()
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                        }
                        left_tx.send(()).unwrap_or(());
                    });
                }
            }
        })
        .unwrap();
    FakeSession {
        received,
        left: left_rx,
    }
}

fn received(session: &FakeSession) -> Vec<String> {
    guard(&session.received).clone()
}

#[test]
fn relay_strips_session_id_and_returns_session_lines() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let sid = "s_0123456789abcdef";
    let ack = json!({
        "kind": "command_accepted",
        "session_id": sid,
        "ts": 1,
        "schema_version": 1,
        "payload": {"command_id": "c_4"},
    });
    let session = bind_session(&temp.dir, sid, ack.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&json!({
        "id": "c_4",
        "session_id": sid,
        "command": "subscribe",
        "args": {"level": "full"},
    }));
    let line = client.next("the relayed acknowledgement");
    assert_eq!(line, ack);
    let forwarded = received(&session);
    assert_eq!(forwarded.len(), 1);
    let sent: Value = serde_json::from_str(&forwarded[0]).unwrap();
    assert_eq!(
        sent,
        json!({"args": {"level": "full"}, "command": "subscribe", "id": "c_4"})
    );
    assert!(sent.get("session_id").is_none());
}

#[test]
fn one_connection_carries_two_sessions() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let (first, second) = ("s_1111111111111111", "s_2222222222222222");
    let line_for = |sid: &str, id: &str| {
        json!({
            "kind": "session_status",
            "session_id": sid,
            "ts": 1,
            "schema_version": 1,
            "payload": {"command_id": id},
        })
    };
    bind_session(&temp.dir, first, line_for(first, "c_1"));
    bind_session(&temp.dir, second, line_for(second, "c_2"));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&json!({
        "id": "c_1", "session_id": first, "command": "subscribe", "args": {"level": "full"},
    }));
    client.send(&json!({
        "id": "c_2", "session_id": second, "command": "subscribe", "args": {"level": "full"},
    }));
    let mut lines = [
        client.next("the first stream"),
        client.next("the second stream"),
    ];
    lines.sort_by_key(|line| {
        line.get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    });
    assert_eq!(lines[0].get("session_id"), Some(&json!(first)));
    assert_eq!(lines[1].get("session_id"), Some(&json!(second)));
}

#[test]
fn a_session_with_no_socket_is_session_not_found() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&json!({
        "id": "c_9",
        "session_id": "s_absent0000000000",
        "command": "subscribe",
        "args": {"level": "full"},
    }));
    let (code, id, _) = rejected(&client.next("the rejection"));
    assert_eq!(code, "session_not_found");
    assert_eq!(id.as_deref(), Some("c_9"));
}

#[test]
fn a_closing_client_shuts_down_every_relay_stream() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let sid = "s_0123456789abcdef";
    let session = bind_session(
        &temp.dir,
        sid,
        json!({"kind": "command_accepted", "ts": 1, "schema_version": 1, "payload": {}}),
    );
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&json!({
        "id": "c_1", "session_id": sid, "command": "subscribe", "args": {"level": "full"},
    }));
    client.next("the relayed line");
    drop(client);
    Deadline::after(DEADLINE)
        .recv(&session.left)
        .expect("the session sees its client leave");
}

/// Guards `Mutex` access the way production code does.
fn guard<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Waits, at most one deadline, for the hub to count `n` connections.
#[track_caller]
fn until_clients(hub: &Arc<Hub>, n: usize, what: &str) {
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-clients".to_owned())
        .spawn({
            let hub = Arc::clone(hub);
            move || {
                while hub.clients() != n {
                    std::thread::yield_now();
                }
                done_tx.send(()).unwrap_or(());
            }
        })
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .unwrap_or_else(|_| panic!("the hub counts {n} connections {what}"));
}

#[test]
fn session_ids_outside_the_minted_shape_are_session_not_found() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    // Planted live sockets the hub must never touch: `run/hub` for `"hub"`,
    // `home/x` for `"../x"`, and an absolute path outside `run/`.
    let run = temp.dir.join("run");
    fs::create_dir_all(&run).unwrap();
    let hub_socket = UnixListener::bind(run.join("hub")).unwrap();
    let escape = UnixListener::bind(temp.dir.join("x")).unwrap();
    let outside = UnixListener::bind(temp.dir.join("outside.sock")).unwrap();
    for listener in [&hub_socket, &escape, &outside] {
        listener.set_nonblocking(true).unwrap();
    }
    let outside_id = temp.dir.join("outside.sock").to_string_lossy().into_owned();
    assert!(outside_id.starts_with('/'));
    let mut client = Client::connect(&hub);
    client.hello();
    for (id, session) in [
        ("c_1", outside_id),
        ("c_2", "../x".to_owned()),
        ("c_3", "hub".to_owned()),
        ("c_4", "/absent/fiber-hub-test".to_owned()),
        ("c_5", "s_ABCDEF0123456789".to_owned()),
        ("c_6", "s_0123456789abcde".to_owned()),
    ] {
        client.send(&json!({
            "id": id,
            "session_id": session,
            "command": "subscribe",
            "args": {"level": "full"},
        }));
        let (code, echoed, _) = rejected(&client.next("the rejection"));
        assert_eq!(code, "session_not_found", "{session}");
        assert_eq!(echoed.as_deref(), Some(id));
    }
    // None of the planted sockets saw a connection.
    for listener in [&hub_socket, &escape, &outside] {
        assert!(listener.accept().is_err());
    }
}

/// A hub on `clock`, for tests that move time.
fn hub_on(temp: &Temp, clock: &Arc<fakes::clock::FakeClock>) -> Hub {
    let timed = Arc::clone(clock);
    let timed: Arc<dyn Clock> = timed;
    Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(FakeStarter::hang(&temp.dir)),
        Arc::clone(&timed),
        Diag::open(&temp.dir, timed),
    )
}

#[test]
fn the_idle_timer_starts_when_the_open_count_reaches_zero() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let hub = hub_on(&temp, &clock);
    assert_eq!(hub.zero_since(), Some(clock.origin()), "start counts as 0");
    let (a, _a) = UnixStream::pair().unwrap();
    let (b, _b) = UnixStream::pair().unwrap();
    let first = hub.register(&a).expect("the clone succeeds");
    assert_eq!(hub.zero_since(), None, "an open client clears the timer");
    let second = hub.register(&b).expect("the clone succeeds");
    clock.advance(Duration::from_secs(5));
    disconnect(&hub, first);
    assert_eq!(hub.zero_since(), None, "one client is still open");
    clock.advance(Duration::from_secs(5));
    disconnect(&hub, second);
    assert_eq!(hub.zero_since(), Some(clock.now()), "0 at the departure");
    assert_eq!(hub.clients(), 0);
}

#[test]
fn a_failed_clone_counts_nothing_and_serves_nothing() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let hub = hub_on(&temp, &clock);
    let failed: std::io::Result<UnixStream> = Err(std::io::Error::other("no fd"));
    let mut conns = lock(&hub.conns);
    assert!(hub.register_with(|| failed, &mut conns).is_none());
    drop(conns);
    assert_eq!(hub.clients(), 0);
    assert_eq!(hub.zero_since(), Some(clock.origin()), "the timer runs on");
}

#[test]
fn rollback_removes_the_count_and_restarts_the_timer_at_zero() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let hub = hub_on(&temp, &clock);
    let (a, _a) = UnixStream::pair().unwrap();
    let (b, _b) = UnixStream::pair().unwrap();
    let first = hub.register(&a).expect("the clone succeeds");
    let second = hub.register(&b).expect("the clone succeeds");
    assert_eq!(hub.clients(), 2);
    clock.advance(Duration::from_secs(5));
    hub.rollback(first);
    assert_eq!(hub.clients(), 1);
    assert_eq!(hub.zero_since(), None, "one client is still open");
    clock.advance(Duration::from_secs(5));
    hub.rollback(second);
    assert_eq!(hub.clients(), 0);
    assert_eq!(hub.zero_since(), Some(clock.now()), "0 at the rollback");
}

#[test]
fn client_numbers_start_at_one() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut first = Client::connect(&hub);
    first.hello();
    let mut second = Client::connect(&hub);
    second.hello();
    let log = fs::read_to_string(temp.dir.join("logs").join("hub.log")).unwrap();
    assert!(log.contains("Client 1 connected."));
    assert!(log.contains("Client 2 connected."));
}

#[test]
fn client_lines_hold_no_workspace_or_model_text() {
    const SECRET: &str = "the-volume-of-the-meeting-room";
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    // Plant path and model secrets the client lines must never hold: the
    // count `n` is hub-minted, the sentences fixed.
    let workspace = temp.dir.join(format!("w-{SECRET}"));
    fs::create_dir_all(&workspace).unwrap();
    client.send(&command(
        "c_1",
        "start",
        json!({"workspace": workspace.to_string_lossy(), "model": SECRET}),
    ));
    let (_, _) = accepted(&client.next("the acknowledgement"));
    drop(client);
    until_clients(&hub, 0, "after the departure");
    let log = fs::read_to_string(temp.dir.join("logs").join("hub.log")).unwrap();
    assert!(log.contains("Client 1 connected."));
    assert!(log.contains("Client 1 disconnected."));
    assert!(
        !log.contains(SECRET),
        "no workspace or model text in client lines"
    );
}

#[test]
fn start_with_null_content_is_invalid_arguments() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command(
        "c_1",
        "start",
        json!({"workspace": workspace, "content": Value::Null}),
    ));
    let (code, _, _) = rejected(&client.next("the rejection"));
    assert_eq!(code, "invalid_arguments");
}

/// Seeds `temp`'s `recent.jsonl` with session `id` crashed, its directory
/// made, and starts the hub's feed.
fn crashed_feed(temp: &Temp, hub: &Arc<Hub>, id: &str) {
    fs::create_dir_all(crate::recent::session_dir(&temp.dir, "p", id)).unwrap();
    let row: crate::RecentRow = serde_json::from_value(json!({
        "session_id": id, "ts": 3, "project": "p", "workspace": "/w", "name": "n",
        "how": "crashed", "status": crate::fake::status("n", "/w", "idle", None),
    }))
    .unwrap();
    crate::append(&temp.dir, &row).unwrap();
    hub.feed.start();
}

/// Waits under [`DEADLINE`] until the feed holds `n` subscribers.
#[track_caller]
fn until_subscribers(hub: &Arc<Hub>, n: usize, what: &str) {
    let (done_tx, done_rx) = mpsc::channel();
    let hub = Arc::clone(hub);
    thread::spawn(move || {
        while hub.feed.subscribers() != n {
            std::thread::yield_now();
        }
        done_tx.send(()).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .unwrap_or_else(|_| panic!("the feed holds {n} subscribers {what}"));
}

#[test]
fn feed_is_accepted_then_sends_the_snapshot_and_leaves_with_the_client() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let id = "s_00000000000000c1";
    crashed_feed(&temp, &hub, id);
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_0", "feed", json!({"x": 1})));
    let (code, echoed, _) = rejected(&client.next("the rejection"));
    assert_eq!(code, "invalid_arguments");
    assert_eq!(echoed.as_deref(), Some("c_0"));
    for round in ["c_1", "c_2"] {
        client.send(&command(round, "feed", json!({})));
        let ack = client.next("the acknowledgement");
        accepted(&ack);
        assert_eq!(ack.get("payload"), Some(&json!({"command_id": round})));
        let status = client.next("the crashed status");
        assert_eq!(status.get("kind"), Some(&json!("session_status")));
        assert_eq!(status.get("session_id"), Some(&json!(id)));
        let left = client.next("its session_left");
        assert_eq!(left.get("kind"), Some(&json!("session_left")));
        assert_eq!(
            left.get("payload"),
            Some(&json!({"session_id": id, "how": "crashed"}))
        );
        // A second `feed` replaces the first subscription.
        until_subscribers(&hub, 1, "on this connection");
    }
    drop(client);
    until_subscribers(&hub, 0, "after the client left");
    hub.feed.stop();
}

#[test]
fn dismiss_over_the_wire_drops_a_crashed_session_for_every_client() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let id = "s_00000000000000c1";
    crashed_feed(&temp, &hub, id);
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_1", "dismiss", json!({"session": 5})));
    let (code, _, _) = rejected(&client.next("the bad dismiss"));
    assert_eq!(code, "invalid_arguments");
    client.send(&command("c_2", "dismiss", json!({"session": id})));
    let ack = client.next("the dismissal");
    accepted(&ack);
    assert_eq!(ack.get("payload"), Some(&json!({"command_id": "c_2"})));
    client.send(&command("c_3", "dismiss", json!({"session": id})));
    let (code, echoed, _) = rejected(&client.next("the second dismissal"));
    assert_eq!(code, "stale_request");
    assert_eq!(echoed.as_deref(), Some("c_3"));
    // A fresh feed is sent nothing for it: its acknowledgement, then the
    // answer to the next command.
    client.send(&command("c_4", "feed", json!({})));
    accepted(&client.next("the feed acknowledgement"));
    client.send(&command("c_5", "status", json!({})));
    let (echoed, _) = accepted(&client.next("the status answer"));
    assert_eq!(echoed, "c_5");
    hub.feed.stop();
}

#[test]
fn recent_over_the_wire_answers_a_page() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let id = "s_00000000000000c1";
    crashed_feed(&temp, &hub, id);
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_1", "recent", json!({"project": "p"})));
    let (echoed, result) = accepted(&client.next("the page"));
    assert_eq!(echoed, "c_1");
    let sessions = result.get("sessions").unwrap().as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions.first().unwrap().get("session_id"),
        Some(&json!(id))
    );
    client.send(&command(
        "c_2",
        "recent",
        json!({"before": "s_0000000000000999"}),
    ));
    let (code, echoed, _) = rejected(&client.next("the unknown before"));
    assert_eq!(code, "invalid_arguments");
    assert_eq!(echoed.as_deref(), Some("c_2"));
    hub.feed.stop();
}

#[test]
fn sessions_over_the_wire_answers_live_and_exited_once() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let id = "s_00000000000000c1";
    crashed_feed(&temp, &hub, id);
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&json!({"id": "c_1", "command": "sessions"}));
    let (echoed, result) = accepted(&client.next("the listing"));
    assert_eq!(echoed, "c_1");
    assert_eq!(result.get("live"), Some(&json!([])));
    let exited = result.get("exited").unwrap().as_array().unwrap();
    assert_eq!(exited.len(), 1);
    assert_eq!(exited.first().unwrap().get("session_id"), Some(&json!(id)));
    client.send(&command("c_2", "sessions", json!({"project": 1})));
    let (code, echoed, _) = rejected(&client.next("the bad project"));
    assert_eq!(code, "invalid_arguments");
    assert_eq!(echoed.as_deref(), Some("c_2"));
    stop_within(&hub);
}

/// Stops the feed on a thread and receives its return under [`DEADLINE`]:
/// joining its threads blocks.
#[track_caller]
fn stop_within(hub: &Arc<Hub>) {
    let (done_tx, done_rx) = mpsc::channel();
    let hub = Arc::clone(hub);
    thread::spawn(move || {
        hub.feed.stop();
        done_tx.send(()).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the feed stops");
}

/// Waits under [`DEADLINE`] until attention holds `n` listeners.
#[track_caller]
fn until_listeners(hub: &Arc<Hub>, n: usize, what: &str) {
    let (done_tx, done_rx) = mpsc::channel();
    let hub = Arc::clone(hub);
    thread::spawn(move || {
        while hub.feed.attention.listeners() != n {
            std::thread::yield_now();
        }
        done_tx.send(()).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .unwrap_or_else(|_| panic!("attention holds {n} listeners {what}"));
}

/// A `session_status` payload with a `waiting` state: `request_id` and
/// `summary "run <request_id>"`, named `n` in `/w`.
fn waiting_payload(request: &str) -> Value {
    let mut payload = crate::fake::status("n", "/w", "idle", None);
    let map = payload.as_object_mut().unwrap();
    map.insert("state".to_owned(), json!("waiting"));
    map.insert(
        "waiting".to_owned(),
        json!({
            "request_id": request,
            "kind": "approval",
            "summary": format!("run {request}"),
        }),
    );
    payload
}

fn waiting_status(request: &str) -> SessionStatus {
    serde_json::from_value(waiting_payload(request)).unwrap()
}

#[test]
fn attention_reaches_every_connection_after_hub_hello_with_or_without_a_feed() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let id = "s_00000000000000a1";
    let session = crate::fake::FakeSession::bind(&temp.dir, id);
    session.say(&crate::fake::status_line(
        id,
        &crate::fake::status("n", "/w", "streaming", None),
    ));
    hub.feed.start();
    assert!(session.await_subscribed(1, DEADLINE));
    let mut a = Client::connect(&hub);
    a.hello();
    a.send(&command("c_1", "feed", json!({})));
    let (echoed, _) = accepted(&a.next("the feed acknowledgement"));
    assert_eq!(echoed, "c_1");
    assert_eq!(a.next("the snapshot status")["kind"], "session_status");
    let mut b = Client::connect(&hub);
    b.hello();
    until_listeners(&hub, 2, "after A and B connected");
    // C's connection thread waits on the pause point before its hello.
    let (reached_tx, reached_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *guard(&hub.before_hello) = Some(Box::new(move || {
        reached_tx.send(()).unwrap_or(());
        release_rx.recv().unwrap_or(());
    }));
    let mut c = Client::connect(&hub);
    Deadline::after(DEADLINE)
        .recv(&reached_rx)
        .expect("C reaches its hello");
    hub.feed
        .attention
        .notify("s_00000000000000ff", None, &waiting_status("rh"), false);
    let held = b.next("the held attention");
    assert_eq!(held["payload"]["reason"], "waiting");
    assert_eq!(held["payload"]["summary"], "run rh");
    release_tx.send(()).unwrap_or(());
    assert_eq!(c.hello()["kind"], "hub_hello");
    until_listeners(&hub, 3, "after C registered");
    // Every connection hears the turn's waiting, but C never hears `rh`.
    session.say(&crate::fake::status_line(id, &waiting_payload("r1")));
    let r1 = json!({
        "kind": "attention", "ts": WALL, "schema_version": 1,
        "payload": {
            "name": "n", "reason": "waiting", "session_id": id,
            "summary": "run r1", "workspace": "/w",
        },
    });
    assert_eq!(b.next("B's attention"), r1);
    assert_eq!(c.next("C's attention"), r1);
    let mut got = [
        a.next("A's first line"),
        a.next("A's second line"),
        a.next("A's third line"),
    ]
    .map(|line| serde_json::to_string(&line).unwrap());
    got.sort();
    let mut want = [
        serde_json::from_str::<Value>(&crate::fake::status_line(id, &waiting_payload("r1")))
            .unwrap(),
        r1,
        held,
    ]
    .map(|line| serde_json::to_string(&line).unwrap());
    want.sort();
    assert_eq!(got, want);
    drop(a);
    drop(b);
    drop(c);
    until_listeners(&hub, 0, "after every client left");
    stop_within(&hub);
}

#[test]
fn a_client_that_half_closes_without_reading_still_leaves() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut d = Client::connect(&hub);
    d.hello();
    until_listeners(&hub, 1, "after D connected");
    // About 4 MB past any socket buffer, so the writer blocks.
    for n in 0..2_000 {
        let mut status = waiting_status(&format!("r{n}"));
        status.name = format!("{n}:{}", "x".repeat(2_000));
        hub.feed
            .attention
            .notify("s_00000000000000ff", None, &status, false);
    }
    d.write.shutdown(Shutdown::Write).unwrap();
    until_listeners(&hub, 0, "after D half-closed");
    until_clients(&hub, 0, "after D half-closed");
    stop_within(&hub);
}

/// A hub on a fake clock whose sessions accept the first prompt, with the
/// clock and the starter.
fn content_hub(temp: &Temp) -> (Arc<Hub>, Arc<fakes::clock::FakeClock>, FakeStarter) {
    let clock = fakes::clock::FakeClock::new();
    let starter = FakeStarter::with_handshake(
        &temp.dir,
        crate::fake::Handshake {
            accept: true,
            code: String::new(),
            message: String::new(),
        },
    );
    let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    let hub = Arc::new(Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(starter.clone()),
        Arc::clone(&timed),
        Diag::open(&temp.dir, timed),
    ));
    (hub, clock, starter)
}

fn content() -> Value {
    json!([{"type": "text", "text": "hi"}])
}

/// Sends `start` with content on `client` and returns the session id and
/// the instant the first prompt's bound is due: the clock does not move
/// after the answer is written.
fn start_with_content(
    client: &mut Client,
    temp: &Temp,
    clock: &fakes::clock::FakeClock,
) -> (String, std::time::Instant) {
    let workspace = temp.workspace();
    client.send(&command(
        "c_start",
        "start",
        json!({"workspace": workspace, "content": content()}),
    ));
    let (id, result) = accepted(&client.next("the start answer"));
    assert_eq!(id, "c_start");
    let session = result["session_id"].as_str().unwrap().to_owned();
    (session, clock.now() + FIRST_PROMPT_WAIT)
}

fn relayed_subscribe(id: &str, session: &str, level: &str) -> Value {
    json!({"id": id, "session_id": session, "command": "subscribe", "args": {"level": level}})
}

/// The commands the fake sessions received, in order.
fn commands(starter: &FakeStarter) -> Vec<Value> {
    starter
        .received()
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn prompts(starter: &FakeStarter) -> usize {
    commands(starter)
        .iter()
        .filter(|line| line["command"] == "prompt")
        .count()
}

/// The first-prompt thread re-checked after an advance of `by` and is
/// still waiting for `due`.
fn still_waiting(clock: &fakes::clock::FakeClock, due: std::time::Instant, by: Duration) {
    let mark = clock.advance_marked(by);
    assert!(
        clock.await_parked_since(&mark, Some(due), DEADLINE),
        "the first prompt re-checked and still waits"
    );
}

#[test]
fn start_with_content_answers_before_the_prompt() {
    let temp = Temp::new();
    let (hub, clock, starter) = content_hub(&temp);
    let mut client = Client::connect(&hub);
    client.hello();
    let (_, due) = start_with_content(&mut client, &temp, &clock);
    assert!(clock.await_parked(due, DEADLINE), "the bound is armed");
    let received = commands(&starter);
    assert_eq!(received.len(), 1, "only the hub's subscription so far");
    assert_eq!(received[0]["args"]["level"], "summary");
}

#[test]
fn a_full_subscribe_on_the_requesting_connection_releases_the_prompt() {
    let temp = Temp::new();
    let (hub, clock, starter) = content_hub(&temp);
    let mut client = Client::connect(&hub);
    client.hello();
    let (session, _) = start_with_content(&mut client, &temp, &clock);
    client.send(&relayed_subscribe("c_sub", &session, "full"));
    // The clock never moves: only the subscription releases the prompt.
    assert!(starter.await_received(3, DEADLINE));
    let received = commands(&starter);
    assert_eq!(received[0]["args"]["level"], "summary");
    assert_eq!(received[1]["id"], "c_sub");
    assert_eq!(received[2]["command"], "prompt");
    assert_eq!(received[2]["args"]["content"], content());
    let (id, _) = accepted(&client.next("the subscription's answer"));
    assert_eq!(id, "c_sub");
}

#[test]
fn a_raise_from_summary_to_full_releases_the_prompt() {
    let temp = Temp::new();
    let (hub, clock, starter) = content_hub(&temp);
    let mut client = Client::connect(&hub);
    client.hello();
    let (session, due) = start_with_content(&mut client, &temp, &clock);
    client.send(&relayed_subscribe("c_summary", &session, "summary"));
    let (id, _) = accepted(&client.next("the summary answer"));
    assert_eq!(id, "c_summary");
    assert!(clock.await_parked(due, DEADLINE));
    still_waiting(&clock, due, Duration::ZERO);
    assert_eq!(prompts(&starter), 0);
    client.send(&relayed_subscribe("c_full", &session, "full"));
    assert!(starter.await_received(4, DEADLINE));
    let received = commands(&starter);
    assert_eq!(received[2]["id"], "c_full");
    assert_eq!(received[3]["command"], "prompt");
}

#[test]
fn a_summary_subscribe_waits_for_the_bound() {
    let temp = Temp::new();
    let (hub, clock, starter) = content_hub(&temp);
    let mut client = Client::connect(&hub);
    client.hello();
    let (session, due) = start_with_content(&mut client, &temp, &clock);
    client.send(&relayed_subscribe("c_summary", &session, "summary"));
    let _answer = accepted(&client.next("the summary answer"));
    assert!(clock.await_parked(due, DEADLINE));
    still_waiting(&clock, due, Duration::ZERO);
    assert_eq!(prompts(&starter), 0);
    still_waiting(&clock, due, Duration::from_millis(999));
    assert_eq!(prompts(&starter), 0, "one millisecond before the bound");
    clock.advance(Duration::from_millis(1));
    assert!(starter.await_received(3, DEADLINE));
    assert_eq!(prompts(&starter), 1, "the prompt goes at the bound");
}

#[test]
fn a_full_subscribe_on_another_connection_does_not_release() {
    let temp = Temp::new();
    let (hub, clock, starter) = content_hub(&temp);
    let mut client = Client::connect(&hub);
    client.hello();
    let (session, due) = start_with_content(&mut client, &temp, &clock);
    let mut other = Client::connect(&hub);
    other.hello();
    other.send(&relayed_subscribe("c_other", &session, "full"));
    let (id, _) = accepted(&other.next("the other client's answer"));
    assert_eq!(id, "c_other");
    assert!(clock.await_parked(due, DEADLINE));
    still_waiting(&clock, due, Duration::ZERO);
    assert_eq!(prompts(&starter), 0);
    clock.advance(FIRST_PROMPT_WAIT);
    assert!(starter.await_received(3, DEADLINE));
    assert_eq!(prompts(&starter), 1);
}

#[test]
fn a_failed_answer_write_still_arms_the_bound() {
    let temp = Temp::new();
    let (hub, clock, starter) = content_hub(&temp);
    // The requester is gone before the answer: its write fails.
    let (dead, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let dead = Arc::new(Mutex::new(dead));
    let id = CommandId("c_1".to_owned());
    let relays: Arc<Mutex<Relays>> = Arc::default();
    let args = json!({"workspace": temp.workspace(), "content": content()});
    let args = args.as_object().unwrap().clone();
    let (done_tx, done) = mpsc::channel();
    let started = Arc::clone(&hub);
    thread::spawn(move || {
        on_start(&id, &args, &started, &dead, &relays);
        done_tx.send(()).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("start returned");
    let due = clock.now() + FIRST_PROMPT_WAIT;
    assert!(clock.await_parked(due, DEADLINE), "the bound is armed");
    clock.advance(FIRST_PROMPT_WAIT);
    assert!(starter.await_received(2, DEADLINE));
    assert_eq!(prompts(&starter), 1);
}

#[test]
fn a_requester_that_disconnects_releases_the_prompt_at_once() {
    let temp = Temp::new();
    let (hub, clock, starter) = content_hub(&temp);
    let mut client = Client::connect(&hub);
    client.hello();
    let _started = start_with_content(&mut client, &temp, &clock);
    drop(client);
    // The clock never moves: the close releases the prompt.
    assert!(starter.await_received(2, DEADLINE));
    assert_eq!(prompts(&starter), 1);
    until_clients(&hub, 0, "after the requester left");
    assert_eq!(prompts(&starter), 1, "sent exactly once");
}

#[test]
fn a_connection_that_never_subscribes_gets_the_prompt_at_the_bound() {
    let temp = Temp::new();
    let (hub, clock, starter) = content_hub(&temp);
    let mut client = Client::connect(&hub);
    client.hello();
    let (session, due) = start_with_content(&mut client, &temp, &clock);
    assert!(clock.await_parked(due, DEADLINE));
    clock.advance(FIRST_PROMPT_WAIT);
    assert!(starter.await_received(2, DEADLINE));
    client.send(&relayed_subscribe("c_late", &session, "full"));
    let (id, _) = accepted(&client.next("the late subscription's answer"));
    assert_eq!(id, "c_late");
    assert_eq!(commands(&starter).len(), 3);
    assert_eq!(prompts(&starter), 1, "a later subscription sends nothing");
}

#[test]
fn start_content_that_does_not_fit_a_prompt_is_invalid_arguments() {
    let temp = Temp::new();
    let (hub, _clock, starter) = content_hub(&temp);
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    let rows = [
        json!("hi"),
        json!({}),
        json!([{"type": "bogus"}]),
        json!([{"type": "text"}]),
        json!([{"type": "text", "text": "a", "extra": 1}]),
    ];
    for content in rows {
        client.send(&command(
            "c_bad",
            "start",
            json!({"workspace": workspace, "content": content}),
        ));
        let (code, id, message) = rejected(&client.next("the rejection"));
        assert_eq!(code, "invalid_arguments", "{content}");
        assert_eq!(id.as_deref(), Some("c_bad"));
        assert_eq!(message, "The arguments do not fit this command.");
    }
    assert!(starter.received().is_empty());
    let sockets = fs::read_dir(temp.dir.join("run")).map_or(0, Iterator::count);
    assert_eq!(sockets, 0, "no session started");
}
