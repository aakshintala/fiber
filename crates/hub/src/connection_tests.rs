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
use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
use crate::fake::{FakeMachine, FakeStarter};
use crate::testkit::{await_true, stop_within};
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
        let (held, dir) = crate::testkit::home("hc");
        Self { dir, held }
    }

    fn hub(&self, starter: impl Starter + 'static) -> Arc<Hub> {
        let timed: Arc<dyn Clock> = fakes::clock::FakeClock::new();
        Arc::new(crate::testkit::hub(&self.dir, Arc::new(starter), timed))
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
            .spawn(move || match hub.poll_accept(&a, &AtomicBool::new(false)) {
                Accept::Counted(n) => serve_counted(a, hub, n),
                Accept::Dropped => panic!("test connection stream could not be cloned"),
                Accept::Exiting => panic!("fresh test hub unexpectedly exiting"),
            })
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

    /// A `status` command padded with trailing spaces to `total` bytes,
    /// then one line feed: trailing spaces parse as whitespace.
    fn send_padded(&mut self, id: &str, total: usize) {
        let mut bytes = serde_json::to_vec(&command(id, "status", json!({}))).unwrap();
        assert!(bytes.len() < total, "the padded line must grow");
        bytes.resize(total, b' ');
        bytes.push(b'\n');
        self.write.write_all(&bytes).unwrap();
        self.write.flush().unwrap();
    }

    /// The same, with no line feed: the hub reads an unfinished line.
    fn send_padded_no_lf(&mut self, total: usize) {
        let mut bytes = serde_json::to_vec(&command("c_9", "status", json!({}))).unwrap();
        assert!(bytes.len() < total, "the padded line must grow");
        bytes.resize(total, b' ');
        self.write.write_all(&bytes).unwrap();
        self.write.flush().unwrap();
    }

    /// One socket line under one deadline: the blocking read runs on a
    /// thread and its result is received under one deadline, never parking
    /// the test thread past the wait. Panics past the deadline naming the
    /// wait.
    fn read_deadline(&mut self, what: &str) -> (std::io::Result<usize>, String) {
        let (done_tx, done_rx) = mpsc::channel();
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut text = String::new();
                let outcome = self.read.read_line(&mut text);
                done_tx.send((outcome, text)).unwrap_or(());
            });
            Deadline::after(DEADLINE)
                .recv(&done_rx)
                .unwrap_or_else(|_| panic!("never received {what}"))
        })
    }

    /// The next read is end of file within one deadline, with no answer
    /// before it.
    fn closed(&mut self, what: &str) {
        let (outcome, text) = self.read_deadline(what);
        let n = outcome.unwrap_or_else(|_| panic!("the hub never closes {what}"));
        assert!(
            text.is_empty(),
            "the hub answers before it closes {what}: {text}"
        );
        assert_eq!(n, 0, "the hub never closes {what}");
    }

    /// The next line, parsed.
    fn next(&mut self, what: &str) -> Value {
        let (outcome, text) = self.read_deadline(what);
        outcome.unwrap_or_else(|_| panic!("never received {what}"));
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

#[test]
fn accepted_local_connection_retains_its_kernel_attribution() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_status", "status", json!({})));
    let ack = client.next("the status acknowledgement");
    assert_eq!(ack.get("kind"), Some(&json!("command_accepted")));

    let ids = hub.rejoins.ids();
    assert_eq!(ids.len(), 1, "the accepted client is registered");
    assert!(
        matches!(
            hub.rejoins.attribution(ids[0]),
            Some(Attribution::Local { peer_pid, session_process_pids })
                if peer_pid == support::peer::Pid(std::process::id()) && session_process_pids.is_empty()
        ),
        "reroutes keep the attribution read from this accepted socket"
    );
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

/// The payload of the first line a hub built with `commit` sends.
fn hello_payload_for(commit: Option<&str>) -> Value {
    let temp = Temp::new();
    let timed: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let hub = Arc::new(
        crate::testkit::hub(&temp.dir, Arc::new(FakeStarter::hang(&temp.dir)), timed)
            .with_commit(commit),
    );
    let mut client = Client::connect(&hub);
    client.hello()["payload"].clone()
}

#[test]
fn hub_hello_carries_the_commit_the_hub_was_built_from() {
    let sha = "2121f38d1e4f0a9b6c3d2e1f0a9b8c7d6e5f4a3b";
    assert_eq!(
        hello_payload_for(Some(sha)),
        json!({"fiber_version": "0.0.0", "fiber_commit": sha})
    );
}

#[test]
fn hub_hello_sends_no_commit_for_an_empty_or_missing_one() {
    let bare = json!({"fiber_version": "0.0.0"});
    assert_eq!(hello_payload_for(Some("")), bare);
    assert_eq!(hello_payload_for(None), bare);
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
fn stop_from_a_local_client_is_accepted_and_records_the_stop() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_stop", "stop", json!({})));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_stop");
    assert_eq!(result, Value::Null);
    // The store landed: the idle wait ends the hub as a SIGTERM does,
    // with no clock advance.
    assert!(matches!(
        idle_until_signal(&hub),
        Idle::Signal(signal_hook::consts::SIGTERM)
    ));
}

#[test]
fn stop_with_arguments_is_invalid_arguments_and_stops_nothing() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_1", "stop", json!({"level": "full"})));
    let (code, echoed, message) = rejected(&client.next("the rejection"));
    assert_eq!(code, "invalid_arguments");
    assert_eq!(echoed.as_deref(), Some("c_1"));
    assert_eq!(message, "The arguments do not fit this command.");
    // No stop flag: a fresh accept still counts.
    let (served, _peer) = UnixStream::pair().unwrap();
    assert!(matches!(
        hub.poll_accept(&served, &AtomicBool::new(false)),
        Accept::Counted(_)
    ));
}

#[test]
fn stop_with_false_sessions_is_accepted_with_no_result_and_exits_at_once() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_stop", "stop", json!({"sessions": false})));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_stop");
    assert_eq!(result, Value::Null);
    // A false `sessions` is the base path: the store lands with no loop.
    assert!(matches!(
        idle_until_signal(&hub),
        Idle::Signal(signal_hook::consts::SIGTERM)
    ));
}

#[test]
fn stop_with_sessions_and_none_running_answers_zero() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::exit_on_close(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_stop", "stop", json!({"sessions": true})));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_stop");
    assert_eq!(result, json!({"sessions_stopped": 0}));
    assert!(matches!(
        idle_until_signal(&hub),
        Idle::Signal(signal_hook::consts::SIGTERM)
    ));
}

#[test]
fn stop_with_sessions_and_an_unreadable_list_answers_io_failed() {
    let temp = Temp::new();
    // A file where `run/` belongs: the listing fails, so the stop names
    // no session as stopped and answers `io_failed` instead of a count.
    fs::write(temp.dir.join("run"), b"").unwrap();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_stop", "stop", json!({"sessions": true})));
    let (code, id, message) = rejected(&client.next("the acknowledgement"));
    assert_eq!(code, "io_failed");
    assert_eq!(id, Some("c_stop".to_owned()));
    assert_eq!(message, "The session list could not be read.");
    assert!(matches!(
        idle_until_signal(&hub),
        Idle::Signal(signal_hook::consts::SIGTERM)
    ));
}

#[test]
fn stop_with_sessions_stops_the_running_session() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::exit_on_close(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command("c_1", "start", json!({"workspace": workspace})));
    let (_, result) = accepted(&client.next("the acknowledgement"));
    let session = result
        .get("session_id")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    // Resume bleed: another connection subscribes before the stop; the
    // session is still stopped once, through its own shutdown.
    let mut other = Client::connect(&hub);
    other.hello();
    other.send(&json!({
        "id": "c_bleed",
        "session_id": session,
        "command": "subscribe",
        "args": {"level": "summary"},
    }));
    let (bleed, _) = accepted(&other.next("the bleed subscribe"));
    assert_eq!(bleed, "c_bleed");
    client.send(&command("c_stop", "stop", json!({"sessions": true})));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_stop");
    assert_eq!(result, json!({"sessions_stopped": 1}));
    assert!(matches!(
        idle_until_signal(&hub),
        Idle::Signal(signal_hook::consts::SIGTERM)
    ));
}

#[test]
fn start_is_refused_while_a_sessions_stop_runs() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    hub.sessions_stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command("c_1", "start", json!({"workspace": workspace})));
    let (code, echoed, message) = rejected(&client.next("the rejection"));
    assert_eq!(code, "io_failed");
    assert_eq!(echoed.as_deref(), Some("c_1"));
    assert_eq!(message, "the hub is stopping.");
    assert!(starter.received().is_empty());
}

#[test]
fn a_subscribe_to_an_exited_session_is_refused_while_a_sessions_stop_runs() {
    // Through `resume_exited`, which reaches the refusal with no opener:
    // the log ends in `fiber_exited`, and a session answering `closing`
    // binds its socket so the relay sees the exited window.
    const SID: &str = "s_0123456789abcdef";
    let temp = Temp::new();
    let workspace = temp.workspace();
    crate::testkit::write_log(
        &temp.dir,
        SID,
        &workspace,
        None,
        &[json!({
            "kind": "fiber_exited",
            "session_id": SID,
            "ts": 2,
            "schema_version": 1,
            "payload": {},
        })],
    );
    let dying = FakeStarter::closing_every_command(&temp.dir);
    crate::Starter::start(
        &dying,
        &SessionId(SID.to_owned()),
        Path::new("/w"),
        None,
        &[],
        false,
        None,
        false,
    )
    .expect("the closing session binds");
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    hub.sessions_stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&json!({
        "id": "c_1",
        "session_id": SID,
        "command": "subscribe",
        "args": {"level": "summary"},
    }));
    let (code, echoed, message) = rejected(&client.next("the refusal"));
    assert_eq!(code, "io_failed");
    assert_eq!(echoed.as_deref(), Some("c_1"));
    assert_eq!(message, "the hub is stopping.");
}

#[test]
fn stop_rejects_a_non_boolean_sessions() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    for (id, sessions) in [
        ("c_1", json!({"sessions": "yes"})),
        ("c_2", json!({"sessions": Value::Null})),
        ("c_3", json!({"sessions": true, "extra": 1})),
    ] {
        client.send(&command(id, "stop", sessions));
        let (code, echoed, message) = rejected(&client.next("the rejection"));
        assert_eq!(code, "invalid_arguments");
        assert_eq!(echoed.as_deref(), Some(id));
        assert_eq!(message, "The arguments do not fit this command.");
    }
    // The hub keeps running: a fresh accept still counts.
    let (served, _peer) = UnixStream::pair().unwrap();
    assert!(matches!(
        hub.poll_accept(&served, &AtomicBool::new(false)),
        Accept::Counted(_)
    ));
}

#[test]
fn stop_from_a_device_is_accepted_and_records_the_stop() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let dir = temp.dir.join("hub").join("devices");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("phone"),
        format!(
            "{{\"token_sha256\":\"{}\"}}",
            crate::devices::sha256_hex(b"tok-planted")
        ),
    )
    .unwrap();
    let who = hub
        .pairing
        .authenticate(&hub, 1, "tok-planted")
        .expect("the device authenticates");
    // An authenticated connection hears no `hub_hello`, as on the port.
    let (served, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(DEADLINE)).unwrap();
    let hub_for_thread = Arc::clone(&hub);
    thread::Builder::new()
        .name("hub-conn".to_owned())
        .spawn(move || serve_authenticated(served, hub_for_thread, 1, who, None))
        .unwrap();
    let mut bytes = serde_json::to_vec(&command("c_1", "stop", json!({}))).unwrap();
    bytes.push(b'\n');
    peer.try_clone().unwrap().write_all(&bytes).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let mut read = BufReader::new(peer);
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut text = String::new();
            let outcome = read.read_line(&mut text);
            done_tx.send((outcome, text)).unwrap_or(());
        });
        let (outcome, text) = Deadline::after(DEADLINE)
            .recv(&done_rx)
            .unwrap_or_else(|_| panic!("never received the rejection"));
        outcome.unwrap_or_else(|_| panic!("the hub never answers"));
        let line: Value = serde_json::from_str(&text).unwrap();
        let (id, result) = accepted(&line);
        assert_eq!(id, "c_1");
        assert_eq!(result, Value::Null);
    });
    await_true("the hub to stop after answering", {
        let hub = Arc::clone(&hub);
        move || hub.stop_requested()
    });
}

#[test]
fn stop_with_sessions_from_a_device_is_accepted() {
    let temp = Temp::new();
    let dir = temp.dir.join("hub").join("devices");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("phone"),
        format!(
            "{{\"token_sha256\":\"{}\"}}",
            crate::devices::sha256_hex(b"tok-planted")
        ),
    )
    .unwrap();
    let hub = temp.hub(FakeStarter::exit_on_close(&temp.dir));
    let who = hub
        .pairing
        .authenticate(&hub, 1, "tok-planted")
        .expect("the device authenticates");
    // An authenticated connection hears no `hub_hello`, as on the port.
    let (served, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(DEADLINE)).unwrap();
    let hub_for_thread = Arc::clone(&hub);
    thread::Builder::new()
        .name("hub-conn".to_owned())
        .spawn(move || serve_authenticated(served, hub_for_thread, 1, who, None))
        .unwrap();
    let mut bytes = serde_json::to_vec(&command("c_1", "stop", json!({"sessions": true}))).unwrap();
    bytes.push(b'\n');
    peer.try_clone().unwrap().write_all(&bytes).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let mut read = BufReader::new(peer);
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut text = String::new();
            let outcome = read.read_line(&mut text);
            done_tx.send((outcome, text)).unwrap_or(());
        });
        let (outcome, text) = Deadline::after(DEADLINE)
            .recv(&done_rx)
            .unwrap_or_else(|_| panic!("never received the acknowledgement"));
        outcome.unwrap_or_else(|_| panic!("the hub never answers"));
        let line: Value = serde_json::from_str(&text).unwrap();
        let (id, result) = accepted(&line);
        assert_eq!(id, "c_1");
        assert_eq!(result, json!({"sessions_stopped": 0}));
    });
    await_true("the hub to stop after answering", {
        let hub = Arc::clone(&hub);
        move || hub.stop_requested()
    });
}

