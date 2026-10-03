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
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::events::{
    ContextAdded, Empty, Event, ExtensionsLoaded, FiberExited, InputItem, LoadedExtension, Notice,
    SessionState, SessionStatus, ToolInfo, ToolSource, ToolState, TurnStarted,
};
use contract::inbox::{Delivery, Message};
use contract::shapes::{ContentPart, Origin, Sender, Tokens, Usage};
use contract::{CommandId, ErrorCode, SessionId};
use doors::{Session, mint};
use fakes::Client;
use fakes::clock::FakeClock;
use log::Log;
use serde_json::Value;

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

    fn wait_for(&self, needle: &str) {
        let guard = self.buf.lock().unwrap();
        let (guard, _) = self
            .ready
            .wait_timeout_while(guard, DEADLINE, |buf| {
                !String::from_utf8_lossy(buf).contains(needle)
            })
            .unwrap();
        assert!(
            String::from_utf8_lossy(&guard).contains(needle),
            "stdout never wrote {needle}"
        );
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

fn context(text: &str) -> Event {
    Event::ContextAdded(ContextAdded {
        text: text.to_owned(),
        extension: "e".into(),
        hook: "turn_start".into(),
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
    match inbox.recv().expect("a delivery") {
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
    }
}

#[test]
fn a_bad_line_is_malformed_and_only_a_string_id_is_echoed() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), move |_inbox| {
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
            let mut buf = Vec::new();
            BufReader::new(raw).read_until(b'\n', &mut buf).unwrap();
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
        .run(Vec::new(), move |_inbox| {
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
        .run(Vec::new(), move |_inbox| {
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
                "message", "cancel", "job_stop", "background", "reload", "model", "credential",
                "name",
                "handoff", "rewind", "shell", "command",
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
        .run(Vec::new(), move |_inbox| {
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
        .run(Vec::new(), move |_inbox| {
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
        .run(Vec::new(), move |_inbox| {
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
        .run(Vec::new(), move |_inbox| {
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
        .run(Vec::new(), move |inbox| {
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
        .run(Vec::new(), move |inbox| {
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
        .run(Vec::new(), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
            );
            let Delivery::Prompt(_, ack) = inbox.recv().unwrap() else {
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
fn a_slow_client_does_not_block_appends_and_keeps_every_durable_line() {
    let opened = Opened::open(vec![]);
    for _ in 0..3 {
        opened.log.append(&step(), None, None).unwrap();
    }
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let out = opened.out.clone();
    opened
        .session
        .run(Vec::new(), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            client.slow(true);
            send(
                &client,
                r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
            );
            out.wait_for("\"kind\":\"clients\"");
            let noise = notice(&"n".repeat(4_000));
            for _ in 0..1_200 {
                log.append(&noise, None, None).unwrap();
            }
            for _ in 0..3 {
                log.append(&step(), None, None).unwrap();
            }
            client.slow(false);
            let lines = until(&client, |line| line["seq"].as_u64() == Some(5));
            assert_eq!(seqs(&lines), vec![0, 1, 2, 3, 4, 5]);
            let notices = lines.iter().filter(|line| kind(line) == "notice").count();
            assert!(
                notices < 1_200,
                "a slow client drops ephemeral lines, got {notices}"
            );
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
        .run(Vec::new(), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            log.append(&exited(), None, None).unwrap();
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = rx.recv().unwrap();
    opened.close();
    let lines = until(&client, |line| kind(line) == "fiber_exited");
    assert_eq!(kind(lines.last().unwrap()), "fiber_exited");
    assert_eq!(lines.last().unwrap()["payload"]["exit_code"], 0);
}

#[test]
fn a_client_that_never_reads_does_not_hold_close_past_the_grace() {
    let opened = Opened::open(vec![]);
    let clock = Arc::clone(&opened.clock);
    let socket = opened.socket.clone();
    let (tx, rx) = mpsc::channel();
    let wide = context(&"z".repeat(4_000));
    for _ in 0..20 {
        opened.log.append(&wide, None, None).unwrap();
    }
    opened
        .session
        .run(Vec::new(), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.slow(true);
            send(
                &client,
                r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
            );
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
            );
            let Delivery::Prompt(_, ack) = inbox.recv().unwrap() else {
                panic!("subscribe finished and the prompt was delivered");
            };
            drop(ack);
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = rx.recv().unwrap();
    let until = clock.now() + Duration::from_secs(2);
    let (done_tx, done_rx) = mpsc::channel();
    let Opened {
        session,
        log,
        _temp,
        ..
    } = opened;
    thread::spawn(move || {
        session.close(log);
        done_tx.send(()).unwrap();
    });
    assert!(
        clock.await_parked(until, DEADLINE),
        "close waits for a writer that is not reading"
    );
    clock.advance(Duration::from_secs(2));
    done_rx
        .recv_timeout(DEADLINE)
        .expect("close returns once the grace has passed");
    drop(client);
    drop(_temp);
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
