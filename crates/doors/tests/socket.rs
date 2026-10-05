//! Clients on a session socket: subscriptions, acknowledgements, the commands
//! served at once, and close (`docs/invocation.md`, "Driver commands").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{
    Empty, Event, ExtensionsLoaded, FiberExited, InputItem, LoadedExtension, Notice, QueuedMessage,
    SessionState, SessionStatus, SteeringQueue, ToolInfo, ToolSource, ToolState, TurnStarted,
};
use contract::inbox::{Delivery, Message};
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Failure, Origin, Process, Sender, Tokens, Usage};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, ErrorCode, SessionId};
use doors::{Session, mint};
use fakes::Client;
use fakes::clock::FakeClock;
use log::Log;
use serde_json::{Map, Value};

/// A hang bound for one line, the same order as the log crate's watcher tests.
const DEADLINE: Duration = Duration::from_secs(10);

const MALFORMED: &str = "A command is one JSON object per line, with a string `id` and `command`.";
const NOT_SUBSCRIBED: &str = "Send `subscribe` first.";
const ALREADY: &str = "This connection is already subscribed.";
const UNFIT: &str = "The arguments do not fit this command.";
const PAST: &str = "`from_seq` is past the latest line.";
const REVERSED: &str = "`to_seq` is before `from_seq`.";
const ENDED: &str = "The session ended before answering.";

struct Temp(
    PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")] fakes::TempDir,
);

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("fd");
        let dir = held.path().to_path_buf();
        Self(dir, held)
    }
}

#[derive(Clone)]
struct Shared {
    buf: Arc<Mutex<Vec<u8>>>,
    ready: Arc<Condvar>,
}

impl Shared {
    fn new() -> Self {
        Self {
            buf: Arc::new(Mutex::new(Vec::new())),
            ready: Arc::new(Condvar::new()),
        }
    }
}

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(buf);
        self.ready.notify_all();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Opened {
    _temp: Temp,
    clock: Arc<FakeClock>,
    log: Arc<Log>,
    sessions: PathBuf,
    id: SessionId,
    dir: PathBuf,
    socket: PathBuf,
    out: Shared,
    session: Session,
}

impl Opened {
    fn open(tools: Vec<ToolInfo>) -> Self {
        let temp = Temp::new();
        let home = temp.0.join("h");
        let sessions = home.join("projects/p/sessions");
        let id = SessionId(mint("s_"));
        let dir = sessions.join(&id.0);
        let clock = FakeClock::new();
        let timed = Arc::clone(&clock);
        let timed: Arc<dyn Clock> = timed;
        let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
        let out = Shared::new();
        let session =
            Session::open(&home, &dir, &log, timed, tools, Box::new(out.clone())).unwrap();
        Self {
            _temp: temp,
            clock,
            log,
            sessions,
            id: id.clone(),
            dir,
            socket: home.join("run").join(&id.0),
            out,
            session,
        }
    }

    /// Closes the session and returns its temporary directory, still present
    /// when the session kept it.
    fn close(self) -> Temp {
        let Opened {
            session,
            log,
            _temp,
            ..
        } = self;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            session.close(log);
            if let Ok(()) = tx.send(()) {}
        });
        rx.recv_timeout(DEADLINE).expect("close returned");
        _temp
    }
}

fn next(client: &Client) -> Value {
    client.recv(DEADLINE).expect("a line arrived")
}

fn until(client: &Client, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
    let mut lines = Vec::new();
    loop {
        let line = next(client);
        let stop = done(&line);
        lines.push(line);
        if stop {
            return lines;
        }
    }
}

fn kind(line: &Value) -> &str {
    line["kind"].as_str().unwrap()
}

fn command_id(line: &Value) -> Option<&str> {
    line["payload"].get("command_id").and_then(Value::as_str)
}

fn seqs(lines: &[Value]) -> Vec<u64> {
    lines
        .iter()
        .filter_map(|line| line["seq"].as_u64())
        .collect()
}

fn send(client: &Client, line: &str) {
    client.send(line).unwrap();
}

/// The acknowledgement for `id`, skipping events that belong to the session.
fn response(client: &Client, id: &str) -> Value {
    until(client, |line| command_id(line) == Some(id))
        .into_iter()
        .next_back()
        .unwrap()
}