/// A written but undrained answer waits out the bound: the write and
/// its flag land before the wait starts, the wait holds to the bound,
/// and the answer goes out whole. An `||` mutant would return as soon
/// as the write lands.
#[test]
fn a_written_but_undrained_answer_waits_out_the_bound() {
    let temp = Temp::new();
    let clock = Arc::new(fakes::clock::FakeClock::new());
    let hub = hub_with_clock(&temp.dir, FakeStarter::hang(&temp.dir), Arc::clone(&clock));
    // The spawn refusal runs the writer on the driving thread: the
    // answer and the written flag land before the wait starts.
    *hub.fail_spawn.lock().unwrap() = Some("hub-stop-out");
    let web = crate::port::Web::never_drained(false);
    let (writer, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(DEADLINE)).unwrap();
    let writer = Arc::new(Mutex::new(writer));
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("drain-web-test".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let web = Arc::clone(&web);
            let writer = Arc::clone(&writer);
            move || {
                drain_web(
                    &hub,
                    &writer,
                    &web,
                    "command_accepted",
                    serde_json::Map::from_iter([(
                        "command_id".to_owned(),
                        Value::String("c_1".to_owned()),
                    )]),
                );
                done_tx.send(()).unwrap_or(());
            }
        })
        .unwrap();
    // The answer is out while the wait still parks on the bound: the
    // mutant would have returned already.
    let mut read = BufReader::new(peer);
    let mut text = String::new();
    read.read_line(&mut text).unwrap();
    let end = clock.origin() + crate::port::STOP_CLOSE_WAIT;
    assert!(
        clock.await_parked(end, fakes::Deadline::after(DEADLINE)),
        "the answer waits for the drain"
    );
    clock.advance_when_parked(
        Some(end),
        None,
        crate::port::STOP_CLOSE_WAIT,
        &fakes::Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE).recv_or_fail(&done_rx, "the wait ends at the bound");
    let line: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        line.get("kind").and_then(Value::as_str),
        Some("command_accepted")
    );
    assert_eq!(
        line.pointer("/payload/command_id").and_then(Value::as_str),
        Some("c_1")
    );
    // EOF after the answer: the writer shut its side down.
    let mut rest = String::new();
    read.read_to_string(&mut rest).unwrap();
    assert!(rest.is_empty());
}

/// An unwritten answer with a drained web still waits: the writer blocks
/// in a full socket, so `written` stays unset while `drained` holds. An
/// `||` mutant would return at once.
#[test]
fn an_unwritten_answer_with_a_drained_web_still_waits() {
    let temp = Temp::new();
    let clock = Arc::new(fakes::clock::FakeClock::new());
    let hub = hub_with_clock(&temp.dir, FakeStarter::hang(&temp.dir), Arc::clone(&clock));
    let web = crate::port::Web::never_drained(true);
    let (writer, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(DEADLINE)).unwrap();
    // Fill the writer's direction past its buffer: the writer blocks in
    // its write, so `written` stays unset deterministically.
    writer.set_nonblocking(true).unwrap();
    let filler = vec![0u8; 65_536];
    for _ in 0..128 {
        match (&writer).write(&filler) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("filling the send buffer: {error}"),
        }
    }
    writer.set_nonblocking(false).unwrap();
    let writer = Arc::new(Mutex::new(writer));
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("drain-web-test".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let web = Arc::clone(&web);
            let writer = Arc::clone(&writer);
            move || {
                drain_web(
                    &hub,
                    &writer,
                    &web,
                    "command_accepted",
                    serde_json::Map::from_iter([(
                        "command_id".to_owned(),
                        Value::String("c_2".to_owned()),
                    )]),
                );
                done_tx.send(()).unwrap_or(());
            }
        })
        .unwrap();
    let end = clock.origin() + crate::port::STOP_CLOSE_WAIT;
    assert!(
        clock.await_parked(end, fakes::Deadline::after(DEADLINE)),
        "the unwritten answer waits"
    );
    // Drain the filler past the answer to EOF: the freed room unblocks
    // the writer, and the shutdown ends the read.
    let mut all = Vec::new();
    BufReader::new(peer).read_to_end(&mut all).unwrap();
    Deadline::after(DEADLINE).recv_or_fail(&done_rx, "the wait ends after the drain");
    let text = String::from_utf8(all).unwrap();
    // The filler carries no newline, so the answer is not the last
    // line: it starts where the JSON starts.
    let start = text
        .find("{\"kind\"")
        .expect("the answer follows the filler");
    let line: Value = serde_json::from_str(&text[start..]).unwrap();
    assert_eq!(
        line.get("kind").and_then(Value::as_str),
        Some("command_accepted")
    );
    assert_eq!(
        line.pointer("/payload/command_id").and_then(Value::as_str),
        Some("c_2")
    );
}

/// Idle-wait passes on a thread until one is not `Woken`, as the hub's own
/// loop does: a pass parked before a stop's store returns `Woken` when the
/// wake lands, and the next pass sees the stop.
fn spawn_idle_until_signal(hub: &Arc<Hub>) -> mpsc::Receiver<Idle> {
    let (done_tx, done) = mpsc::channel();
    let hub = Arc::clone(hub);
    thread::spawn(move || {
        let outcome = loop {
            match hub.idle_wait(
                Duration::from_secs(60),
                &AtomicBool::new(false),
                &AtomicI32::new(0),
            ) {
                Idle::Woken => {}
                other @ (Idle::Signal(_) | Idle::Expired) => break other,
            }
        };
        done_tx.send(outcome).unwrap_or(());
    });
    done
}

/// [`spawn_idle_until_signal`], received under one deadline.
fn idle_until_signal(hub: &Arc<Hub>) -> Idle {
    Deadline::after(DEADLINE).recv_or_fail(
        &spawn_idle_until_signal(hub),
        "the idle wait to see the stop",
    )
}

/// A hub on `clock`, held by the test for advances: the stop loop parks
/// on the injected clock, so bound-expiry tests drive it from here.
fn hub_with_clock(
    dir: &Path,
    starter: impl Starter + 'static,
    clock: Arc<fakes::clock::FakeClock>,
) -> Arc<Hub> {
    let timed: Arc<dyn Clock> = clock;
    Arc::new(crate::testkit::hub(dir, Arc::new(starter), timed))
}

#[test]
fn stop_with_sessions_names_the_survivor_past_the_close_bound() {
    // A strict session answers `subscribe` and `close` but never exits:
    // the loop's close-wait expires with the socket still open.
    let temp = Temp::new();
    let clock = Arc::new(fakes::clock::FakeClock::new());
    let hub = hub_with_clock(
        &temp.dir,
        FakeStarter::strict(&temp.dir),
        Arc::clone(&clock),
    );
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command("c_1", "start", json!({"workspace": workspace})));
    let (_, result) = accepted(&client.next("the acknowledgement"));
    let session = result
        .get("session_id")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    // The hook records the loop thread before the stop is sent, so the
    // advance below can name its wait: under load the session worker's
    // read wait can park at the same bound.
    let (fanout_tx, fanout_rx) = mpsc::channel();
    *hub.on_fanout_wait.lock().unwrap() = Some(Box::new(move || {
        fanout_tx.send(thread::current().id()).unwrap_or(());
    }));
    client.send(&command("c_stop", "stop", json!({"sessions": true})));
    // The loop's fan-out wait parks on the scan bound with the session
    // unconfirmed; expiring it names the survivor.
    let bound = clock.origin() + crate::sessions_stop::SESSIONS_STOP_WITHIN;
    assert!(
        clock.await_parked(bound, fakes::Deadline::after(DEADLINE)),
        "the loop waits for the close"
    );
    let loop_thread = Deadline::after(DEADLINE)
        .recv(&fanout_rx)
        .unwrap_or_else(|_| panic!("the loop enters its fan-out wait"));
    clock.advance_when_parked(
        Some(bound),
        Some(loop_thread),
        crate::sessions_stop::SESSIONS_STOP_WITHIN,
        &Deadline::after(DEADLINE),
    );
    let (code, echoed, message) = rejected(&client.next("the rejection"));
    assert_eq!(code, "io_failed");
    assert_eq!(echoed.as_deref(), Some("c_stop"));
    assert!(
        message.contains(&session),
        "the rejection names the survivor: {message}"
    );
    // The hub still exits after the partial answer.
    assert!(matches!(
        idle_until_signal(&hub),
        Idle::Signal(signal_hook::consts::SIGTERM)
    ));
}

/// A starter that holds each `start` at a gate, then runs the inner
/// starter: the test stores the sessions-stop flag mid-bind.
struct GateStarter {
    inner: FakeStarter,
    entered: mpsc::Sender<()>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Starter for GateStarter {
    fn start(
        &self,
        id: &SessionId,
        workspace: &Path,
        model: Option<&str>,
        overrides: &[&str],
        worktree: bool,
        continues: Option<&SessionId>,
        supervised: bool,
    ) -> std::io::Result<Box<dyn Started>> {
        self.entered.send(()).unwrap_or(());
        if let Some(release) = support::lock(&self.release).take() {
            release.recv().unwrap_or(());
        }
        self.inner.start(
            id, workspace, model, overrides, worktree, continues, supervised,
        )
    }

    fn resume(
        &self,
        id: &SessionId,
        workspace: &Path,
        credential: Option<&str>,
    ) -> std::io::Result<Box<dyn Started>> {
        self.inner.resume(id, workspace, credential)
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
fn a_start_paused_between_admit_steps_is_refused_after_the_store() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    // The start pauses after its increment, before its flag load.
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    *hub.between_admit_steps.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap_or(());
        release.recv().unwrap_or(());
    }));
    // The stop's zero-wait fires when it first sees the nonzero count.
    let (waiting_tx, waiting) = mpsc::channel();
    *hub.on_inflight_wait.lock().unwrap() = Some(Box::new(move || {
        waiting_tx.send(()).unwrap_or(());
    }));
    let mut starter_client = Client::connect(&hub);
    starter_client.hello();
    let workspace = temp.workspace();
    starter_client.send(&command("c_1", "start", json!({"workspace": workspace})));
    assert!(
        Deadline::after(DEADLINE).recv(&entered).is_ok(),
        "the start pauses between the admit steps"
    );
    let mut stopper = Client::connect(&hub);
    stopper.hello();
    stopper.send(&command("c_stop", "stop", json!({"sessions": true})));
    assert!(
        Deadline::after(DEADLINE).recv(&waiting).is_ok(),
        "the stop parks on the nonzero count"
    );
    drop(release_tx);
    // The released start loads the flag set: refused, never started.
    let (code, echoed, message) = rejected(&starter_client.next("the refusal"));
    assert_eq!(code, "io_failed");
    assert_eq!(echoed.as_deref(), Some("c_1"));
    assert_eq!(message, "the hub is stopping.");
    assert!(
        starter.started_worktrees().is_empty(),
        "the starter never ran"
    );
    // The stop then scans nothing and answers zero: never an orphan.
    let (id, result) = accepted(&stopper.next("the acknowledgement"));
    assert_eq!(id, "c_stop");
    assert_eq!(result, json!({"sessions_stopped": 0}));
    assert!(!temp.dir.join("run").exists(), "no session socket accepts");
    assert!(matches!(
        idle_until_signal(&hub),
        Idle::Signal(signal_hook::consts::SIGTERM)
    ));
}

#[test]
fn a_start_caught_mid_bind_is_stopped_and_refused() {
    let temp = Temp::new();
    let inner = FakeStarter::hang(&temp.dir);
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let hub = temp.hub(GateStarter {
        inner: inner.clone(),
        entered: entered_tx,
        release: Mutex::new(Some(release)),
    });
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command("c_1", "start", json!({"workspace": workspace})));
    assert!(
        Deadline::after(DEADLINE).recv(&entered).is_ok(),
        "the start reaches the starter"
    );
    // The store lands mid-bind: the first retry stops and refuses.
    hub.sessions_stopping.store(true, Ordering::SeqCst);
    drop(release_tx);
    let (code, echoed, message) = rejected(&client.next("the refusal"));
    assert_eq!(code, "io_failed");
    assert_eq!(echoed.as_deref(), Some("c_1"));
    assert_eq!(message, "the hub is stopping.");
    assert_eq!(inner.started_worktrees().len(), 1);
    assert_eq!(inner.kills().len(), 1, "the started session is killed");
    assert!(!temp.dir.join("run").exists(), "no session socket accepts");
    assert_eq!(hub.inflight_binds.load(Ordering::SeqCst), 0);
}

#[test]
fn resume_is_refused_at_entry_after_the_store() {
    let temp = Temp::new();
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command("c_1", "start", json!({"workspace": workspace})));
    let (_, result) = accepted(&client.next("the acknowledgement"));
    let session = result
        .get("session_id")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    // The socket is gone but the session is resumable: a subscribe
    // takes the resume path, not the rejoin.
    assert!(
        starter.stop(&SessionId(session.clone()), DEADLINE),
        "the session socket goes away"
    );
    hub.sessions_stopping.store(true, Ordering::SeqCst);
    let mut other = Client::connect(&hub);
    other.hello();
    other.send(&json!({
        "id": "c_2",
        "session_id": session,
        "command": "subscribe",
        "args": {"level": "summary"},
    }));
    let (code, echoed, message) = rejected(&other.next("the refusal"));
    assert_eq!(code, "io_failed");
    assert_eq!(echoed.as_deref(), Some("c_2"));
    assert_eq!(message, "the hub is stopping.");
    assert!(starter.resumed().is_empty(), "the starter never resumes");
}

#[test]
fn a_bare_stop_leaves_a_running_session_alone() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::exit_on_close(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command("c_1", "start", json!({"workspace": workspace})));
    let (_, result) = accepted(&client.next("the acknowledgement"));
    let session = result
        .get("session_id")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    client.send(&command("c_stop", "stop", json!({})));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_stop");
    assert_eq!(result, Value::Null, "a bare stop carries no result");
    assert!(matches!(
        idle_until_signal(&hub),
        Idle::Signal(signal_hook::consts::SIGTERM)
    ));
    // The session outlives the hub: its socket still accepts.
    assert!(
        UnixStream::connect(temp.dir.join("run").join(&session)).is_ok(),
        "the session socket still accepts"
    );
}

