//! Tests for one client connection: `hub_hello` first, hub-command
//! parsing, `status`, `start` over the wire, and the relay to sessions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
use crate::fake::FakeStarter;

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

    fn hub(&self, starter: FakeStarter) -> Arc<Hub> {
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
        command("c_6", "status", json!({"level": "full"})),
    ];
    for line in &bad {
        client.send(line);
        let (code, _, _) = rejected(&client.next("the rejection"));
        assert_eq!(code, "invalid_arguments");
    }
    assert!(starter.received().is_empty());
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
    session
        .left
        .recv_timeout(DEADLINE)
        .expect("the session sees its client leave");
}

/// Guards `Mutex` access the way production code does.
fn guard<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Waits, at most one deadline, for the hub to count `n` connections.
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
    done_rx
        .recv_timeout(DEADLINE)
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

#[test]
fn a_stale_relay_never_drops_a_reconnect_to_the_same_session() {
    fn entry(epoch: u64) -> Relay {
        let (writer, _) = UnixStream::pair().unwrap();
        Relay {
            session: "s_0123456789abcdef".to_owned(),
            epoch,
            writer,
        }
    }
    let sid = "s_0123456789abcdef";
    let mut relays = Relays::default();
    let stale = relays.mint();
    relays.entries.push(entry(stale));
    // A failed write drops the entry, leaving the map empty; the
    // reconnect mints its epoch after that.
    relays.entries.remove(0);
    let fresh = relays.mint();
    assert_ne!(fresh, stale);
    relays.entries.push(entry(fresh));
    // The stale relay thread finishes after the reconnect.
    relays.finish(sid, stale);
    assert_eq!(relays.entries.len(), 1, "the reconnect's entry stays");
    assert_eq!(relays.entries[0].epoch, fresh);
    relays.finish(sid, fresh);
    assert!(relays.entries.is_empty(), "a relay drops its own entry");
}

#[test]
fn relay_slots_drop_only_their_own_entry() {
    fn entry(session: &str, epoch: u64) -> Relay {
        let (writer, _) = UnixStream::pair().unwrap();
        Relay {
            session: session.to_owned(),
            epoch,
            writer,
        }
    }
    let entries = [
        entry("s_aaaaaaaaaaaaaaaa", 1),
        entry("s_bbbbbbbbbbbbbbbb", 2),
    ];
    assert_eq!(relay_slot(&entries, "s_aaaaaaaaaaaaaaaa", 1), Some(0));
    assert_eq!(relay_slot(&entries, "s_bbbbbbbbbbbbbbbb", 2), Some(1));
    assert_eq!(relay_slot(&entries, "s_aaaaaaaaaaaaaaaa", 2), None);
    assert_eq!(relay_slot(&entries, "s_bbbbbbbbbbbbbbbb", 1), None);
    assert_eq!(relay_slot(&entries, "s_cccccccccccccccc", 1), None);
    assert_eq!(relay_slot(&[], "s_aaaaaaaaaaaaaaaa", 1), None);
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