fn subscribe(client: &Client, id: &str, level: &str) -> Value {
    send(
        client,
        &format!(r#"{{"id":"{id}","command":"subscribe","args":{{"level":"{level}"}}}}"#),
    );
    let line = next(client);
    assert_eq!(kind(&line), "command_accepted", "{line}");
    assert_eq!(command_id(&line).unwrap(), id);
    assert_eq!(
        line["ts"].as_u64(),
        Some(1_700_000_000_000),
        "an acknowledgement's ts comes from the clock"
    );
    line
}

fn rejection(line: &Value) -> (&str, &str) {
    assert_eq!(kind(line), "command_rejected", "{line}");
    (
        line["payload"]["code"].as_str().unwrap(),
        line["payload"]["message"].as_str().unwrap(),
    )
}

fn step() -> Event {
    Event::StepStarted(Empty {})
}

fn notice(message: &str) -> Event {
    Event::Notice(Notice {
        code: ErrorCode::IoFailed,
        message: message.to_owned(),
        extension: None,
    })
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 0,
        },
        cost: Some(0.0),
        subscription_cost: 0.0,
    }
}

fn status(name: &str) -> Event {
    Event::SessionStatus(SessionStatus {
        name: name.to_owned(),
        workspace: "/w".into(),
        parent: None,
        model: "m".into(),
        state: SessionState::Idle,
        since: 0,
        git: None,
        context: None,
        spend: usage(),
        delegates: 0,
        jobs: 0,
    })
}

fn extensions() -> Event {
    Event::ExtensionsLoaded(ExtensionsLoaded {
        extensions: vec![LoadedExtension {
            name: "demo".into(),
            version: "1".into(),
        }],
    })
}

fn exited() -> Event {
    Event::FiberExited(FiberExited {
        exit_code: 0,
        usage: usage(),
        final_message: None,
        error: None,
        suspended_on: None,
        questions: None,
    })
}

fn turn() -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Text { text: "hi".into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: CommandId("c_1".into()),
            },
            changed_by: None,
        }],
    })
}

fn tool() -> ToolInfo {
    ToolInfo {
        name: "read".into(),
        source: ToolSource::Builtin,
        state: ToolState::Full,
        bytes: 12,
        tokens: None,
    }
}

fn text_of(message: &Message) -> String {
    match message.content.as_slice() {
        [ContentPart::Text { text }] => text.clone(),
        _ => panic!("a text part"),
    }
}

/// What the stand-in took, and it accepts the delivery.
fn take(inbox: &Receiver<Delivery>) -> String {
    match inbox.recv_timeout(DEADLINE).expect("a delivery") {
        Delivery::Prompt(message, ack) => {
            let text = text_of(&message);
            ack.0(Ok(None));
            format!("prompt {text}")
        }
        Delivery::Steer(message, ack) => {
            let text = text_of(&message);
            ack.0(Ok(None));
            format!("steer {text}")
        }
        Delivery::SteerDrop(id, ack) => {
            ack.0(Ok(None));
            format!("drop {}", id.0)
        }
        Delivery::Reply(reply, ack) => {
            ack.0(Ok(None));
            format!("reply {}", reply.request_id.0)
        }
        Delivery::Close(ack) => {
            ack.0(Ok(None));
            "close".to_owned()
        }
        Delivery::Cancelled => panic!("a wake arrives as a delivery"),
        Delivery::Job(_) => panic!("no job runs here"),
    }
}