#[test]
fn an_idle_wait_parked_before_the_bare_stop_store_still_ends_on_the_stop() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let hub = hub_with_clock(&temp.dir, FakeStarter::hang(&temp.dir), Arc::clone(&clock));
    // The stop pauses after its acknowledgement, before the store.
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    *hub.bare_stop_pause.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap_or(());
        release.recv().unwrap_or(());
    }));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_stop", "stop", json!({})));
    let (id, _) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_stop");
    Deadline::after(DEADLINE).recv_or_fail(&entered, "the stop to pause before its store");
    // The helper's first pass parks on the clock with no stop stored.
    let done = spawn_idle_until_signal(&hub);
    assert!(
        clock.await_parked_unbounded(fakes::Deadline::after(DEADLINE)),
        "the idle wait parks before the store"
    );
    drop(release_tx);
    // Whatever the first pass returns, the helper ends on the stop.
    assert!(matches!(
        Deadline::after(DEADLINE).recv_or_fail(&done, "the idle wait to see the stop"),
        Idle::Signal(signal_hook::consts::SIGTERM)
    ));
}

/// The import answer the driver answers with, scripted through the
/// machine: names and reasons only, never secret values.
fn import_report() -> Value {
    json!({
        "imported": {
            "mcp_servers": ["notes"],
            "skills": ["review"],
            "commands": [],
            "instructions": [],
        },
        "left_behind": [
            {"item": "auth.json", "reason": "credentials are never imported"},
        ],
    })
}

#[test]
fn import_runs_through_the_machine() {
    let temp = Temp::new();
    let timed: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let machine = Arc::new(FakeMachine::default());
    machine
        .imports
        .lock()
        .unwrap()
        .extend([Ok(Some(import_report())), Ok(Some(import_report()))]);
    // The hub machine's person home, which the source homes hang
    // under: the environments carry it, the `directories` precedent.
    let person = temp.dir.join("person");
    let environments = crate::Environments::new(
        vec![(
            std::ffi::OsString::from("HOME"),
            person.as_os_str().to_owned(),
        )],
        &temp.dir,
    );
    let hub = Arc::new(
        crate::testkit::hub(&temp.dir, Arc::new(FakeStarter::hang(&temp.dir)), timed)
            .with_machine(machine.clone())
            .with_environments(environments),
    );
    let mut client = Client::connect(&hub);
    client.hello();
    // Bare imports both sources; a named source imports that one.
    client.send(&command("c_1", "import", json!({})));
    let (id, result) = accepted(&client.next("the bare answer"));
    assert_eq!(id, "c_1");
    assert_eq!(result, import_report());
    client.send(&command("c_2", "import", json!({"source": "codex"})));
    let (id, result) = accepted(&client.next("the sourced answer"));
    assert_eq!(id, "c_2");
    assert_eq!(result, import_report());
    // An unknown source and a non-string source are refused
    // `invalid_arguments`, naming the two sources, mirroring the
    // driver; an extra key is refused the same code, the `directories`
    // precedent.
    for (id, args) in [
        ("c_3", json!({"source": "vim"})),
        ("c_4", json!({"source": 1})),
    ] {
        client.send(&command(id, "import", args));
        let (code, echoed, message) = rejected(&client.next("the refusal"));
        assert_eq!(code, "invalid_arguments");
        assert_eq!(echoed.as_deref(), Some(id));
        assert!(
            message.contains("claude-code") && message.contains("codex"),
            "{message}"
        );
    }
    client.send(&command(
        "c_5",
        "import",
        json!({"source": "codex", "extra": true}),
    ));
    let (code, echoed, _) = rejected(&client.next("the extra-key refusal"));
    assert_eq!(code, "invalid_arguments");
    assert_eq!(echoed.as_deref(), Some("c_5"));
    // The machine saw the two accepted runs with the hub machine's
    // person home, and no refused one.
    let calls = machine.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["import"]["source"], Value::Null);
    assert_eq!(calls[1]["import"]["source"], json!("codex"));
    assert_eq!(
        calls[0]["import"]["home"],
        Value::String(person.to_string_lossy().into_owned()),
        "the run reads the hub machine's person home"
    );
    assert_eq!(
        calls[0]["import"]["home"], calls[1]["import"]["home"],
        "both runs read the same person home"
    );
}

#[test]
fn import_without_a_machine_is_unknown_command() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    // Without a machine the `import` arm's guard misses, like the other
    // machine commands: the hub refuses `unknown_command` instead of
    // running the import.
    client.send(&command("c_1", "import", json!({})));
    let (code, echoed, message) = rejected(&client.next("the refusal"));
    assert_eq!(code, "unknown_command");
    assert_eq!(echoed.as_deref(), Some("c_1"));
    assert!(
        message.contains("`import` is not a hub command."),
        "{message}"
    );
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
        json!({"running": true, "fiber_version": "0.0.0", "clients": 1,
            "variables": {"path": "", "names": [], "source": "inherited"}})
    );
    let reread: contract::events::CommandAccepted =
        serde_json::from_value(json!({"command_id": "c_1", "result": result})).unwrap();
    assert_eq!(
        reread.result,
        Some(contract::events::CommandResult::Status {
            running: true,
            fiber_version: "0.0.0".into(),
            clients: 1,
            variables: contract::events::Variables {
                path: "".into(),
                names: Vec::new(),
                source: contract::events::VariablesSource::Inherited,
            },
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
fn placeholder_env_answers_requested_values_and_omits_the_rest() {
    let temp = Temp::new();
    let hub = {
        let timed: Arc<dyn Clock> = fakes::clock::FakeClock::new();
        let hub = crate::testkit::hub(&temp.dir, Arc::new(FakeStarter::hang(&temp.dir)), timed);
        Arc::new(
            hub.with_environments(crate::Environments::ready(crate::Environment {
                vars: vec![
                    ("PRESENT".into(), "kept".into()),
                    ("UNASKED".into(), "stays".into()),
                ],
                source: contract::events::VariablesSource::Inherited,
            })),
        )
    };
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command(
        "c_1",
        "placeholder_env",
        json!({"names": ["PRESENT", "ABSENT"]}),
    ));
    let (id, result) = accepted(&client.next("the placeholder_env answer"));
    assert_eq!(id, "c_1");
    assert_eq!(result, json!({"values": {"PRESENT": "kept"}}));
    let reread: contract::events::CommandAccepted =
        serde_json::from_value(json!({"command_id": "c_1", "result": result})).unwrap();
    assert_eq!(
        reread.result,
        Some(contract::events::CommandResult::PlaceholderEnv {
            values: [("PRESENT".to_owned(), "kept".to_owned())]
                .into_iter()
                .collect(),
        })
    );
}

#[test]
fn placeholder_env_rejects_a_misshaped_request() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    for (id, args) in [
        ("c_1", json!({})),
        ("c_2", json!({"names": "PRESENT"})),
        ("c_3", json!({"names": ["PRESENT"], "extra": true})),
        ("c_4", json!({"names": null})),
    ] {
        client.send(&command(id, "placeholder_env", args));
        let (code, answered, message) = rejected(&client.next("the rejection"));
        assert_eq!(
            (code.as_str(), answered.as_deref(), message.as_str()),
            (
                "invalid_arguments",
                Some(id),
                "The arguments do not fit this command.",
            )
        );
    }
}

#[test]
fn client_lines_end_at_the_cap_before_any_parse() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    hub.line_cap.store(256, Ordering::SeqCst);
    // Just below and exactly at the cap, the padded status is answered.
    let mut client = Client::connect(&hub);
    client.hello();
    client.send_padded("c_1", 255);
    let (id, _) = accepted(&client.next("the 255-byte acknowledgement"));
    assert_eq!(id, "c_1");
    client.send_padded("c_2", 256);
    let (id, _) = accepted(&client.next("the 256-byte acknowledgement"));
    assert_eq!(id, "c_2");
    // One byte past the cap with no line feed, the hub closes with no answer.
    let mut over = Client::connect(&hub);
    over.hello();
    over.send_padded_no_lf(257);
    over.closed("after the over-long line with no feed");
    // One byte past the cap with a line feed, the valid JSON is refused
    // unparsed: the hub closes with no answer.
    let mut framed = Client::connect(&hub);
    framed.hello();
    framed.send_padded("c_3", 257);
    framed.closed("after the over-long framed line");
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
        command(
            "c_9",
            "start",
            json!({"workspace": workspace, "continues": 1}),
        ),
        command(
            "c_10",
            "start",
            json!({"workspace": workspace, "continues": Value::Null}),
        ),
        command(
            "c_11",
            "start",
            json!({"workspace": workspace, "continues": {"id": "s_aaaaaaaaaaaaaaaa"}}),
        ),
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
fn start_with_supervised_true_records_supervised() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    client.send(&command(
        "c_1",
        "start",
        json!({"workspace": workspace, "supervised": true}),
    ));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_1");
    assert!(result.get("session_id").is_some());
    // One accepted `start` starts one session (`docs/invocation.md`, "The
    // hub", `start`: "Starts a session in the workspace ... and answers with
    // its `session_id`"), so the single launch carries the request's flag.
    assert_eq!(starter.started_supervised(), [true]);
}

#[test]
fn start_without_supervised_records_not_supervised() {
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
    // One accepted `start` starts one session (`docs/invocation.md`, "The
    // hub", `start`: "Starts a session in the workspace ... and answers with
    // its `session_id`"), so the single launch carries the absent flag as
    // false.
    assert_eq!(starter.started_supervised(), [false]);
}

#[test]
fn start_rejects_a_misshaped_supervised() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    let workspace = temp.workspace();
    for (name, supervised) in [
        ("c_1", json!("yes")),
        ("c_2", Value::Null),
        ("c_3", json!({"supervised": true})),
    ] {
        client.send(&command(
            name,
            "start",
            json!({"workspace": workspace, "supervised": supervised}),
        ));
        let (code, _, _) = rejected(&client.next("the rejection"));
        assert_eq!(code, "invalid_arguments", "{name}");
    }
    // A rejected `start` launches nothing: `supervised` is a boolean,
    // optional arg (`docs/invocation.md`, "The hub", `start`), and a request
    // that does not fit is rejected `invalid_arguments` before any session
    // starts (same page: a `content` that does not fit `prompt`'s `args` is
    // "rejected `invalid_arguments` before any session starts"; the arg
    // validation in `start_args` runs before `start::run` either way).
    assert!(starter.started_supervised().is_empty());
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

/// The old session the `continues` tests name.
const OLD: &str = "s_aaaaaaaaaaaaaaaa";

/// A worktree directory with the old session's log recording it: the
/// `continues` fixture. Returns the worktree path the new `start` names
/// as its workspace.
fn continues_setup(temp: &Temp) -> String {
    let worktree = temp.dir.join("wt");
    fs::create_dir_all(&worktree).unwrap();
    let worktree = worktree.to_string_lossy().into_owned();
    crate::testkit::write_log_with_worktree(&temp.dir, OLD, &worktree, &worktree);
    worktree
}

/// Rejects the `start` naming `args` with `code`: every refusal leaves
/// the starter uncalled.
fn refused_start(temp: &Temp, args: Value, code: &str) {
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_1", "start", args));
    let (refused, echoed, _) = rejected(&client.next("the rejection"));
    assert_eq!(refused, code);
    assert_eq!(echoed.as_deref(), Some("c_1"));
    assert!(starter.received().is_empty());
    assert!(starter.started_worktrees().is_empty());
    assert!(starter.started_continues().is_empty());
}

#[test]
fn start_with_continues_and_worktree_true_is_invalid_arguments() {
    let temp = Temp::new();
    let worktree = continues_setup(&temp);
    refused_start(
        &temp,
        json!({"workspace": worktree, "worktree": true, "continues": OLD}),
        "invalid_arguments",
    );
}

#[test]
fn start_with_continues_naming_no_session_is_session_not_found() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    refused_start(
        &temp,
        json!({"workspace": workspace, "continues": OLD}),
        "session_not_found",
    );
}

#[test]
fn start_with_continues_and_a_gone_log_is_session_not_found() {
    let temp = Temp::new();
    // The session directory without its log: pruned away.
    fs::create_dir_all(temp.dir.join("projects/-w/sessions").join(OLD)).unwrap();
    let workspace = temp.workspace();
    refused_start(
        &temp,
        json!({"workspace": workspace, "continues": OLD}),
        "session_not_found",
    );
}

#[test]
fn start_with_continues_and_a_corrupt_first_line_is_log_corrupt() {
    for first in [
        "not json\n".to_owned(),
        json!({"kind": "fiber_started", "payload": {"workspace": "/w"}}).to_string() + "\n",
        json!({"kind": "session_started", "payload": {}}).to_string() + "\n",
        json!({"kind": "session_started", "payload": {"workspace": 7}}).to_string() + "\n",
        // No newline: the first line is still being written.
        "{\"kind\":\"session_started\"".to_owned(),
    ] {
        let temp = Temp::new();
        let dir = temp.dir.join("projects/-w/sessions").join(OLD);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), first.as_bytes()).unwrap();
        let workspace = temp.workspace();
        let code = if first.ends_with('\n') {
            "log_corrupt"
        } else {
            "session_not_found"
        };
        let starter = FakeStarter::bind_and_hold(&temp.dir);
        let hub = temp.hub(starter.clone());
        let mut client = Client::connect(&hub);
        client.hello();
        client.send(&command(
            "c_1",
            "start",
            json!({"workspace": workspace, "continues": OLD}),
        ));
        let (refused, _, _) = rejected(&client.next("the rejection"));
        assert_eq!(refused, code, "{first}");
        assert!(starter.started_continues().is_empty(), "{first}");
    }
}

#[test]
fn start_with_continues_and_an_unreadable_log_is_log_corrupt() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = Temp::new();
    let worktree = continues_setup(&temp);
    let log = temp
        .dir
        .join("projects/-w/sessions")
        .join(OLD)
        .join("events.jsonl");
    fs::set_permissions(&log, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&log).is_ok() {
        panic!("running as root: root bypasses file modes, so the read cannot fail here");
    }
    refused_start(
        &temp,
        json!({"workspace": worktree, "continues": OLD}),
        "log_corrupt",
    );
    fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn start_with_continues_and_a_non_object_worktree_is_log_corrupt() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let dir = temp.dir.join("projects/-w/sessions").join(OLD);
    fs::create_dir_all(&dir).unwrap();
    let first = json!({"kind": "session_started", "payload": {
        "workspace": workspace,
        "worktree": [],
    }})
    .to_string()
        + "\n";
    fs::write(dir.join("events.jsonl"), first).unwrap();
    refused_start(
        &temp,
        json!({"workspace": workspace, "continues": OLD}),
        "log_corrupt",
    );
}

#[test]
fn start_with_continues_and_a_null_worktree_is_log_corrupt() {
    // The payload rules forbid `null`: an optional key is absent, never
    // `null`, so a `null` worktree is foreign data and malformed.
    let temp = Temp::new();
    let workspace = temp.workspace();
    let dir = temp.dir.join("projects/-w/sessions").join(OLD);
    fs::create_dir_all(&dir).unwrap();
    let first = json!({"kind": "session_started", "payload": {
        "workspace": workspace,
        "worktree": Value::Null,
    }})
    .to_string()
        + "\n";
    fs::write(dir.join("events.jsonl"), first).unwrap();
    refused_start(
        &temp,
        json!({"workspace": workspace, "continues": OLD}),
        "log_corrupt",
    );
}

#[test]
fn start_with_continues_and_a_worktree_without_repository_is_log_corrupt() {
    let temp = Temp::new();
    let worktree = temp.dir.join("wt");
    fs::create_dir_all(&worktree).unwrap();
    let worktree = worktree.to_string_lossy().into_owned();
    let dir = temp.dir.join("projects/-w/sessions").join(OLD);
    fs::create_dir_all(&dir).unwrap();
    let first = json!({"kind": "session_started", "payload": {
        "workspace": worktree,
        "worktree": {"path": worktree, "branch": format!("fiber/{OLD}")},
    }})
    .to_string()
        + "\n";
    fs::write(dir.join("events.jsonl"), first).unwrap();
    refused_start(
        &temp,
        json!({"workspace": worktree, "continues": OLD}),
        "log_corrupt",
    );
}

#[test]
fn start_with_continues_and_a_relative_worktree_path_is_log_corrupt() {
    // Every recorded path is absolute, so a relative recorded path is
    // foreign data and malformed: the starter records nothing.
    let temp = Temp::new();
    let workspace = temp.workspace();
    let dir = temp.dir.join("projects/-w/sessions").join(OLD);
    fs::create_dir_all(&dir).unwrap();
    let first = json!({"kind": "session_started", "payload": {
        "workspace": workspace,
        "worktree": {"path": "relative", "branch": "b", "repository": "/r"},
    }})
    .to_string()
        + "\n";
    fs::write(dir.join("events.jsonl"), first).unwrap();
    refused_start(
        &temp,
        json!({"workspace": workspace, "continues": OLD}),
        "log_corrupt",
    );
}

#[test]
fn start_with_continues_naming_a_session_with_no_worktree_is_invalid_arguments() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    crate::testkit::write_log(&temp.dir, OLD, &workspace, None, &[]);
    refused_start(
        &temp,
        json!({"workspace": workspace, "continues": OLD}),
        "invalid_arguments",
    );
}

#[test]
fn start_with_continues_naming_another_worktree_is_invalid_arguments() {
    let temp = Temp::new();
    let worktree = continues_setup(&temp);
    let elsewhere = temp.dir.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let elsewhere = elsewhere.to_string_lossy().into_owned();
    crate::testkit::write_log_with_worktree(&temp.dir, OLD, &elsewhere, &elsewhere);
    refused_start(
        &temp,
        json!({"workspace": worktree, "continues": OLD}),
        "invalid_arguments",
    );
}

#[test]
fn start_with_continues_refuses_an_alias_of_the_same_directory() {
    for alias in ["slash", "link"] {
        let temp = Temp::new();
        let worktree = continues_setup(&temp);
        let workspace = match alias {
            "slash" => format!("{worktree}/"),
            _ => {
                let link = temp.dir.join("link");
                std::os::unix::fs::symlink(&worktree, &link).unwrap();
                link.to_string_lossy().into_owned()
            }
        };
        refused_start(
            &temp,
            json!({"workspace": workspace, "continues": OLD}),
            "invalid_arguments",
        );
    }
}

#[test]
fn start_with_continues_accepts_while_the_old_session_runs() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    let worktree = continues_setup(&temp);
    let _old = crate::fake::FakeSession::bind(&temp.dir, OLD);
    client.send(&command(
        "c_1",
        "start",
        json!({"workspace": worktree, "continues": OLD}),
    ));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_1");
    let session = result.get("session_id").unwrap().as_str().unwrap();
    assert!(session.starts_with("s_"));
    assert_ne!(session, OLD);
    assert_eq!(starter.started_worktrees(), [false]);
    assert_eq!(
        starter.started_continues(),
        [Some(contract::SessionId(OLD.to_owned()))]
    );
}

#[test]
fn start_with_continues_accepts_after_the_old_session_exited() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    // The old session's log only: its process is gone.
    let worktree = continues_setup(&temp);
    client.send(&command(
        "c_1",
        "start",
        json!({"workspace": worktree, "continues": OLD}),
    ));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_1");
    assert!(result.get("session_id").is_some());
    assert_eq!(starter.started_worktrees(), [false]);
    assert_eq!(
        starter.started_continues(),
        [Some(contract::SessionId(OLD.to_owned()))]
    );
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
        continues: Option<&SessionId>,
        supervised: bool,
    ) -> std::io::Result<Box<dyn Started>> {
        lock(&self.seen).push(overrides.iter().map(|text| text.to_string()).collect());
        self.inner
            .start(id, workspace, model, &[], worktree, continues, supervised)
    }

    fn resume(
        &self,
        id: &SessionId,
        workspace: &Path,
        credential: Option<&str>,
    ) -> std::io::Result<Box<dyn Started>> {
        self.inner.resume(id, workspace, credential)
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
    crate::testkit::hub(&temp.dir, Arc::new(FakeStarter::hang(&temp.dir)), timed)
}

#[test]
fn the_idle_timer_starts_when_the_open_count_reaches_zero() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let hub = hub_on(&temp, &clock);
    let (a, _a) = UnixStream::pair().unwrap();
    let (b, _b) = UnixStream::pair().unwrap();
    let first = match hub.poll_accept(&a, &AtomicBool::new(false)) {
        Accept::Counted(n) => n,
        Accept::Dropped => panic!("the test connection clone failed"),
        Accept::Exiting => panic!("fresh test hub unexpectedly exiting"),
    };
    let second = match hub.poll_accept(&b, &AtomicBool::new(false)) {
        Accept::Counted(n) => n,
        Accept::Dropped => panic!("the test connection clone failed"),
        Accept::Exiting => panic!("fresh test hub unexpectedly exiting"),
    };
    assert_eq!(hub.clients(), 2);
    disconnect(&hub, first);
    assert_eq!(hub.clients(), 1, "one client is still open");
    disconnect(&hub, second);
    assert_eq!(hub.clients(), 0);
}

#[test]
fn a_failed_clone_counts_nothing_and_serves_nothing() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let hub = hub_on(&temp, &clock);
    let failed: std::io::Result<UnixStream> = Err(std::io::Error::other("no fd"));
    let mut conns = lock(&hub.conns);
    assert!(
        hub.register_with(|| failed.map(Closer::Local), &mut conns)
            .is_none()
    );
    drop(conns);
    assert_eq!(hub.clients(), 0);
}

#[test]
fn rollback_removes_the_count_and_restarts_the_timer_at_zero() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let hub = hub_on(&temp, &clock);
    let (a, _a) = UnixStream::pair().unwrap();
    let (b, _b) = UnixStream::pair().unwrap();
    let first = match hub.poll_accept(&a, &AtomicBool::new(false)) {
        Accept::Counted(n) => n,
        Accept::Dropped => panic!("the test connection clone failed"),
        Accept::Exiting => panic!("fresh test hub unexpectedly exiting"),
    };
    let second = match hub.poll_accept(&b, &AtomicBool::new(false)) {
        Accept::Counted(n) => n,
        Accept::Dropped => panic!("the test connection clone failed"),
        Accept::Exiting => panic!("fresh test hub unexpectedly exiting"),
    };
    assert_eq!(hub.clients(), 2);
    hub.rollback(first);
    assert_eq!(hub.clients(), 1, "one client is still open");
    hub.rollback(second);
    assert_eq!(hub.clients(), 0);
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
    fs::create_dir_all(log::session_dir(&temp.dir, "p", &SessionId(id.to_owned()))).unwrap();
    let row: crate::RecentRow = serde_json::from_value(json!({
        "session_id": id, "ts": 3, "project": "p", "workspace": "/w", "name": "n",
        "how": "crashed", "status": crate::fake::status("n", "/w", "idle", None),
    }))
    .unwrap();
    crate::append(&temp.dir, &row).unwrap();
    hub.feed.start();
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
    }
    drop(client);
    until_clients(&hub, 0, "after the client left");
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
    stop_within(&hub.feed);
}

#[test]
fn sessions_default_listing_omits_active() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    // A live session whose log ran on, and an exited row with a readable
    // log: neither answer names any activity without `active: true`.
    let live = "s_00000000000000a1";
    let session = crate::fake::FakeSession::bind(&temp.dir, live);
    session.say(&crate::fake::status_line(
        live,
        &crate::fake::status("n", "/w", "idle", None),
    ));
    write_log(
        &temp,
        "-w",
        live,
        &format!(
            "{}{}",
            started_line(live, 100),
            log_line("session_status", live, 9000)
        ),
    );
    let exited = "s_00000000000000b2";
    exited_row(&temp, exited, "p", 3, None);
    write_log(
        &temp,
        "p",
        exited,
        &format!(
            "{}{}",
            started_line(exited, 50),
            log_line("fiber_exited", exited, 7000)
        ),
    );
    hub.feed.start();
    assert!(session.await_subscribed(1, DEADLINE));
    let mut client = Client::connect(&hub);
    client.hello();
    // Missing `active` and `active: false` both list without activity.
    for (id, args) in [("c_1", json!({})), ("c_2", json!({"active": false}))] {
        client.send(&command(id, "sessions", args));
        let (echoed, result) = accepted(&client.next("the listing"));
        assert_eq!(echoed, id);
        for entries in ["live", "exited"] {
            let rows = result.get(entries).unwrap().as_array().unwrap();
            assert_eq!(rows.len(), 1, "{entries}");
            assert_eq!(rows[0].get("active"), None, "{entries} {id}");
        }
    }
    stop_within(&hub.feed);
}

#[test]
fn sessions_rejects_a_misshaped_active_arg() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    // Two live sessions in distinct projects: the `active: true` project
    // filter below must keep the match and drop the other, so inverting
    // the `status.project == key` check in the active path fails.
    let kept = "s_00000000000000a1";
    let kept_session = crate::fake::FakeSession::bind(&temp.dir, kept);
    let mut kept_status = crate::fake::status("n", "/w", "idle", None);
    kept_status["project"] = json!("p");
    kept_session.say(&crate::fake::status_line(kept, &kept_status));
    write_log(
        &temp,
        "p",
        kept,
        &format!(
            "{}{}",
            started_line(kept, 100),
            log_line("session_status", kept, 9000)
        ),
    );
    let dropped = "s_00000000000000b2";
    let dropped_session = crate::fake::FakeSession::bind(&temp.dir, dropped);
    let mut dropped_status = crate::fake::status("n", "/w", "idle", None);
    dropped_status["project"] = json!("q");
    dropped_session.say(&crate::fake::status_line(dropped, &dropped_status));
    write_log(
        &temp,
        "q",
        dropped,
        &format!(
            "{}{}",
            started_line(dropped, 100),
            log_line("session_status", dropped, 8000)
        ),
    );
    let mut client = Client::connect(&hub);
    client.hello();
    hub.feed.start();
    assert!(kept_session.await_subscribed(1, DEADLINE));
    assert!(dropped_session.await_subscribed(1, DEADLINE));
    for (id, args) in [
        ("c_1", json!({"active": "yes"})),
        ("c_2", json!({"active": Value::Null})),
        ("c_3", json!({"active": 1})),
        ("c_4", json!({"project": "p", "active": "yes"})),
        ("c_5", json!({"active": true, "extra": 1})),
    ] {
        client.send(&command(id, "sessions", args));
        let (code, echoed, _) = rejected(&client.next("the rejection"));
        assert_eq!(code, "invalid_arguments", "{id}");
        assert_eq!(echoed.as_deref(), Some(id));
    }
    // A project filter composes with `active: true`: only the live entry
    // in `p` lists, with its activity; the `q` entry is excluded.
    client.send(&command(
        "c_6",
        "sessions",
        json!({"project": "p", "active": true}),
    ));
    let (echoed, result) = accepted(&client.next("the filtered listing"));
    assert_eq!(echoed, "c_6");
    let live = result.get("live").unwrap().as_array().unwrap();
    assert_eq!(live.len(), 1, "only the matching live entry lists");
    assert_eq!(live[0].get("session_id"), Some(&json!(kept)));
    assert_eq!(live[0].get("active"), Some(&json!(9000)));
    assert!(
        !live
            .iter()
            .any(|row| row.get("session_id") == Some(&json!(dropped))),
        "the other project's live entry is excluded"
    );
    assert_eq!(result.get("exited"), Some(&json!([])));
    stop_within(&hub.feed);
}

/// A `session_started` first line for `id` at `ts`, as a session writes it.
fn started_line(id: &str, ts: u64) -> String {
    let line = json!({
        "kind": "session_started", "seq": 0, "session_id": id,
        "ts": ts, "schema_version": 1,
        "payload": {"workspace": "/w",
            "variables": {"path": "", "names": [], "source": "login_shell"}},
    });
    format!("{line}\n")
}

/// A complete log line of `kind` for `id` at `ts`.
fn log_line(kind: &str, id: &str, ts: u64) -> String {
    let line = json!({
        "kind": kind, "session_id": id, "ts": ts, "schema_version": 1,
        "payload": {},
    });
    format!("{line}\n")
}

/// Writes `events.jsonl` for session `id` in `project`.
fn write_log(temp: &Temp, project: &str, id: &str, text: &str) {
    let dir = log::session_dir(&temp.dir, project, &SessionId(id.to_owned()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), text).unwrap();
}

/// An exited recent row for `id` in `project` with row `ts`, delegate of
/// `parent` when given.
fn exited_row(temp: &Temp, id: &str, project: &str, ts: u64, parent: Option<&str>) {
    let row: crate::RecentRow = serde_json::from_value(json!({
        "session_id": id, "ts": ts, "project": project, "workspace": "/w",
        "name": "n", "how": "exited",
        "status": crate::fake::status("n", "/w", "idle", parent),
    }))
    .unwrap();
    crate::append(&temp.dir, &row).unwrap();
}