#[test]
fn a_bad_line_is_malformed_and_only_a_string_id_is_echoed() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            let cases = [
                ("not json", None),
                ("[]", None),
                (r#"{"command":"tools"}"#, None),
                (r#"{"id":1,"command":"tools"}"#, None),
                (r#"{"id":"c_1","command":1}"#, Some("c_1")),
                (r#"{"id":"c_2","command":"tools","extra":1}"#, Some("c_2")),
                (r#"{"id":"c_3","command":"tools","args":[]}"#, Some("c_3")),
                (r#"{"id":"c_4","command":"tools","args":null}"#, Some("c_4")),
            ];
            for (line, id) in cases {
                send(&client, line);
                let answer = next(&client);
                assert_eq!(rejection(&answer), ("malformed", MALFORMED), "{line}");
                assert_eq!(command_id(&answer), id, "{line}");
            }

            let mut raw = UnixStream::connect(&socket).unwrap();
            raw.write_all(b"\xff\n").unwrap();
            raw.flush().unwrap();
            raw.set_read_timeout(Some(DEADLINE)).unwrap();
            let mut buf = Vec::new();
            BufReader::new(raw)
                .read_until(b'\n', &mut buf)
                .expect("a malformed line is answered");
            buf.pop();
            let answer: Value = serde_json::from_slice(&buf).unwrap();
            assert_eq!(rejection(&answer), ("malformed", MALFORMED));
            assert_eq!(command_id(&answer), None);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn a_command_without_a_trailing_newline_is_answered() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let mut raw = UnixStream::connect(&socket).unwrap();
            raw.write_all(br#"{"id":"c_nl","command":"tools"}"#)
                .unwrap();
            raw.shutdown(std::net::Shutdown::Write).unwrap();
            raw.set_read_timeout(Some(DEADLINE)).unwrap();
            let mut line = String::new();
            BufReader::new(&raw)
                .read_line(&mut line)
                .expect("a command without a trailing newline is answered");
            let value: Value = serde_json::from_str(line.trim_end()).unwrap();
            assert_eq!(command_id(&value), Some("c_nl"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn subscribe_is_first_and_unknown_or_unfit_commands_are_rejected() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            send(&client, r#"{"id":"c_early","command":"tools"}"#);
            let early = next(&client);
            assert_eq!(rejection(&early), ("not_subscribed", NOT_SUBSCRIBED));
            assert_eq!(command_id(&early), Some("c_early"));

            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_again","command":"subscribe","args":{"level":"summary"}}"#,
            );
            let again = response(&client, "c_again");
            assert_eq!(rejection(&again), ("invalid_arguments", ALREADY));

            send(&client, r#"{"id":"c_nope","command":"frob"}"#);
            let unknown = response(&client, "c_nope");
            assert_eq!(
                rejection(&unknown),
                ("unknown_command", "`frob` is not built in this Fiber yet.")
            );

            for name in [
                "message", "job_stop", "background", "reload", "model", "credential",
                "name",
                "handoff", "rewind", "command",
            ] {
                send(
                    &client,
                    &format!(r#"{{"id":"c_{name}","command":"{name}"}}"#),
                );
                let line = response(&client, &format!("c_{name}"));
                assert_eq!(
                    rejection(&line),
                    (
                        "unknown_command",
                        format!("`{name}` is not built in this Fiber yet.").as_str()
                    ),
                    "{name}"
                );
            }

            send(&client, r#"{"id":"c_shell","command":"shell"}"#);
            assert_eq!(
                rejection(&response(&client, "c_shell")),
                ("invalid_arguments", UNFIT)
            );
            send(
                &client,
                r#"{"id":"c_bad","command":"prompt","args":{"content":"nope"}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_bad")),
                ("invalid_arguments", UNFIT)
            );
            send(
                &client,
                r#"{"id":"c_null","command":"history","args":{"from_seq":null}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_null")),
                ("invalid_arguments", UNFIT)
            );
            send(
                &client,
                r#"{"id":"c_img","command":"prompt","args":{"content":[{"type":"image","data":"YQ==","mime_type":"image/png"}]}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_img")),
                (
                    "invalid_arguments",
                    "Image 1 cannot be read: this Fiber processes no images yet."
                )
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn full_folds_the_log_then_live_lines_and_the_latest_status() {
    let opened = Opened::open(vec![]);
    opened.log.append(&step(), None, None).unwrap();
    opened.log.append(&step(), None, None).unwrap();
    opened.log.append(&status("alpha"), None, None).unwrap();
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            let folded = until(&client, |line| kind(line) == "session_status");
            let seq_at = folded
                .iter()
                .position(|line| line["seq"].as_u64() == Some(1))
                .expect("seq 1 is in the fold");
            let status_at = folded
                .iter()
                .position(|line| kind(line) == "session_status")
                .unwrap();
            assert!(seq_at < status_at, "status follows the fold");
            assert_eq!(folded[status_at]["payload"]["name"], "alpha");
            assert_eq!(seqs(&folded), vec![0, 1]);

            log.append(&step(), None, None).unwrap();
            let live = until(&client, |line| line["seq"].as_u64() == Some(2));
            assert_eq!(seqs(&live), vec![2]);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn two_full_clients_see_the_same_durable_lines() {
    let opened = Opened::open(vec![]);
    opened.log.append(&step(), None, None).unwrap();
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let first = Client::connect(&socket).unwrap();
            let second = Client::connect(&socket).unwrap();
            subscribe(&first, "c_a", "full");
            subscribe(&second, "c_b", "full");
            log.append(&step(), None, None).unwrap();
            log.append(&step(), None, None).unwrap();
            let a = until(&first, |line| line["seq"].as_u64() == Some(2));
            let b = until(&second, |line| line["seq"].as_u64() == Some(2));
            assert_eq!(seqs(&a), vec![0, 1, 2]);
            assert_eq!(seqs(&b), seqs(&a));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn summary_gets_the_latest_status_and_extensions_and_nothing_else() {
    let opened = Opened::open(vec![]);
    opened.log.append(&status("alpha"), None, None).unwrap();
    opened.log.append(&extensions(), None, None).unwrap();
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            let ack = subscribe(&client, "c_sub", "summary");
            let status_line = next(&client);
            let extensions_line = next(&client);
            assert_eq!(kind(&ack), "command_accepted");
            assert_eq!(kind(&status_line), "session_status");
            assert_eq!(status_line["payload"]["name"], "alpha");
            assert_eq!(kind(&extensions_line), "extensions_loaded");
            assert_eq!(extensions_line["payload"]["extensions"][0]["name"], "demo");

            log.append(&step(), None, None).unwrap();
            log.append(&notice("nope"), None, None).unwrap();
            log.append(&status("beta"), None, None).unwrap();
            let later = until(&client, |line| {
                kind(line) == "session_status" && line["payload"]["name"] == "beta"
            });
            let mut lines = vec![ack, status_line, extensions_line];
            lines.extend(later);
            assert!(lines.iter().all(|line| {
                matches!(
                    kind(line),
                    "command_accepted" | "session_status" | "extensions_loaded"
                )
            }));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn clients_counts_full_connections_on_attach_and_leave() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let summary = Client::connect(&socket).unwrap();
            subscribe(&summary, "c_sum", "summary");
            let full = Client::connect(&socket).unwrap();
            subscribe(&full, "c_full", "full");
            let attached = until(&full, |line| kind(line) == "clients");
            let counts: Vec<u64> = attached
                .iter()
                .filter(|line| kind(line) == "clients")
                .map(|line| line["payload"]["count"].as_u64().unwrap())
                .collect();
            assert_eq!(counts, vec![1], "a summary connection is not counted");

            let other = Client::connect(&socket).unwrap();
            subscribe(&other, "c_other", "full");
            let both = until(&full, |line| {
                kind(line) == "clients" && line["payload"]["count"] == 2
            });
            assert!(both.iter().any(|line| line["payload"]["count"] == 2));
            let other_counts: Vec<u64> = until(&other, |line| kind(line) == "clients")
                .iter()
                .filter(|line| kind(line) == "clients")
                .map(|line| line["payload"]["count"].as_u64().unwrap())
                .collect();
            assert_eq!(other_counts, vec![2]);

            drop(other);
            let left = until(&full, |line| {
                kind(line) == "clients" && line["payload"]["count"] == 1
            });
            assert!(left.iter().any(|line| line["payload"]["count"] == 1));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn tools_and_history_answer_while_the_inbox_is_unread() {
    let opened = Opened::open(vec![tool()]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"wait"}]}}"#,
            );
            send(&client, r#"{"id":"c_hist0","command":"history","args":{"from_seq":0}}"#);
            let empty = response(&client, "c_hist0");
            assert_eq!(rejection(&empty), ("invalid_arguments", PAST));
            assert_eq!(command_id(&empty), Some("c_hist0"));

            send(&client, r#"{"id":"c_tools","command":"tools"}"#);
            let tools = response(&client, "c_tools");
            assert_eq!(kind(&tools), "command_accepted");
            assert_eq!(tools["payload"]["result"]["tools"][0]["name"], "read");
            assert!(tools["payload"]["result"]["tools"][0].get("tokens").is_none());

            for _ in 0..300 {
                log.append(&step(), None, None).unwrap();
            }
            send(&client, r#"{"id":"c_page","command":"history","args":{"from_seq":0}}"#);
            let page = response(&client, "c_page");
            assert_eq!(kind(&page), "command_accepted");
            let lines = &page["payload"]["result"]["lines"];
            assert_eq!(lines.as_array().unwrap().len(), 256);
            assert_eq!(lines[0]["seq"], 0);
            assert_eq!(lines[255]["seq"], 255);

            send(
                &client,
                r#"{"id":"c_rev","command":"history","args":{"from_seq":5,"to_seq":1}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_rev")),
                ("invalid_arguments", REVERSED)
            );
            send(
                &client,
                r#"{"id":"c_latest","command":"history","args":{"from_seq":299}}"#,
            );
            let latest = response(&client, "c_latest");
            assert_eq!(kind(&latest), "command_accepted");
            let latest_seqs: Vec<u64> = latest["payload"]["result"]["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line["seq"].as_u64().unwrap())
                .collect();
            assert_eq!(latest_seqs, vec![299]);
            send(
                &client,
                r#"{"id":"c_past","command":"history","args":{"from_seq":300}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_past")),
                ("invalid_arguments", PAST)
            );
            send(
                &client,
                r#"{"id":"c_range","command":"history","args":{"from_seq":2,"to_seq":4}}"#,
            );
            let range = response(&client, "c_range");
            assert_eq!(kind(&range), "command_accepted");
            let got: Vec<u64> = range["payload"]["result"]["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line["seq"].as_u64().unwrap())
                .collect();
            assert_eq!(got, vec![2, 3, 4]);
            send(
                &client,
                r#"{"id":"c_one","command":"history","args":{"from_seq":4,"to_seq":4}}"#,
            );
            let one = response(&client, "c_one");
            let one: Vec<u64> = one["payload"]["result"]["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line["seq"].as_u64().unwrap())
                .collect();
            assert_eq!(one, vec![4]);
            assert!(
                matches!(inbox.try_recv(), Ok(Delivery::Prompt(_, _))),
                "the prompt was waiting unread while tools and history answered"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn inbox_commands_are_answered_only_on_the_connection_that_sent_them() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let dir = opened.dir.clone();
    let out = opened.out.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let sender = Client::connect(&socket).unwrap();
            let other = Client::connect(&socket).unwrap();
            subscribe(&sender, "c_a", "full");
            subscribe(&other, "c_b", "full");
            send(
                &sender,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
            );
            send(
                &sender,
                r#"{"id":"c_steer","command":"steer","args":{"content":[{"type":"text","text":"more"}]}}"#,
            );
            send(
                &sender,
                r#"{"id":"c_drop","command":"steer_drop","args":{"command_id":"c_steer"}}"#,
            );
            send(
                &sender,
                r#"{"id":"c_reply","command":"reply","args":{"request_id":"r_1","confirmed":true}}"#,
            );
            send(&sender, r#"{"id":"c_close","command":"close"}"#);
            assert_eq!(
                (0..5).map(|_| take(&inbox)).collect::<Vec<_>>(),
                vec![
                    "prompt hi".to_owned(),
                    "steer more".to_owned(),
                    "drop c_steer".to_owned(),
                    "reply r_1".to_owned(),
                    "close".to_owned(),
                ]
            );
            log.append(&step(), None, None).unwrap();
            let own = until(&sender, |line| line["seq"].as_u64() == Some(0));
            let ids: Vec<&str> = own.iter().filter_map(command_id).collect();
            assert_eq!(
                ids,
                vec!["c_prompt", "c_steer", "c_drop", "c_reply", "c_close"]
            );
            assert!(own.iter().all(|line| kind(line) != "command_rejected"));
            let seen = until(&other, |line| line["seq"].as_u64() == Some(0));
            assert!(
                seen.iter().all(|line| command_id(line).is_none()),
                "the other connection sees the step and none of the acknowledgements"
            );
            let printed = String::from_utf8(out.buf.lock().unwrap().clone()).unwrap();
            assert!(!printed.contains("c_prompt"));
            let file = fs::read_to_string(dir.join("events.jsonl")).unwrap();
            assert!(file.contains("step_started"));
            assert!(!file.contains("command_accepted"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn an_acknowledgement_dropped_uncalled_answers_closing() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
            );
            let Delivery::Prompt(_, ack) = inbox
                .recv_timeout(DEADLINE)
                .expect("the prompt is delivered")
            else {
                panic!("the prompt is delivered");
            };
            drop(ack);
            let line = until(&client, |line| kind(line) == "command_rejected");
            let line = line.last().unwrap();
            assert_eq!(rejection(line), ("closing", ENDED));
            assert_eq!(command_id(line), Some("c_prompt"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn close_delivers_fiber_exited_to_a_client_that_is_still_reading() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            log.append(&exited(), None, None).unwrap();
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = rx.recv_timeout(DEADLINE).expect("the client subscribed");
    let lines = until(&client, |line| kind(line) == "fiber_exited");
    assert_eq!(kind(lines.last().unwrap()), "fiber_exited");
    assert_eq!(lines.last().unwrap()["payload"]["exit_code"], 0);
    opened.close();
}

#[test]
fn close_releases_the_log_lock() {
    let opened = Opened::open(vec![]);
    opened.log.append(&turn(), None, None).unwrap();
    let sessions = opened.sessions.clone();
    let id = opened.id.clone();
    let clock = Arc::clone(&opened.clock);
    let dir = opened.dir.clone();
    let kept = opened.close();
    assert!(
        dir.is_dir(),
        "a session that got a turn keeps its directory"
    );
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    match Log::open(&sessions, id, timed) {
        Ok(_) => {}
        Err(error) => panic!("the lock was not released: {error}"),
    }
    drop(kept);
}

#[test]
fn cancel_with_no_turn_running_is_rejected_stale() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"cancel"}"#);
            let rejected = response(&client, "c_1");
            assert_eq!(
                rejection(&rejected),
                ("stale_request", "No turn is running.")
            );
            assert_eq!(command_id(&rejected), Some("c_1"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn cancel_with_a_turn_running_is_accepted_and_wakes_the_inbox() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| true), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"cancel"}"#);
            let accepted = response(&client, "c_1");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_1"));
            // The wake carries no meaning beyond waking the loop.
            assert!(matches!(
                inbox.recv_timeout(DEADLINE),
                Ok(Delivery::Cancelled)
            ));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn a_full_subscriber_after_a_queued_steer_gets_the_latest_steering_queue() {
    let opened = Opened::open(vec![]);
    opened.log.append(&step(), None, None).unwrap();
    opened
        .log
        .append(
            &Event::SteeringQueue(SteeringQueue {
                messages: vec![QueuedMessage {
                    content: vec![ContentPart::Text {
                        text: "later".into(),
                    }],
                    sender: Sender {
                        origin: Origin::Driver,
                        command_id: CommandId("c_later".into()),
                    },
                }],
            }),
            None,
            None,
        )
        .unwrap();
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            let lines = until(&client, |line| kind(line) == "steering_queue");
            let at = lines
                .iter()
                .position(|line| kind(line) == "steering_queue")
                .unwrap();
            // The queue arrives after the fold, which does not have it.
            assert!(
                lines[..at]
                    .iter()
                    .any(|line| line["seq"].as_u64() == Some(0))
            );
            assert_eq!(
                lines[at]["payload"]["messages"][0]["content"][0]["text"],
                "later"
            );
            assert_eq!(lines[at]["payload"]["messages"][0]["command_id"], "c_later");
            Ok(())
        })
        .unwrap();
    opened.close();
}

fn shell_line(id: &str, command: &str) -> String {
    format!(r#"{{"id":"{id}","command":"shell","args":{{"command":"{command}"}}}}"#)
}

fn ended(code: i32, text: &str) -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
        process: Some(Process {
            exit_code: Some(code),
            signal: None,
            timed_out: false,
        }),
        ..Output::default()
    }
}

fn fixed(output: Output, bound: Bound) -> Arc<dyn Tool> {
    Arc::new(Fixed {
        output,
        bound,
        ran: Arc::new(AtomicBool::new(false)),
    })
}

fn fixed_ran(output: Output, bound: Bound, ran: Arc<AtomicBool>) -> Arc<dyn Tool> {
    Arc::new(Fixed { output, bound, ran })
}

struct Fixed {
    output: Output,
    bound: Bound,
    ran: Arc<AtomicBool>,
}

impl Tool for Fixed {
    fn definition(&self) -> ToolDefinition {
        shell_definition()
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        // True when the command started with no cancel on it.
        self.ran.store(!cancel.is_cancelled(), Ordering::Relaxed);
        self.output.clone()
    }

    fn bound(&self) -> Bound {
        self.bound
    }
}

fn shell_definition() -> ToolDefinition {
    ToolDefinition {
        name: "shell".to_owned(),
        description: "test".to_owned(),
        input_schema: Value::Object(Map::new()),
        deferred: false,
    }
}

struct Flag {
    ready: Mutex<bool>,
    cv: Condvar,
}

impl Wake for Flag {
    fn wake(&self) {
        *self.ready.lock().expect("the flag lock") = true;
        self.cv.notify_all();
    }
}

struct Blocks {
    entered: Mutex<Option<mpsc::Sender<()>>>,
}

impl Tool for Blocks {
    fn definition(&self) -> ToolDefinition {
        shell_definition()
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let flag = Arc::new(Flag {
            ready: Mutex::new(false),
            cv: Condvar::new(),
        });
        let wake: Arc<dyn Wake> = flag.clone();
        cancel.subscribe(Arc::downgrade(&wake));
        if let Some(sender) = self.entered.lock().expect("the entered lock").take() {
            sender.send(()).expect("the test is waiting");
        }
        if !cancel.is_cancelled() {
            let guard = flag.ready.lock().expect("the flag lock");
            let _wait = flag
                .cv
                .wait_timeout_while(guard, DEADLINE, |_| !cancel.is_cancelled());
        }
        assert!(
            cancel.is_cancelled(),
            "timed out waiting for the shell to be cancelled"
        );
        Output {
            content: vec![ContentPart::Text {
                text: "Cancelled and stopped.\n".to_owned(),
            }],
            process: Some(Process {
                exit_code: None,
                signal: None,
                timed_out: false,
            }),
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

/// Longer than [`DEADLINE`], so a close that never cancels the shell fails
/// the test's own wait rather than this tool's timeout.
const SHELL_LIMIT: Duration = Duration::from_secs(30);

struct Hangs {
    entered: Mutex<Option<mpsc::Sender<()>>>,
    saw_cancel: Arc<AtomicBool>,
}

impl Tool for Hangs {
    fn definition(&self) -> ToolDefinition {
        shell_definition()
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let flag = Arc::new(Flag {
            ready: Mutex::new(false),
            cv: Condvar::new(),
        });
        let wake: Arc<dyn Wake> = flag.clone();
        cancel.subscribe(Arc::downgrade(&wake));
        if let Some(sender) = self.entered.lock().expect("the entered lock").take() {
            sender.send(()).expect("the test is waiting");
        }
        if !cancel.is_cancelled() {
            let guard = flag.ready.lock().expect("the flag lock");
            let _wait = flag
                .cv
                .wait_timeout_while(guard, SHELL_LIMIT, |_| !cancel.is_cancelled());
        }
        self.saw_cancel
            .store(cancel.is_cancelled(), Ordering::Relaxed);
        Output {
            content: vec![ContentPart::Text {
                text: "Cancelled and stopped.\n".to_owned(),
            }],
            process: Some(Process {
                exit_code: None,
                signal: None,
                timed_out: false,
            }),
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

fn no_durable(dir: &Path) {
    assert!(
        log::read(dir).unwrap().is_empty(),
        "a driver shell writes nothing to the log"
    );
}

#[test]
fn shell_before_subscribe_is_not_subscribed() {
    let opened = Opened::open(vec![]);
    opened
        .session
        .shell(fixed(ended(0, "hi\n"), Bound::DEFAULT));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            send(&client, &shell_line("c_1", "echo hi"));
            let line = response(&client, "c_1");
            assert_eq!(rejection(&line), ("not_subscribed", NOT_SUBSCRIBED));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_with_no_tool_is_unknown() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_1", "echo hi"));
            let line = response(&client, "c_1");
            assert_eq!(
                rejection(&line),
                ("unknown_command", "`shell` is not built in this Fiber yet.")
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_exit_zero_stays_on_the_sending_connection() {
    let opened = Opened::open(vec![]);
    opened
        .session
        .shell(fixed(ended(0, "hi\n"), Bound::DEFAULT));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            let other = Client::connect(&socket).unwrap();
            subscribe(&other, "c_other", "full");
            send(&client, &shell_line("c_1", "echo hi"));
            let line = response(&client, "c_1");
            let answered = &line["payload"]["result"];
            assert_eq!(answered["output"], "hi\n");
            assert_eq!(answered["process"]["exit_code"], 0);
            assert_eq!(answered["process"]["timed_out"], false);
            assert!(answered.get("artifact").is_none());
            send(&other, r#"{"id":"c_tools","command":"tools"}"#);
            let seen = until(&other, |line| command_id(line) == Some("c_tools"));
            assert!(
                seen.iter().all(|line| command_id(line) != Some("c_1")),
                "the other client sees the shell answer"
            );
            send(&client, r#"{"id":"c_cancel","command":"cancel"}"#);
            let rejected = response(&client, "c_cancel");
            assert_eq!(
                rejection(&rejected),
                ("stale_request", "No turn is running.")
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_nonzero_exit_is_accepted() {
    let opened = Opened::open(vec![]);
    opened.session.shell(fixed(ended(2, "no"), Bound::DEFAULT));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_1", "false"));
            let line = response(&client, "c_1");
            assert_eq!(kind(&line), "command_accepted");
            assert_eq!(line["payload"]["result"]["output"], "no");
            assert_eq!(line["payload"]["result"]["process"]["exit_code"], 2);
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_output_over_the_cap_is_cut_under_a_minted_name() {
    let opened = Opened::open(vec![]);
    let full = "0123456789abcdefghij";
    opened
        .session
        .shell(fixed(ended(0, full), Bound { start: 4, end: 4 }));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"../x","command":"shell","args":{"command":"big"}}"#,
            );
            let line = response(&client, "../x");
            let artifact = line["payload"]["result"]["artifact"]
                .as_str()
                .expect("the cut output names an artifact");
            assert!(artifact.starts_with("artifacts/o_"), "{artifact}");
            assert!(!artifact.contains("../x"), "{artifact}");
            let saved = fs::read(dir.join(artifact)).unwrap();
            assert_eq!(saved, full.as_bytes());
            let output = line["payload"]["result"]["output"].as_str().unwrap();
            assert_ne!(output, full);
            assert!(output.contains("bytes cut"), "{output}");
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_is_answered_while_a_turn_sits_in_the_inbox() {
    let opened = Opened::open(vec![]);
    opened
        .session
        .shell(fixed(ended(0, "hi\n"), Bound::DEFAULT));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"wait"}]}}"#,
            );
            send(&client, &shell_line("c_shell", "echo hi"));
            let line = response(&client, "c_shell");
            assert_eq!(line["payload"]["result"]["output"], "hi\n");
            let delivery = inbox.recv_timeout(DEADLINE).expect("the prompt is still queued");
            assert!(matches!(delivery, Delivery::Prompt(..)));
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn cancel_from_another_connection_stops_a_driver_shell() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let opened = Opened::open(vec![]);
    opened.session.shell(Arc::new(Blocks {
        entered: Mutex::new(Some(entered_tx)),
    }));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            let other = Client::connect(&socket).unwrap();
            subscribe(&other, "c_other", "full");
            send(&client, &shell_line("c_shell", "sleep 60"));
            entered_rx
                .recv_timeout(DEADLINE)
                .expect("the shell is running");
            send(&other, r#"{"id":"c_cancel","command":"cancel"}"#);
            let accepted = response(&other, "c_cancel");
            assert_eq!(kind(&accepted), "command_accepted");
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "a shell cancel does not wake the loop"
            );
            let line = response(&client, "c_shell");
            assert_eq!(
                line["payload"]["result"]["output"],
                "Cancelled and stopped.\n"
            );
            assert!(
                line["payload"]["result"]["process"]
                    .get("exit_code")
                    .is_none()
            );
            assert_eq!(line["payload"]["result"]["process"]["timed_out"], false);
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn close_cancels_a_shell_blocked_in_its_tool() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let opened = Opened::open(vec![]);
    opened.session.shell(Arc::new(Hangs {
        entered: Mutex::new(Some(entered_tx)),
        saw_cancel: Arc::clone(&saw_cancel),
    }));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_shell", "sleep 60"));
            entered_rx
                .recv_timeout(DEADLINE)
                .expect("the shell is running");
            Ok(())
        })
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let session = opened.session;
    let log = opened.log;
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = done_tx.send(()) {}
    });
    done_rx
        .recv_timeout(DEADLINE)
        .expect("close returned without the shell's timeout");
    assert!(
        saw_cancel.load(Ordering::Relaxed),
        "the tool did not see its cancel"
    );
}

#[test]
fn a_shell_nobody_cancelled_starts_uncancelled() {
    let opened = Opened::open(vec![]);
    let ran = Arc::new(AtomicBool::new(false));
    opened.session.shell(fixed_ran(
        ended(0, "hi\n"),
        Bound::DEFAULT,
        Arc::clone(&ran),
    ));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_1", "echo hi"));
            response(&client, "c_1");
            assert!(ran.load(Ordering::Relaxed), "the tool saw a cancel");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_send_true_is_rejected_and_does_not_run() {
    let opened = Opened::open(vec![]);
    let ran = Arc::new(AtomicBool::new(false));
    opened.session.shell(fixed_ran(
        ended(0, "hi\n"),
        Bound::DEFAULT,
        Arc::clone(&ran),
    ));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_2","command":"shell","args":{"command":"echo hi","send":true}}"#,
            );
            let line = response(&client, "c_2");
            assert_eq!(
                rejection(&line),
                (
                    "invalid_arguments",
                    "`send` is not built in this Fiber yet."
                )
            );
            assert!(!ran.load(Ordering::Relaxed));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_without_a_process_is_invalid_arguments() {
    let opened = Opened::open(vec![]);
    opened.session.shell(fixed(
        Output {
            error: Some(Failure {
                code: ErrorCode::InvalidArguments,
                message: "Give one command.".to_owned(),
                retry_after: None,
                provider: None,
            }),
            ..Output::default()
        },
        Bound::DEFAULT,
    ));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_1", ""));
            let line = response(&client, "c_1");
            assert_eq!(rejection(&line), ("invalid_arguments", "Give one command."));
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}