#[test]
fn sessions_entries_carry_the_last_line_ts_never_since_or_row_ts() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    // A live session streaming for an hour: its status says `since` 1, its
    // log's last line is far newer.
    let live = "s_00000000000000a1";
    let session = crate::fake::FakeSession::bind(&temp.dir, live);
    session.say(&crate::fake::status_line(
        live,
        &crate::fake::status("n", "/w", "idle", None),
    ));
    write_log(
        &temp,
        "-w",
        live,
        &format!(
            "{}{}",
            started_line(live, 100),
            log_line("session_status", live, 9000)
        ),
    );
    // An exited row from long ago whose log ran on.
    let exited = "s_00000000000000b2";
    exited_row(&temp, exited, "p", 3, None);
    write_log(
        &temp,
        "p",
        exited,
        &format!(
            "{}{}",
            started_line(exited, 50),
            log_line("fiber_exited", exited, 7000)
        ),
    );
    hub.feed.start();
    assert!(session.await_subscribed(1, DEADLINE));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&json!({"id": "c_1", "command": "sessions", "args": {"active": true}}));
    let (echoed, result) = accepted(&client.next("the listing"));
    assert_eq!(echoed, "c_1");
    let live_entries = result.get("live").unwrap().as_array().unwrap();
    assert_eq!(live_entries.len(), 1);
    assert_eq!(live_entries[0].get("session_id"), Some(&json!(live)));
    assert_eq!(live_entries[0].get("active"), Some(&json!(9000)));
    let exited_entries = result.get("exited").unwrap().as_array().unwrap();
    assert_eq!(exited_entries.len(), 1);
    assert_eq!(exited_entries[0].get("session_id"), Some(&json!(exited)));
    assert_eq!(exited_entries[0].get("active"), Some(&json!(7000)));
    stop_within(&hub.feed);
}

#[test]
fn sessions_torn_tail_uses_the_last_complete_line() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    // The first line's `ts` differs from the last complete line's, and a
    // torn write follows it: the torn bytes are not activity.
    let id = "s_00000000000000c3";
    exited_row(&temp, id, "p", 3, None);
    let mut text = started_line(id, 100);
    text.push_str(&log_line("session_status", id, 200));
    text.push_str("{\"kind\": \"sess");
    write_log(&temp, "p", id, &text);
    hub.feed.start();
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&json!({"id": "c_1", "command": "sessions", "args": {"active": true}}));
    let (_, result) = accepted(&client.next("the listing"));
    let exited_entries = result.get("exited").unwrap().as_array().unwrap();
    assert_eq!(exited_entries.len(), 1);
    assert_eq!(exited_entries[0].get("active"), Some(&json!(200)));
    stop_within(&hub.feed);
}

#[test]
fn sessions_ts_less_tail_falls_back_to_the_opening_ts() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    // The tail is complete JSON but carries no `ts`: only the opening
    // line dates the session.
    let id = "s_00000000000000d4";
    exited_row(&temp, id, "p", 3, None);
    let tail = format!(
        "{}\n",
        json!({"kind": "session_status", "session_id": id, "schema_version": 1, "payload": {}})
    );
    write_log(
        &temp,
        "p",
        id,
        &format!("{}{}", started_line(id, 100), tail),
    );
    hub.feed.start();
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&json!({"id": "c_1", "command": "sessions", "args": {"active": true}}));
    let (_, result) = accepted(&client.next("the listing"));
    let exited_entries = result.get("exited").unwrap().as_array().unwrap();
    assert_eq!(exited_entries.len(), 1);
    assert_eq!(exited_entries[0].get("active"), Some(&json!(100)));
    stop_within(&hub.feed);
}

#[test]
fn sessions_row_ts_never_decides_active_either_direction() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    // An ancient row whose log ran on, and a fresh row whose log is older.
    let old_row = "s_00000000000000e5";
    exited_row(&temp, old_row, "p", 1, None);
    write_log(
        &temp,
        "p",
        old_row,
        &format!(
            "{}{}",
            started_line(old_row, 10),
            log_line("fiber_exited", old_row, 9000)
        ),
    );
    let new_row = "s_00000000000000f6";
    exited_row(&temp, new_row, "p", 99999, None);
    write_log(
        &temp,
        "p",
        new_row,
        &format!(
            "{}{}",
            started_line(new_row, 10),
            log_line("fiber_exited", new_row, 42)
        ),
    );
    hub.feed.start();
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&json!({"id": "c_1", "command": "sessions", "args": {"active": true}}));
    let (_, result) = accepted(&client.next("the listing"));
    let exited_entries = result.get("exited").unwrap().as_array().unwrap();
    assert_eq!(exited_entries.len(), 2);
    let active = |id: &str| {
        exited_entries
            .iter()
            .find(|row| row.get("session_id") == Some(&json!(id)))
            .unwrap_or_else(|| panic!("{id} is listed"))
            .get("active")
            .cloned()
    };
    assert_eq!(active(old_row), Some(json!(9000)));
    assert_eq!(active(new_row), Some(json!(42)));
    stop_within(&hub.feed);
}

#[test]
fn sessions_empty_log_lists_with_null_active_and_skips_delegates() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    // A directory with an empty log stays listed, with no activity.
    let empty = "s_0000000000000007";
    exited_row(&temp, empty, "p", 3, None);
    write_log(&temp, "p", empty, "");
    // A delegate row never lists, even with a readable log.
    let delegate = "s_0000000000000008";
    exited_row(&temp, delegate, "p", 4, Some("s_0000000000000009"));
    write_log(
        &temp,
        "p",
        delegate,
        &format!(
            "{}{}",
            started_line(delegate, 10),
            log_line("fiber_exited", delegate, 5000)
        ),
    );
    hub.feed.start();
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&json!({"id": "c_1", "command": "sessions", "args": {"active": true}}));
    let (_, result) = accepted(&client.next("the listing"));
    let exited_entries = result.get("exited").unwrap().as_array().unwrap();
    assert_eq!(exited_entries.len(), 1);
    assert_eq!(exited_entries[0].get("session_id"), Some(&json!(empty)));
    assert_eq!(exited_entries[0].get("active"), Some(&json!(Value::Null)));
    stop_within(&hub.feed);
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
    drop(
        hub.feed
            .attention
            .notify("s_00000000000000ff", None, &waiting_status("rh"), false),
    );
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
    stop_within(&hub.feed);
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
        drop(
            hub.feed
                .attention
                .notify("s_00000000000000ff", None, &status, false),
        );
    }
    d.write.shutdown(Shutdown::Write).unwrap();
    until_listeners(&hub, 0, "after D half-closed");
    until_clients(&hub, 0, "after D half-closed");
    stop_within(&hub.feed);
}

/// A served connection plus a test-owned attention subscriber on a clone
/// of its socket: the subscriber is what overflows, while the
/// connection's teardown races the bound report.
struct OverflowRace {
    _temp: Temp,
    hub: Arc<Hub>,
    client: UnixStream,
    read: BufReader<UnixStream>,
    owned: Arc<Mutex<UnixStream>>,
    serve_rx: mpsc::Receiver<()>,
    paused_rx: mpsc::Receiver<()>,
    release_tx: mpsc::Sender<()>,
}

/// Serves one counted connection, drains its `hub_hello`, and overflows a
/// test-owned subscriber on a clone of its socket up to the bound report:
/// a 4 MiB line the writer blocks on, never read by the peer. Returns the
/// race with the writer proven blocked and the teardown pause armed.
fn overflow_race() -> OverflowRace {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let (server, client) = UnixStream::pair().unwrap();
    client.set_read_timeout(Some(DEADLINE)).unwrap();
    let sub_sock = server.try_clone().unwrap();
    let closer_sock = server.try_clone().unwrap();
    let retired_sock = server.try_clone().unwrap();
    let n = match hub.poll_accept(&server, &AtomicBool::new(false)) {
        Accept::Counted(n) => n,
        Accept::Dropped => panic!("the hub counts the connection"),
        Accept::Exiting => panic!("fresh test hub unexpectedly exiting"),
    };
    let serve_hub = Arc::clone(&hub);
    let (serve_tx, serve_rx) = mpsc::channel();
    thread::spawn(move || {
        serve_counted(server, serve_hub, n);
        serve_tx.send(()).unwrap_or(());
    });
    let mut read = BufReader::new(client.try_clone().unwrap());
    let mut hello = String::new();
    read.read_line(&mut hello).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&hello).unwrap()["kind"],
        "hub_hello"
    );
    until_listeners(&hub, 1, "the connection registered");
    let owned = Arc::new(Mutex::new(sub_sock));
    let closer = crate::feed::Closer {
        stream: closer_sock,
        web: None,
    };
    let retired = crate::feed::Closer {
        stream: retired_sock,
        web: None,
    };
    hub.feed
        .attention
        .listen(Arc::clone(&owned), closer, retired)
        .unwrap();
    until_listeners(&hub, 2, "the test subscriber registered");
    // One line larger than any socket buffer, never read: the writer
    // blocks in its write path, which the held mutex proves.
    let big = waiting_status(&format!("r-big-{}", "x".repeat(4 * 1024 * 1024)));
    drop(
        hub.feed
            .attention
            .notify("s_0000000000000bb0", None, &big, false),
    );
    let blocked = Arc::clone(&owned);
    await_true("the writer to block in its write", move || {
        blocked.try_lock().is_err()
    });
    let (paused_tx, paused_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    *lock(&hub.before_disconnect_cleanup) = Some(Box::new(move || {
        paused_tx.send(()).unwrap_or(());
        release_rx.recv().unwrap_or(());
    }));
    OverflowRace {
        _temp: temp,
        hub,
        client,
        read,
        owned,
        serve_rx,
        paused_rx,
        release_tx,
    }
}

/// QUEUE+1 small lines after the big one: the bound fires for every
/// stalled listener, and the shutdown runs outside every lock. The loop
/// allows for the dequeued empty snapshot and stops at the report.
fn overflow_full(race: &OverflowRace) {
    let mut closers = Vec::new();
    for n in 0..crate::feed::QUEUE + 10 {
        if !closers.is_empty() {
            break;
        }
        let session = format!("s_{n:016x}");
        let status = waiting_status(&format!("r{n}"));
        closers.extend(
            race.hub
                .feed
                .attention
                .notify(&session, None, &status, false),
        );
    }
    assert!(!closers.is_empty(), "the bound fired");
    crate::feed::close_connections(closers, UnixStream::shutdown);
    until_listeners(&race.hub, 0, "the overflow unregistered every listener");
}

/// The race converged: the read loop returned, the count released, EOF
/// drained, and the writer ended and joined.
fn overflow_joined(race: OverflowRace, what: &str) {
    let OverflowRace {
        _temp: _,
        hub,
        client,
        mut read,
        owned,
        serve_rx,
        paused_rx: _,
        release_tx: _,
    } = race;
    Deadline::after(DEADLINE)
        .recv(&serve_rx)
        .unwrap_or_else(|_| panic!("the read loop returns {what}"));
    until_clients(&hub, 0, "the count released");
    // Whatever partial writes landed drains first; then EOF.
    let mut buf = [0; 8];
    loop {
        match read.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => panic!("the client drains to EOF {what}: {error}"),
        }
    }
    let _ = client;
    stop_within(&hub.feed);
    assert_eq!(
        Arc::strong_count(&owned),
        1,
        "the writer ended and joined {what}"
    );
}

#[test]
fn overflow_while_teardown_is_held_still_closes_and_releases() {
    let race = overflow_race();
    // The client's half-close ends the read loop; teardown pauses after
    // EOF but before its own write shutdown.
    race.client.shutdown(Shutdown::Write).unwrap();
    Deadline::after(DEADLINE)
        .recv(&race.paused_rx)
        .expect("teardown to pause after EOF");
    // The overflow runs while teardown is held, then teardown resumes.
    overflow_full(&race);
    race.release_tx.send(()).unwrap_or(());
    overflow_joined(race, "after held teardown");
}

#[test]
fn overflow_first_still_closes_and_releases() {
    let race = overflow_race();
    // The overflow's own read shutdown ends the connection reader, so
    // teardown pauses; only then does the client half-close, whose errors
    // after the disconnect are allowed.
    overflow_full(&race);
    Deadline::after(DEADLINE)
        .recv(&race.paused_rx)
        .expect("teardown to pause after the overflow");
    race.client.shutdown(Shutdown::Write).unwrap_or(());
    race.release_tx.send(()).unwrap_or(());
    overflow_joined(race, "after the overflow first");
}

#[test]
fn a_relay_only_connection_starts_no_attention_writer() {
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
    let _session = bind_session(&temp.dir, sid, ack.clone());
    let mut client = Client::connect(&hub);
    client.hello();
    until_listeners(&hub, 1, "the connection registered");
    assert_eq!(
        hub.feed.attention.writers_started(),
        0,
        "hello starts no writer"
    );
    // Relay only: a session command is acknowledged and verbatim, with no
    // feed and no unsolicited line.
    client.send(&json!({
        "id": "c_4",
        "session_id": sid,
        "command": "subscribe",
        "args": {"level": "full"},
    }));
    assert_eq!(client.next("the relayed acknowledgement"), ack);
    assert_eq!(hub.feed.attention.listeners(), 1, "still registered");
    assert_eq!(
        hub.feed.attention.writers_started(),
        0,
        "relay-only traffic starts no writer"
    );
    stop_within(&hub.feed);
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
    let hub = Arc::new(crate::testkit::hub(
        &temp.dir,
        Arc::new(starter.clone()),
        timed,
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
    let mark = clock.advance_when_parked(Some(due), None, by, &fakes::Deadline::after(DEADLINE));
    assert!(
        clock.await_parked_since(&mark, Some(due), fakes::Deadline::after(DEADLINE)),
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
    assert!(
        clock.await_parked(due, fakes::Deadline::after(DEADLINE)),
        "the bound is armed"
    );
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
    assert!(clock.await_parked(due, fakes::Deadline::after(DEADLINE)));
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
    still_waiting(&clock, due, Duration::ZERO);
    assert_eq!(prompts(&starter), 0);
    still_waiting(&clock, due, Duration::from_millis(999));
    assert_eq!(prompts(&starter), 0, "one millisecond before the bound");
    clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_millis(1),
        &fakes::Deadline::after(DEADLINE),
    );
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
    still_waiting(&clock, due, Duration::ZERO);
    assert_eq!(prompts(&starter), 0);
    clock.advance_when_parked(
        Some(due),
        None,
        FIRST_PROMPT_WAIT,
        &fakes::Deadline::after(DEADLINE),
    );
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
    assert!(
        clock.await_parked(due, fakes::Deadline::after(DEADLINE)),
        "the bound is armed"
    );
    clock.advance_when_parked(
        Some(due),
        None,
        FIRST_PROMPT_WAIT,
        &fakes::Deadline::after(DEADLINE),
    );
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
    assert!(
        clock.await_parked(due, fakes::Deadline::after(DEADLINE)),
        "the first prompt waits at its bound"
    );
    clock.advance_when_parked(
        Some(due),
        None,
        FIRST_PROMPT_WAIT,
        &fakes::Deadline::after(DEADLINE),
    );
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

#[test]
fn status_carries_the_published_environments_variables() {
    let temp = Temp::new();
    let environments = crate::Environments::ready(crate::Environment {
        vars: vec![
            (
                std::ffi::OsString::from("PATH"),
                std::ffi::OsString::from("/usr/bin:/bin"),
            ),
            (
                std::ffi::OsString::from("HOME"),
                std::ffi::OsString::from("/home/u"),
            ),
        ],
        source: contract::events::VariablesSource::LoginShell,
    });
    let expected = environments.get().variables();
    let timed: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let hub = Arc::new(
        crate::testkit::hub(&temp.dir, Arc::new(FakeStarter::hang(&temp.dir)), timed)
            .with_environments(environments),
    );
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_1", "status", json!({})));
    let (id, result) = accepted(&client.next("the acknowledgement"));
    assert_eq!(id, "c_1");
    assert_eq!(
        result["variables"],
        serde_json::to_value(&expected).unwrap()
    );
}

#[test]
fn status_waits_for_the_first_publish() {
    let temp = Temp::new();
    let pending = crate::Environments::new(Vec::new(), &temp.dir);
    let timed: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let hub = Arc::new(
        crate::testkit::hub(
            &temp.dir,
            Arc::new(FakeStarter::hang(&temp.dir)),
            Arc::clone(&timed),
        )
        .with_environments(Arc::clone(&pending)),
    );
    // Signal the `status` wait itself: nothing is published yet, so the
    // admission proves the handler parks in `get` rather than passing
    // because the command never arrived.
    let (waiting_tx, waiting_rx) = mpsc::channel();
    *pending.before_get_wait.lock().unwrap() = Some(Box::new(move || {
        waiting_tx.send(()).unwrap_or(());
    }));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_1", "status", json!({})));
    let (id, result) = thread::scope(|scope| {
        let (ack_tx, ack_rx) = mpsc::channel();
        scope.spawn(move || {
            let line = client.next("the acknowledgement");
            ack_tx.send(line).unwrap_or(());
        });
        Deadline::after(DEADLINE)
            .recv(&waiting_rx)
            .expect("status waits for the first publish");
        assert!(
            matches!(ack_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "the pending status answers nothing yet"
        );
        let clock: Arc<fakes::clock::FakeClock> = fakes::clock::FakeClock::new();
        let timed: Arc<dyn Clock> = clock.clone();
        let pending = Arc::clone(&pending);
        let diag = crate::diag::open(&temp.dir, timed);
        let (first_tx, first_rx) = mpsc::channel();
        scope.spawn(move || {
            pending.first(&*clock, &diag);
            first_tx.send(()).unwrap_or(());
        });
        Deadline::after(DEADLINE)
            .recv(&first_rx)
            .expect("first publishes");
        let line = Deadline::after(DEADLINE)
            .recv(&ack_rx)
            .expect("the acknowledgement");
        accepted(&line)
    });
    assert_eq!(id, "c_1");
    assert_eq!(result["variables"]["source"], json!("inherited"));
}

#[test]
fn refresh_without_args_is_accepted_with_no_result_and_publishes() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let before = hub.environments.get();
    let mut client = Client::connect(&hub);
    client.hello();
    client.send_raw("{\"id\":\"c_r\",\"command\":\"refresh\"}\n");
    let line = client.next("the acknowledgement");
    let (id, result) = accepted(&line);
    assert_eq!(id, "c_r");
    assert_eq!(result, Value::Null);
    assert!(line.get("payload").unwrap().get("result").is_none());
    let after = hub.environments.get();
    assert!(!Arc::ptr_eq(&before, &after), "refresh publishes anew");
}

#[test]
fn refresh_with_empty_args_is_accepted_with_no_result() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_r", "refresh", json!({})));
    let line = client.next("the acknowledgement");
    let (id, result) = accepted(&line);
    assert_eq!(id, "c_r");
    assert_eq!(result, Value::Null);
    assert!(line.get("payload").unwrap().get("result").is_none());
}

#[test]
fn refresh_with_an_argument_is_invalid_arguments() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let mut client = Client::connect(&hub);
    client.hello();
    client.send(&command("c_r", "refresh", json!({"x": 1})));
    let (code, id, message) = rejected(&client.next("the rejection"));
    assert_eq!(code, "invalid_arguments");
    assert_eq!(id.as_deref(), Some("c_r"));
    assert_eq!(message, "The arguments do not fit this command.");
}

/// A scripted process tree for peer attribution: parents, owners and
/// argument vectors by pid, with unreadable pids and argument vectors
/// that panic when read.
struct PeerScript {
    current: support::peer::EffectiveUid,
    parents: std::collections::HashMap<support::peer::Pid, support::peer::Pid>,
    euids: std::collections::HashMap<support::peer::Pid, support::peer::EffectiveUid>,
    argvs: std::collections::HashMap<support::peer::Pid, Vec<Vec<u8>>>,
    unreadable_parent: Vec<support::peer::Pid>,
    unreadable_euid: Vec<support::peer::Pid>,
    unreadable_argv: Vec<support::peer::Pid>,
    argv_read: std::cell::Cell<usize>,
}

impl support::peer::ProcessRead for PeerScript {
    fn current_euid(&self) -> Result<support::peer::EffectiveUid, support::peer::ReadError> {
        Ok(self.current)
    }

    fn euid(
        &self,
        pid: support::peer::Pid,
    ) -> Result<support::peer::EffectiveUid, support::peer::ReadError> {
        if self.unreadable_euid.contains(&pid) {
            return Err(support::peer::ReadError::Unreadable { pid, what: "euid" });
        }
        self.euids
            .get(&pid)
            .copied()
            .ok_or(support::peer::ReadError::Unreadable { pid, what: "euid" })
    }

    fn parent(
        &self,
        pid: support::peer::Pid,
    ) -> Result<support::peer::Pid, support::peer::ReadError> {
        if self.unreadable_parent.contains(&pid) {
            return Err(support::peer::ReadError::Unreadable {
                pid,
                what: "parent",
            });
        }
        self.parents
            .get(&pid)
            .copied()
            .ok_or(support::peer::ReadError::Unreadable {
                pid,
                what: "parent",
            })
    }

    fn argv(&self, pid: support::peer::Pid) -> Result<Vec<Vec<u8>>, support::peer::ReadError> {
        self.argv_read.set(self.argv_read.get() + 1);
        if self.unreadable_argv.contains(&pid) {
            return Err(support::peer::ReadError::Unreadable { pid, what: "argv" });
        }
        self.argvs
            .get(&pid)
            .cloned()
            .ok_or(support::peer::ReadError::Unreadable { pid, what: "argv" })
    }
}

/// Tests for the hub side of the session-socket refusals: the caller
/// attribution, the three refusals, the direct-parent exception and the
/// incomplete-walk failure, on published relays and on the no-relay
/// first-command path.
mod session_refusal_tests {
    use std::collections::HashMap;
    use std::io::BufRead;
    use std::os::unix::net::UnixStream;

    use serde_json::{Value, json};

    use super::{Attribution, PeerScript, Temp, attribute_peer};
    use crate::fake::FakeStarter;
    use crate::relay::{Kept, Relay, Replayed};

    const PEER: support::peer::Pid = support::peer::Pid(555);
    const TARGET: support::peer::Pid = support::peer::Pid(777);
    const UNREADABLE: support::peer::Pid = support::peer::Pid(999);
    const CALLER_HOST: support::peer::Pid = support::peer::Pid(444);
    const SHARED_HOST: support::peer::Pid = support::peer::Pid(446);
    const OWNED: &str = "s_0123456789abcdef";
    const OTHER: &str = "s_fedcba9876543210";

    /// A session-hosting argument vector with no session id: the argv-only
    /// predicate recognizes the executable and subcommand alone.
    fn host_argv() -> Vec<Vec<u8>> {
        ["/path/to/fiber", "session"]
            .iter()
            .map(|word| word.as_bytes().to_vec())
            .collect()
    }

    /// The exact refusal sentences (`docs/permissions.md`, "Sessions on the
    /// sockets"): no caller session id is ever named.
    fn approval_refusal(target: &str) -> String {
        format!(
            "Only a person answers an approval. This connection comes from inside a session, so `reply` to session `{target}` is refused."
        )
    }
    const PAIRING_REFUSAL: &str =
        "A connection from inside a session cannot ask for a pairing code.";

    fn session_command_refusal(command: &str, target: &str) -> String {
        format!("A connection from inside a session cannot send `{command}` to session `{target}`.")
    }

    /// Scripted kernel reads: the hub's test seam answers from these
    /// tables, and a missing entry reads as unreadable.
    #[derive(Default)]
    struct Script {
        current: support::peer::EffectiveUid,
        parents: HashMap<support::peer::Pid, support::peer::Pid>,
        parent_changes: HashMap<support::peer::Pid, (support::peer::Pid, support::peer::Pid)>,
        parent_reads: std::sync::Arc<std::sync::Mutex<HashMap<support::peer::Pid, usize>>>,
        euids: HashMap<support::peer::Pid, support::peer::EffectiveUid>,
        argvs: HashMap<support::peer::Pid, Vec<Vec<u8>>>,
        /// Pause point: the first `parent` read signals `entered`, then
        /// waits for `release` (or the deadline) before answering from
        /// `parents`. Lets a test replace the relay entry mid-walk. The
        /// receiver sits behind a lock so the script stays shareable.
        block_first_parent: Option<(
            std::sync::mpsc::Sender<()>,
            std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        )>,
        blocked: std::sync::atomic::AtomicBool,
    }

    impl support::peer::ProcessRead for Script {
        fn current_euid(&self) -> Result<support::peer::EffectiveUid, support::peer::ReadError> {
            Ok(self.current)
        }

        fn euid(
            &self,
            pid: support::peer::Pid,
        ) -> Result<support::peer::EffectiveUid, support::peer::ReadError> {
            self.euids
                .get(&pid)
                .copied()
                .ok_or(support::peer::ReadError::Unreadable { pid, what: "euid" })
        }

        fn parent(
            &self,
            pid: support::peer::Pid,
        ) -> Result<support::peer::Pid, support::peer::ReadError> {
            if let Some((entered, release)) = &self.block_first_parent
                && !self.blocked.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                entered.send(()).unwrap_or(());
                if let Ok(release) = release.lock() {
                    fakes::Deadline::after(super::DEADLINE)
                        .recv(&release)
                        .unwrap_or(());
                }
            }
            let read_number = if let Ok(mut reads) = self.parent_reads.lock() {
                let number = reads.entry(pid).or_default();
                let current = *number;
                *number += 1;
                current
            } else {
                0
            };
            if let Some((first, later)) = self.parent_changes.get(&pid) {
                return Ok(if read_number == 0 { *first } else { *later });
            }
            self.parents
                .get(&pid)
                .copied()
                .ok_or(support::peer::ReadError::Unreadable {
                    pid,
                    what: "parent",
                })
        }

        fn argv(&self, pid: support::peer::Pid) -> Result<Vec<Vec<u8>>, support::peer::ReadError> {
            self.argvs
                .get(&pid)
                .cloned()
                .ok_or(support::peer::ReadError::Unreadable { pid, what: "argv" })
        }
    }

    struct Rig {
        hub: std::sync::Arc<super::Hub>,
        writer: std::sync::Arc<std::sync::Mutex<UnixStream>>,
        read: std::io::BufReader<UnixStream>,
        relays: std::sync::Arc<std::sync::Mutex<crate::relay::Relays>>,
        _temp: Temp,
    }

    impl Rig {
        fn new(script: Script) -> Self {
            let temp = Temp::new();
            let hub = temp.hub(FakeStarter::hang(&temp.dir));
            *crate::connection::lock(&hub.test_read) = Some(std::sync::Arc::new(script));
            let (write, read) = UnixStream::pair().unwrap();
            read.set_read_timeout(Some(super::DEADLINE)).unwrap();
            Self {
                hub,
                writer: std::sync::Arc::new(std::sync::Mutex::new(write)),
                read: std::io::BufReader::new(read),
                relays: std::sync::Arc::default(),
                _temp: temp,
            }
        }

        fn push_entry(
            &self,
            session: &str,
            target: crate::relay::RelayTarget,
        ) -> (UnixStream, std::sync::mpsc::Sender<()>) {
            let (writer, peer) = UnixStream::pair().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let thread = std::thread::spawn(move || {
                rx.recv().unwrap_err();
            });
            crate::connection::lock(&self.relays).entries.push(Relay {
                session: session.to_owned(),
                epoch: 1,
                writer,
                kept: Kept::default(),
                replayed: Replayed::default(),
                thread: Some(thread),
                retiring: None,
                target,
            });
            peer.set_read_timeout(Some(super::DEADLINE)).unwrap();
            (peer, tx)
        }

        fn relay(&mut self, id: &str, session: &str, command: Value, attribution: &Attribution) {
            crate::relay::relay_command(
                &contract::CommandId(id.to_owned()),
                session,
                &command,
                &self.hub,
                &self.writer,
                &self.relays,
                attribution,
            );
        }

        fn next_client_line(&mut self) -> Value {
            let mut text = String::new();
            self.read.read_line(&mut text).unwrap();
            serde_json::from_str(text.trim_end()).unwrap()
        }
    }

    fn reply_with(id: &str, args: Value) -> Value {
        json!({"command": "reply", "args": args, "id": id})
    }

    fn steer(id: &str) -> Value {
        json!({"command": "steer", "args": {"content": [{"type": "text", "text": "hi"}]}, "id": id})
    }

    fn prompt(id: &str) -> Value {
        json!({"command": "prompt", "args": {"content": [{"type": "text", "text": "hi"}]}, "id": id})
    }

    fn message(id: &str) -> Value {
        json!({"command": "message", "args": {"text": "hi"}, "id": id})
    }

    fn full_subscribe(id: &str) -> Value {
        json!({"command": "subscribe", "args": {"level": "full"}, "id": id})
    }

    fn approval_args(request: &str) -> Value {
        json!({"request_id": request, "decision": "allow"})
    }

    fn labels_args(request: &str) -> Value {
        json!({"request_id": request, "labels": ["a"]})
    }

    fn session_caller(pids: &[support::peer::Pid]) -> Attribution {
        Attribution::Local {
            peer_pid: PEER,
            session_process_pids: pids.to_vec(),
        }
    }

    fn incomplete() -> Attribution {
        Attribution::Incomplete {
            peer_pid: Some(PEER),
            pid: Some(UNREADABLE),
            peer_error: None,
        }
    }

    /// The target's walk: its parent and, through PID 1, no session.
    fn plain_target() -> Script {
        Script {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (TARGET, support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [
                (TARGET, support::peer::EffectiveUid(1000)),
                (support::peer::Pid(1), support::peer::EffectiveUid(1000)),
            ]
            .into_iter()
            .collect(),
            argvs: [(support::peer::Pid(1), vec![b"/sbin/init".to_vec()])]
                .into_iter()
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn approval_reply_from_a_session_caller_is_refused() {
        let mut rig = Rig::new(plain_target());
        let (_peer, _tx) = rig.push_entry(OWNED, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay(
            "c_1",
            OWNED,
            reply_with("c_1", approval_args("r_1")),
            &session_caller(&[CALLER_HOST]),
        );
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        assert_eq!(line["payload"]["command_id"], "c_1");
        let message = line["payload"]["message"].as_str().unwrap();
        assert_eq!(message, approval_refusal(OWNED), "{message}");
    }

    #[test]
    fn other_answers_from_a_session_caller_are_forwarded() {
        let mut rig = Rig::new(plain_target());
        let (peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay(
            "c_1",
            OTHER,
            reply_with("c_1", labels_args("r_1")),
            &session_caller(&[CALLER_HOST]),
        );
        let mut forwarded = String::new();
        std::io::BufReader::new(peer)
            .read_line(&mut forwarded)
            .unwrap();
        let sent: Value = serde_json::from_str(forwarded.trim_end()).unwrap();
        assert_eq!(sent["command"], "reply");
        assert!(sent.get("session_id").is_none());
    }

    #[test]
    fn steer_to_an_unrelated_session_is_allowed() {
        let mut rig = Rig::new(plain_target());
        let (peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay("c_1", OTHER, steer("c_1"), &session_caller(&[CALLER_HOST]));
        let mut forwarded = String::new();
        std::io::BufReader::new(peer)
            .read_line(&mut forwarded)
            .unwrap();
        let sent: Value = serde_json::from_str(forwarded.trim_end()).unwrap();
        assert_eq!(sent["command"], "steer");
    }

    #[test]
    fn steer_to_a_target_on_the_caller_walk_is_refused() {
        let mut rig = Rig::new(plain_target());
        let (_peer, _tx) = rig.push_entry(OWNED, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay("c_1", OWNED, steer("c_1"), &session_caller(&[TARGET]));
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        let message = line["payload"]["message"].as_str().unwrap();
        assert_eq!(
            message,
            session_command_refusal("steer", OWNED),
            "{message}"
        );
    }

    #[test]
    fn prompt_to_a_target_on_the_caller_walk_is_refused() {
        let mut rig = Rig::new(plain_target());
        let (_peer, _tx) = rig.push_entry(OWNED, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay("c_1", OWNED, prompt("c_1"), &session_caller(&[TARGET]));
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        let message = line["payload"]["message"].as_str().unwrap();
        assert_eq!(
            message,
            session_command_refusal("prompt", OWNED),
            "{message}"
        );
    }

    #[test]
    fn a_question_reply_to_a_target_on_the_caller_walk_is_refused() {
        let mut rig = Rig::new(plain_target());
        let (_peer, _tx) = rig.push_entry(OWNED, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay(
            "c_1",
            OWNED,
            reply_with("c_1", labels_args("r_1")),
            &session_caller(&[TARGET]),
        );
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        let message = line["payload"]["message"].as_str().unwrap();
        assert_eq!(
            message,
            session_command_refusal("reply", OWNED),
            "{message}"
        );
    }

    #[test]
    fn an_approval_reply_to_an_unrelated_session_is_refused() {
        let mut rig = Rig::new(plain_target());
        let (_peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay(
            "c_1",
            OTHER,
            reply_with("c_1", approval_args("r_1")),
            &session_caller(&[CALLER_HOST]),
        );
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        let message = line["payload"]["message"].as_str().unwrap();
        assert_eq!(message, approval_refusal(OTHER), "{message}");
    }

    #[test]
    fn steer_sharing_a_session_process_with_the_target_is_refused() {
        let script = Script {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (TARGET, SHARED_HOST),
                (SHARED_HOST, support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [
                (TARGET, support::peer::EffectiveUid(1000)),
                (SHARED_HOST, support::peer::EffectiveUid(1000)),
                (support::peer::Pid(1), support::peer::EffectiveUid(1000)),
            ]
            .into_iter()
            .collect(),
            argvs: [
                (SHARED_HOST, host_argv()),
                (TARGET, vec![b"/sbin/init".to_vec()]),
                (support::peer::Pid(1), vec![b"/sbin/init".to_vec()]),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let mut rig = Rig::new(script);
        let (_peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay("c_1", OTHER, steer("c_1"), &session_caller(&[SHARED_HOST]));
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        let message = line["payload"]["message"].as_str().unwrap();
        assert_eq!(
            message,
            session_command_refusal("steer", OTHER),
            "{message}"
        );
    }

    #[test]
    fn a_question_reply_sharing_a_session_process_with_the_target_is_refused() {
        let script = Script {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (TARGET, SHARED_HOST),
                (SHARED_HOST, support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [
                (TARGET, support::peer::EffectiveUid(1000)),
                (SHARED_HOST, support::peer::EffectiveUid(1000)),
                (support::peer::Pid(1), support::peer::EffectiveUid(1000)),
            ]
            .into_iter()
            .collect(),
            argvs: [
                (SHARED_HOST, host_argv()),
                (TARGET, vec![b"/sbin/init".to_vec()]),
                (support::peer::Pid(1), vec![b"/sbin/init".to_vec()]),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let mut rig = Rig::new(script);
        let (_peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay(
            "c_1",
            OTHER,
            reply_with("c_1", labels_args("r_1")),
            &session_caller(&[SHARED_HOST]),
        );
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        let message = line["payload"]["message"].as_str().unwrap();
        assert_eq!(
            message,
            session_command_refusal("reply", OTHER),
            "{message}"
        );
    }

    #[test]
    fn a_message_to_a_target_on_the_caller_walk_is_forwarded() {
        let mut rig = Rig::new(plain_target());
        let (peer, _tx) = rig.push_entry(OWNED, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay("c_1", OWNED, message("c_1"), &session_caller(&[TARGET]));
        let mut forwarded = String::new();
        std::io::BufReader::new(peer)
            .read_line(&mut forwarded)
            .unwrap();
        let sent: Value = serde_json::from_str(forwarded.trim_end()).unwrap();
        assert_eq!(sent["command"], "message");
    }

    #[test]
    fn a_full_subscribe_to_an_unknown_session_is_not_found_not_refused() {
        let mut rig = Rig::new(plain_target());
        rig.relay(
            "c_1",
            "s_aaaaaaaaaaaaaaaa",
            full_subscribe("c_1"),
            &session_caller(&[CALLER_HOST]),
        );
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "session_not_found");
    }

    #[test]
    fn approval_reply_to_an_unknown_session_is_not_found_not_refused() {
        let mut rig = Rig::new(plain_target());
        rig.relay(
            "c_1",
            "s_aaaaaaaaaaaaaaaa",
            reply_with("c_1", approval_args("r_1")),
            &session_caller(&[CALLER_HOST]),
        );
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "session_not_found");
    }

    #[test]
    fn approval_reply_to_a_synthetic_replay_is_not_refused() {
        let command = reply_with("c_1", approval_args("r_1"));
        let stripped = command.as_object().unwrap();
        let target = contract::SessionId(OWNED.to_owned());
        let read = plain_target();
        assert!(
            crate::relay::session_command_refusal(
                &session_caller(&[CALLER_HOST]),
                stripped,
                &target,
                crate::relay::RelayTarget::SyntheticReplay,
                &read,
            )
            .is_none()
        );
    }

    #[test]
    fn the_direct_parent_reaches_its_delegate() {
        let script = Script {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (TARGET, PEER),
                (PEER, support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [
                (TARGET, support::peer::EffectiveUid(1000)),
                (PEER, support::peer::EffectiveUid(1000)),
                (support::peer::Pid(1), support::peer::EffectiveUid(1000)),
            ]
            .into_iter()
            .collect(),
            argvs: [
                (PEER, vec![b"/bin/sh".to_vec()]),
                (support::peer::Pid(1), vec![b"/sbin/init".to_vec()]),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let mut rig = Rig::new(script);
        let (peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay("c_1", OTHER, steer("c_1"), &session_caller(&[CALLER_HOST]));
        let mut forwarded = String::new();
        std::io::BufReader::new(peer)
            .read_line(&mut forwarded)
            .unwrap();
        let sent: Value = serde_json::from_str(forwarded.trim_end()).unwrap();
        assert_eq!(sent["command"], "steer");
    }

    #[test]
    fn exact_parent_and_target_ancestry_share_one_parent_read() {
        let parent_reads = std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
        let script = Script {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (PEER, SHARED_HOST),
                (SHARED_HOST, support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            parent_changes: [(TARGET, (PEER, SHARED_HOST))].into_iter().collect(),
            parent_reads: std::sync::Arc::clone(&parent_reads),
            euids: [
                (TARGET, support::peer::EffectiveUid(1000)),
                (PEER, support::peer::EffectiveUid(1000)),
                (SHARED_HOST, support::peer::EffectiveUid(1000)),
                (support::peer::Pid(1), support::peer::EffectiveUid(1000)),
            ]
            .into_iter()
            .collect(),
            argvs: [
                (PEER, vec![b"/bin/sh".to_vec()]),
                (SHARED_HOST, host_argv()),
                (support::peer::Pid(1), vec![b"/sbin/init".to_vec()]),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let mut rig = Rig::new(script);
        let (peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay("c_1", OTHER, steer("c_1"), &session_caller(&[SHARED_HOST]));
        let mut forwarded = String::new();
        std::io::BufReader::new(peer)
            .read_line(&mut forwarded)
            .unwrap();
        let sent: Value = serde_json::from_str(forwarded.trim_end()).unwrap();
        assert_eq!(sent["command"], "steer");
        assert_eq!(parent_reads.lock().unwrap().get(&TARGET), Some(&1));
    }

    #[test]
    fn an_incomplete_caller_with_the_parent_exception_still_reaches() {
        let script = Script {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (TARGET, PEER),
                (PEER, support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [
                (TARGET, support::peer::EffectiveUid(1000)),
                (PEER, support::peer::EffectiveUid(1000)),
                (support::peer::Pid(1), support::peer::EffectiveUid(1000)),
            ]
            .into_iter()
            .collect(),
            argvs: [
                (PEER, vec![b"/bin/sh".to_vec()]),
                (support::peer::Pid(1), vec![b"/sbin/init".to_vec()]),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let mut rig = Rig::new(script);
        let (peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay("c_1", OTHER, steer("c_1"), &incomplete());
        let mut forwarded = String::new();
        std::io::BufReader::new(peer)
            .read_line(&mut forwarded)
            .unwrap();
        let sent: Value = serde_json::from_str(forwarded.trim_end()).unwrap();
        assert_eq!(sent["command"], "steer");
    }

    #[test]
    fn an_incomplete_caller_without_the_exception_names_its_pid() {
        let mut rig = Rig::new(plain_target());
        let (_peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay("c_1", OTHER, steer("c_1"), &incomplete());
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        let message = line["payload"]["message"].as_str().unwrap();
        assert!(message.contains(&UNREADABLE.to_string()), "{message}");
        assert!(message.contains("incomplete"), "{message}");
        assert!(message.ends_with("`steer` is refused."), "{message}");
    }

    #[test]
    fn an_incomplete_target_walk_names_its_pid() {
        let script = Script {
            current: support::peer::EffectiveUid(1000),
            parents: HashMap::new(),
            euids: HashMap::new(),
            argvs: HashMap::new(),
            ..Default::default()
        };
        let mut rig = Rig::new(script);
        let (_peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay("c_1", OTHER, steer("c_1"), &session_caller(&[CALLER_HOST]));
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        let message = line["payload"]["message"].as_str().unwrap();
        assert!(message.contains(OTHER), "{message}");
    }

    #[test]
    fn remote_commands_route_unchanged() {
        let mut rig = Rig::new(plain_target());
        let (peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay("c_1", OTHER, steer("c_1"), &Attribution::Remote);
        let mut forwarded = String::new();
        std::io::BufReader::new(peer)
            .read_line(&mut forwarded)
            .unwrap();
        let sent: Value = serde_json::from_str(forwarded.trim_end()).unwrap();
        assert_eq!(sent["command"], "steer");
    }

    #[test]
    fn approval_reply_on_a_failed_credential_is_refused() {
        let mut rig = Rig::new(plain_target());
        let (_peer, _tx) = rig.push_entry(OWNED, crate::relay::RelayTarget::Live { pid: TARGET });
        let attribution = Attribution::Incomplete {
            peer_pid: None,
            pid: None,
            peer_error: Some("injected credential failure".to_owned()),
        };
        rig.relay(
            "c_1",
            OWNED,
            reply_with("c_1", approval_args("r_1")),
            &attribution,
        );
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        let message = line["payload"]["message"].as_str().unwrap();
        assert!(message.contains("injected credential failure"), "{message}");
        assert!(message.ends_with("`reply` is refused."), "{message}");
    }

    /// A live client against a hub whose walk is scripted: the peer is
    /// this test process, so the script names it.
    fn pairing_client(script: Script) -> (super::Client, Temp) {
        let temp = Temp::new();
        let hub = temp.hub(FakeStarter::hang(&temp.dir));
        *crate::connection::lock(&hub.test_read) = Some(std::sync::Arc::new(script));
        let mut client = super::Client::connect(&hub);
        client.hello();
        (client, temp)
    }

    fn session_walk() -> Script {
        let own = support::peer::Pid(std::process::id());
        Script {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (own, support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [(support::peer::Pid(1), support::peer::EffectiveUid(1000))]
                .into_iter()
                .collect(),
            argvs: [(
                support::peer::Pid(1),
                vec![
                    b"/path/to/fiber".to_vec(),
                    b"session".to_vec(),
                    format!("--id={OWNED}").into_bytes(),
                ],
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }

    fn empty_walk() -> Script {
        let own = support::peer::Pid(std::process::id());
        Script {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (own, support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [(support::peer::Pid(1), support::peer::EffectiveUid(0))]
                .into_iter()
                .collect(),
            argvs: HashMap::new(),
            ..Default::default()
        }
    }

    #[test]
    fn pairing_code_from_a_session_caller_is_refused() {
        let (mut client, _temp) = pairing_client(session_walk());
        client.send(&json!({"id": "c_p", "command": "pairing_code", "args": {"device": "probe"}}));
        let line = client.next("the refusal");
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        assert_eq!(line["payload"]["command_id"], "c_p");
        let message = line["payload"]["message"].as_str().unwrap();
        assert_eq!(message, PAIRING_REFUSAL, "{message}");
    }

    #[test]
    fn pairing_code_from_a_caller_passing_no_session_process_is_minted() {
        let (mut client, _temp) = pairing_client(empty_walk());
        client.send(&json!({"id": "c_p", "command": "pairing_code", "args": {"device": "probe"}}));
        let line = client.next("the mint");
        assert_eq!(line["kind"], "command_accepted", "{line}");
        assert_eq!(line["payload"]["command_id"], "c_p");
    }

    #[test]
    fn pairing_code_with_an_unreadable_caller_walk_fails_closed() {
        let own = support::peer::Pid(std::process::id());
        let script = Script {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (own, support::peer::Pid(50)),
                (support::peer::Pid(50), support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [(support::peer::Pid(1), support::peer::EffectiveUid(1000))]
                .into_iter()
                .collect(),
            argvs: HashMap::new(),
            ..Default::default()
        };
        let (mut client, _temp) = pairing_client(script);
        client.send(&json!({"id": "c_p", "command": "pairing_code", "args": {"device": "probe"}}));
        let line = client.next("the refusal");
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        let message = line["payload"]["message"].as_str().unwrap();
        assert!(message.contains("`50`"), "{message}");
    }

    /// The no-relay first command: a listener stands in for the session
    /// socket, so the hub's opener connects to this test process.
    fn first_command_rig(script: Script, session: &str) -> (Rig, std::os::unix::net::UnixListener) {
        let rig = Rig::new(script);
        let run = rig._temp.dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(run.join(session)).unwrap();
        (rig, listener)
    }

    #[test]
    fn a_first_command_from_a_session_caller_is_refused_before_it_publishes() {
        let own = support::peer::Pid(std::process::id());
        let script = Script {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (own, TARGET),
                (TARGET, support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [
                (TARGET, support::peer::EffectiveUid(1000)),
                (support::peer::Pid(1), support::peer::EffectiveUid(0)),
            ]
            .into_iter()
            .collect(),
            argvs: [(
                TARGET,
                vec![
                    b"/path/to/fiber".to_vec(),
                    b"session".to_vec(),
                    format!("--id={OWNED}").into_bytes(),
                ],
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let (mut rig, listener) = first_command_rig(script, OTHER);
        // The opener's peer is this test process, whose walk passes the
        // session host above it; the target's own walk shares that host
        // with the caller's, so the command is refused.
        let accepted = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
        });
        rig.relay("c_1", OTHER, steer("c_1"), &session_caller(&[TARGET]));
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        assert!(
            crate::connection::lock(&rig.relays).entries.is_empty(),
            "no relay publishes on a refused first command"
        );
        drop(accepted.join().unwrap());
    }

    #[test]
    fn a_first_command_from_a_remote_peer_publishes() {
        let script = Script {
            current: support::peer::EffectiveUid(1000),
            parents: HashMap::new(),
            euids: HashMap::new(),
            argvs: HashMap::new(),
            ..Default::default()
        };
        let (mut rig, listener) = first_command_rig(script, OTHER);
        // No subscription is kept on this rig, so the opener writes the
        // command alone: one line, not a subscribe plus the command. The
        // mock session socket stays open through the publication assertion,
        // so the relay thread never observes EOF and removes the entry
        // first; the held stream drops afterward or on unwinding.
        let accepted = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut read = std::io::BufReader::new(stream);
            let mut first = String::new();
            read.read_line(&mut first).unwrap();
            let stream = read.into_inner();
            (first, stream)
        });
        rig.relay("c_1", OTHER, steer("c_1"), &Attribution::Remote);
        let (forwarded, _accepted) = accepted.join().unwrap();
        let sent: Value = serde_json::from_str(forwarded.trim_end()).unwrap();
        assert_eq!(sent["command"], "steer");
        let held = crate::connection::lock(&rig.relays);
        assert_eq!(held.entries.len(), 1);
        let entry = held.entries.first().unwrap();
        assert_eq!(entry.session, OTHER);
        assert_eq!(
            entry.target,
            crate::relay::RelayTarget::Live {
                pid: support::peer::Pid(std::process::id())
            }
        );
    }

    fn ask_argv() -> Vec<Vec<u8>> {
        ["/path/to/fiber", "ask", "--prompt=hi"]
            .iter()
            .map(|word| word.as_bytes().to_vec())
            .collect()
    }

    fn extension_case_argv() -> Vec<Vec<u8>> {
        ["/path/to/fiber", "extension-case", "casefile"]
            .iter()
            .map(|word| word.as_bytes().to_vec())
            .collect()
    }

    /// The caller walk with a session-hosting `ask` above the peer: the
    /// shared predicate marks it without any session id.
    fn ask_walk() -> PeerScript {
        PeerScript {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (support::peer::Pid(10), support::peer::Pid(20)),
                (support::peer::Pid(20), support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [
                (support::peer::Pid(20), support::peer::EffectiveUid(1000)),
                (support::peer::Pid(1), support::peer::EffectiveUid(1000)),
            ]
            .into_iter()
            .collect(),
            argvs: [
                (support::peer::Pid(20), ask_argv()),
                (support::peer::Pid(1), vec![b"/sbin/init".to_vec()]),
            ]
            .into_iter()
            .collect(),
            unreadable_parent: vec![],
            unreadable_euid: vec![],
            unreadable_argv: vec![],
            argv_read: std::cell::Cell::new(0),
        }
    }

    #[test]
    fn ask_argv_on_the_caller_walk_marks_the_session_process_pid() {
        let attribution = attribute_peer(Ok(support::peer::Pid(10)), &ask_walk());
        let Attribution::Local {
            session_process_pids,
            ..
        } = attribution
        else {
            panic!("an ask descendant is a local session-caller peer");
        };
        assert!(session_process_pids.contains(&support::peer::Pid(20)));
    }

    #[test]
    fn extension_case_argv_on_the_caller_walk_marks_the_session_process_pid() {
        let mut walk = ask_walk();
        walk.argvs
            .insert(support::peer::Pid(20), extension_case_argv());
        let attribution = attribute_peer(Ok(support::peer::Pid(10)), &walk);
        let Attribution::Local {
            session_process_pids,
            ..
        } = attribution
        else {
            panic!("an extension-case descendant is a local session-caller peer");
        };
        assert!(session_process_pids.contains(&support::peer::Pid(20)));
    }

    /// The target walk with a session-hosting `ask` above the target: the
    /// target shares a session process with a caller below that host.
    fn ask_target() -> Script {
        const ASK: support::peer::Pid = support::peer::Pid(440);
        Script {
            current: support::peer::EffectiveUid(1000),
            parents: [
                (TARGET, ASK),
                (ASK, support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [
                (TARGET, support::peer::EffectiveUid(1000)),
                (ASK, support::peer::EffectiveUid(1000)),
                (support::peer::Pid(1), support::peer::EffectiveUid(1000)),
            ]
            .into_iter()
            .collect(),
            argvs: [
                (ASK, ask_argv()),
                (TARGET, vec![b"/sbin/init".to_vec()]),
                (support::peer::Pid(1), vec![b"/sbin/init".to_vec()]),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn steer_refused_when_the_target_walk_passes_an_ask_host() {
        let mut rig = Rig::new(ask_target());
        let (_peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay(
            "c_1",
            OTHER,
            steer("c_1"),
            &session_caller(&[support::peer::Pid(440)]),
        );
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected", "{line}");
        assert_eq!(line["payload"]["code"], "from_session", "{line}");
        assert_eq!(
            line["payload"]["message"],
            session_command_refusal("steer", OTHER),
            "{line}"
        );
    }

    #[test]
    fn question_reply_refused_when_the_target_walk_passes_an_ask_host() {
        let mut rig = Rig::new(ask_target());
        let (_peer, _tx) = rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: TARGET });
        rig.relay(
            "c_1",
            OTHER,
            reply_with("c_1", labels_args("r_1")),
            &session_caller(&[support::peer::Pid(440)]),
        );
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected", "{line}");
        assert_eq!(line["payload"]["code"], "from_session", "{line}");
        assert_eq!(
            line["payload"]["message"],
            session_command_refusal("reply", OTHER),
            "{line}"
        );
    }

    #[test]
    fn no_command_routes_under_stale_authorization() {
        // The stale entry's target is the caller's own child, so the
        // direct-parent exception allows the command; the replacement
        // entry's target has no readable walk, so a fresh check refuses
        // it. Without the epoch re-check the stale allow would write to
        // the new entry.
        const STALE_PID: support::peer::Pid = support::peer::Pid(50);
        const FRESH_PID: support::peer::Pid = support::peer::Pid(60);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut script = Script {
            current: support::peer::EffectiveUid(1000),
            // The fresh target's parent is unreadable, so its check fails
            // closed; the stale target's parent stays readable for the
            // direct-parent exception.
            parents: [
                (STALE_PID, PEER),
                (PEER, support::peer::Pid(1)),
                (support::peer::Pid(1), support::peer::Pid(0)),
            ]
            .into_iter()
            .collect(),
            euids: [
                (STALE_PID, support::peer::EffectiveUid(1000)),
                (FRESH_PID, support::peer::EffectiveUid(1000)),
                (PEER, support::peer::EffectiveUid(1000)),
                (support::peer::Pid(1), support::peer::EffectiveUid(1000)),
            ]
            .into_iter()
            .collect(),
            argvs: [
                (PEER, vec![b"/bin/sh".to_vec()]),
                (support::peer::Pid(1), vec![b"/sbin/init".to_vec()]),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        script.block_first_parent = Some((entered_tx, std::sync::Mutex::new(release_rx)));
        let mut rig = Rig::new(script);
        let (mut peer_old, _keep_old) =
            rig.push_entry(OTHER, crate::relay::RelayTarget::Live { pid: STALE_PID });
        peer_old.set_nonblocking(true).unwrap();
        let hub = std::sync::Arc::clone(&rig.hub);
        let writer = std::sync::Arc::clone(&rig.writer);
        let relays = std::sync::Arc::clone(&rig.relays);
        let routed = std::thread::spawn(move || {
            crate::relay::relay_command(
                &contract::CommandId("c_1".to_owned()),
                OTHER,
                &steer("c_1"),
                &hub,
                &writer,
                &relays,
                &session_caller(&[CALLER_HOST]),
            );
        });
        // The route parks in the stale target's parent read; replace the
        // entry before it is allowed to answer, so the check and the write
        // see different epochs.
        fakes::Deadline::after(super::DEADLINE)
            .recv_or_fail(&entered_rx, "the route to park in the parent read");
        let (old, mut peer_new) = {
            let mut held = crate::connection::lock(&rig.relays);
            let at = held
                .entries
                .iter()
                .position(|entry| entry.session == OTHER)
                .unwrap();
            let mut old = held.entries.remove(at);
            let (writer_new, peer) = std::os::unix::net::UnixStream::pair().unwrap();
            peer.set_nonblocking(true).unwrap();
            held.entries.push(Relay {
                session: OTHER.to_owned(),
                epoch: old.epoch + 1,
                writer: writer_new,
                kept: Kept::default(),
                replayed: Replayed::default(),
                thread: old.thread.take(),
                retiring: None,
                target: crate::relay::RelayTarget::Live { pid: FRESH_PID },
            });
            (old, peer)
        };
        release_tx.send(()).unwrap();
        routed.join().unwrap();
        // The fresh check refused, so neither entry got the command.
        let line = rig.next_client_line();
        assert_eq!(line["kind"], "command_rejected");
        assert_eq!(line["payload"]["code"], "from_session");
        let message = line["payload"]["message"].as_str().unwrap();
        assert!(message.contains(OTHER), "{message}");
        for (name, peer) in [("stale", &mut peer_old), ("fresh", &mut peer_new)] {
            let mut idle = [0u8; 1];
            let idle = std::io::Read::read(peer, &mut idle);
            assert!(
                matches!(idle, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                "nothing reaches the {name} entry"
            );
        }
        assert!(
            crate::connection::lock(&old.kept).is_empty(),
            "nothing queues on the stale entry"
        );
    }

    #[test]
    fn incomplete_connection_refusal_names_its_command() {
        let attribution = Attribution::Incomplete {
            peer_pid: None,
            pid: None,
            peer_error: Some("credential denied".to_owned()),
        };
        let target = contract::SessionId(OTHER.to_owned());
        let message = crate::relay::from_session_message(
            &attribution,
            crate::relay::FromSessionRefusal::SessionCommand {
                command: "steer",
                target: &target,
            },
        );
        assert!(message.ends_with("`steer` is refused."), "{message}");
    }
}
