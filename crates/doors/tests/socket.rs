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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{
    CacheLifetime, CommandInfo, Empty, Event, ExtensionsLoaded, FiberExited, InputItem,
    LoadedExtension, Notice, PreambleBuilt, PreambleReason, QueuedMessage, SentTool, SessionState,
    SessionStatus, SteeringQueue, ToolInfo, ToolSource, ToolState, TurnStarted, UsageRecorded,
};
use contract::inbox::{Delivery, Message, Rejection};
use contract::jobs::{Foreground, Jobs, Opening, Stop};
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Failure, Origin, Process, Sender, Tokens, Usage};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{ActionId, CommandId, ErrorCode, GenerationId, SessionId};
use doors::{Session, mint};
use fakes::Client;
use fakes::clock::FakeClock;
use fakes::jobs::FakeJobs;
use log::Log;
use serde_json::{Map, Value};

/// A hang bound for one line, the same order as the log crate's watcher tests.
const DEADLINE: Duration = Duration::from_secs(10);

/// One deadline for a whole `until` wait. The busiest test waits on it seven
/// times and closes its session once under [`DEADLINE`]: 7 x 6 + 10 = 52 s,
/// at most half of nextest's 120 s kill (`docs/testing.md`, "Waits and
/// timeouts").
const UNTIL: Duration = Duration::from_secs(6);

const MALFORMED: &str = "A command is one JSON object per line, with a string `id` and `command`.";
const NOT_SUBSCRIBED: &str = "Send `subscribe` first.";
const ALREADY: &str = "This connection is already subscribed at this level.";
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

/// Lines up to and including the first one `done` accepts, read under one
/// [`UNTIL`] deadline for the whole wait, not one per line. Fails naming the
/// wait when it passes.
fn until(client: &Client, mut done: impl FnMut(&Value) -> bool + Send) -> Vec<Value> {
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        let stop = &stop;
        scope.spawn(move || {
            let mut lines = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                let Some(line) = client.recv(UNTIL) else {
                    break;
                };
                let finished = done(&line);
                lines.push(line);
                if finished {
                    if let Ok(()) = tx.send(lines) {}
                    return;
                }
            }
        });
        let got = rx.recv_timeout(UNTIL);
        stop.store(true, Ordering::SeqCst);
        got.expect("the awaited line arrived within one deadline for the whole wait")
    })
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

fn kinds(lines: &[Value]) -> Vec<&str> {
    lines.iter().map(kind).collect()
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
        project: "-w".into(),
        clients: 0,
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
                command_id: Some(CommandId("c_1".into())),
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
        Delivery::Handoff(id, args, ack) => {
            ack.0(Ok(None));
            format!("handoff {} {:?}", id.0, args.instructions)
        }
        Delivery::Model(args, ack) => {
            ack.0(Ok(None));
            format!("model {}", args.model)
        }
        Delivery::Close(ack) => {
            ack.0(Ok(None));
            "close".to_owned()
        }
        Delivery::Cancelled => panic!("a wake arrives as a delivery"),
        Delivery::Job(_)
        | Delivery::JobLine(_)
        | Delivery::Interaction(_)
        | Delivery::Resolved(..)
        | Delivery::ExtensionExec(_)
        | Delivery::ExtensionLog(_) => {
            panic!("no job runs here")
        }
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
                r#"{"id":"c_again","command":"subscribe","args":{"level":"full"}}"#,
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
                "message",
                "reload",
                "credential",
                "name",
                "rewind",
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
fn tools_give_tokens_from_the_first_request_after_each_preamble() {
    fn definition() -> Map<String, Value> {
        serde_json::from_value(serde_json::json!({"type": "object"})).unwrap()
    }

    fn preamble(reason: PreambleReason, system_prompt: &str) -> Event {
        Event::PreambleBuilt(PreambleBuilt {
            reason,
            model: "fake/m".into(),
            context_window: 200_000,
            trigger_at: None,
            thinking: None,
            tool_choice: "auto".into(),
            cache_lifetime: CacheLifetime::FiveMinutes,
            credential: None,
            system_prompt: system_prompt.into(),
            tools: vec![SentTool {
                name: "read".into(),
                registered_by: "builtin".into(),
                deferred: false,
                definition: definition(),
            }],
            replaced: Vec::new(),
        })
    }

    fn usage(
        generation: &str,
        input: u64,
        cache_write: Vec<(&str, u64)>,
        input_bytes: u64,
        input_media: Option<bool>,
        extension: Option<&str>,
        origin: Option<SessionId>,
    ) -> Event {
        Event::UsageRecorded(UsageRecorded {
            generation_id: GenerationId(generation.into()),
            model: "fake/m".into(),
            tokens: Tokens {
                input,
                cache_read: 0,
                cache_write: cache_write
                    .into_iter()
                    .map(|(lifetime, written)| (lifetime.to_owned(), written))
                    .collect(),
                output: 3,
            },
            web_searches: None,
            cost: None,
            subscription: None,
            extension: extension.map(str::to_owned),
            origin_session_id: origin,
            input_bytes,
            input_media,
        })
    }

    fn started() -> Event {
        Event::AssistantMessageStarted(Empty {})
    }

    fn tools_of(answer: &Value) -> &Vec<Value> {
        answer["payload"]["result"]["tools"].as_array().unwrap()
    }

    let opened = Opened::open(vec![tool()]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");

            send(&client, r#"{"id":"c_t0","command":"tools"}"#);
            let t0 = response(&client, "c_t0");
            assert_eq!(kind(&t0), "command_accepted", "{t0}");
            assert_eq!(tools_of(&t0).len(), 1);
            assert_eq!(tools_of(&t0)[0]["bytes"], 12);
            assert!(tools_of(&t0)[0].get("tokens").is_none(), "{t0}");

            log.append(&preamble(PreambleReason::Start, "system-a"), None, None)
                .unwrap();
            send(&client, r#"{"id":"c_t1","command":"tools"}"#);
            let t1 = response(&client, "c_t1");
            assert_eq!(tools_of(&t1)[0]["bytes"], 12);
            assert!(tools_of(&t1)[0].get("tokens").is_none(), "{t1}");

            let a1 = Some(ActionId("a_1".into()));
            log.append(&started(), None, a1.clone()).unwrap();
            log.append(
                &usage("g_x", 10, vec![("5m", 1000)], 500, None, None, None),
                None,
                None,
            )
            .unwrap();
            log.append(
                &usage("g_x", 10, vec![("5m", 1000)], 500, None, Some("x"), None),
                None,
                a1.clone(),
            )
            .unwrap();
            log.append(
                &usage(
                    "g_x",
                    10,
                    vec![("5m", 1000)],
                    500,
                    None,
                    None,
                    Some(SessionId("s_other".into())),
                ),
                None,
                a1.clone(),
            )
            .unwrap();
            log.append(
                &usage("g_x", 10, vec![("5m", 1000)], 500, None, None, None),
                None,
                Some(ActionId("a_9".into())),
            )
            .unwrap();
            log.append(
                &usage("g_x", 10, vec![("5m", 5000)], 500, Some(true), None, None),
                None,
                a1.clone(),
            )
            .unwrap();
            send(&client, r#"{"id":"c_t2","command":"tools"}"#);
            let t2 = response(&client, "c_t2");
            assert_eq!(tools_of(&t2)[0]["bytes"], 12);
            assert!(tools_of(&t2)[0].get("tokens").is_none(), "{t2}");

            log.append(
                &usage("g_1", 10, vec![("5m", 90)], 500, None, None, None),
                None,
                a1.clone(),
            )
            .unwrap();
            send(&client, r#"{"id":"c_t3","command":"tools"}"#);
            let t3 = response(&client, "c_t3");
            let expected = 12 * 100 / 500;
            assert!(expected >= 2 && (12 * 100) % 500 != 0);
            assert_eq!(tools_of(&t3)[0]["bytes"], 12);
            assert_eq!(
                tools_of(&t3)[0]["tokens"].as_u64().unwrap(),
                expected,
                "{t3}"
            );

            log.append(
                &usage("g_1", 10, vec![("5m", 555)], 500, None, None, None),
                None,
                a1.clone(),
            )
            .unwrap();
            send(&client, r#"{"id":"c_t4","command":"tools"}"#);
            let t4 = response(&client, "c_t4");
            assert_eq!(tools_of(&t4)[0]["bytes"], 12);
            assert_eq!(
                tools_of(&t4)[0]["tokens"].as_u64().unwrap(),
                expected,
                "{t4}"
            );

            log.append(&preamble(PreambleReason::Reload, "system-b"), None, None)
                .unwrap();
            send(&client, r#"{"id":"c_t5","command":"tools"}"#);
            let t5 = response(&client, "c_t5");
            assert_eq!(tools_of(&t5)[0]["bytes"], 12);
            assert!(tools_of(&t5)[0].get("tokens").is_none(), "{t5}");

            log.append(
                &usage("g_1", 10, vec![("5m", 1000)], 500, None, None, None),
                None,
                a1.clone(),
            )
            .unwrap();
            send(&client, r#"{"id":"c_t6","command":"tools"}"#);
            let t6 = response(&client, "c_t6");
            assert_eq!(tools_of(&t6)[0]["bytes"], 12);
            assert!(tools_of(&t6)[0].get("tokens").is_none(), "{t6}");

            log.append(&started(), None, Some(ActionId("a_2".into())))
                .unwrap();
            log.append(
                &usage("g_2", 0, Vec::new(), 500, None, None, None),
                None,
                Some(ActionId("a_2".into())),
            )
            .unwrap();
            send(&client, r#"{"id":"c_t7","command":"tools"}"#);
            let t7 = response(&client, "c_t7");
            assert_eq!(tools_of(&t7)[0]["bytes"], 12);
            assert_eq!(tools_of(&t7)[0]["tokens"], 0, "{t7}");
            Ok(())
        })
        .unwrap();
    opened.close();
}

/// Overwrites line `index` (from 0) of the session's log in place with bytes
/// that do not parse, keeping its length.
fn corrupt(dir: &Path, index: usize) {
    use std::os::unix::fs::FileExt;
    let path = dir.join("events.jsonl");
    let whole = fs::read(&path).unwrap();
    let start: usize = whole
        .split_inclusive(|b| *b == b'\n')
        .take(index)
        .map(<[u8]>::len)
        .sum();
    let len = whole[start..].iter().position(|b| *b == b'\n').unwrap();
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all_at(&vec![b'x'; len], u64::try_from(start).unwrap())
        .unwrap();
}

#[test]
fn history_reads_only_its_window_of_the_log() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            for _ in 0..8 {
                log.append(&step(), None, None).unwrap();
            }
            // Bad lines before and after the window.
            corrupt(&dir, 1);
            corrupt(&dir, 6);
            let client = Client::connect(&socket).unwrap();
            // A full subscribe would read the bad line in its first page.
            subscribe(&client, "c_sub", "summary");
            send(
                &client,
                r#"{"id":"c_after","command":"history","args":{"from_seq":2,"to_seq":5}}"#,
            );
            let after = response(&client, "c_after");
            assert_eq!(kind(&after), "command_accepted", "{after}");
            let got: Vec<u64> = after["payload"]["result"]["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line["seq"].as_u64().unwrap())
                .collect();
            assert_eq!(got, vec![2, 3, 4, 5]);
            send(
                &client,
                r#"{"id":"c_over","command":"history","args":{"from_seq":0,"to_seq":4}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_over")),
                ("invalid_arguments", UNFIT)
            );
            // The checks before the read need no parse.
            send(
                &client,
                r#"{"id":"c_past","command":"history","args":{"from_seq":8}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_past")),
                ("invalid_arguments", PAST)
            );
            send(
                &client,
                r#"{"id":"c_rev","command":"history","args":{"from_seq":3,"to_seq":1}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_rev")),
                ("invalid_arguments", REVERSED)
            );
            // A window cut short by `to_seq` reads no further.
            send(
                &client,
                r#"{"id":"c_first","command":"history","args":{"from_seq":0,"to_seq":0}}"#,
            );
            let first = response(&client, "c_first");
            assert_eq!(kind(&first), "command_accepted", "{first}");
            assert_eq!(first["payload"]["result"]["lines"][0]["seq"], 0);
            assert_eq!(
                first["payload"]["result"]["lines"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
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
            send(
                &sender,
                r#"{"id":"c_handoff","command":"handoff","args":{"instructions":"focus on tests"}}"#,
            );
            send(&sender, r#"{"id":"c_bare","command":"handoff","args":{}}"#);
            send(&sender, r#"{"id":"c_noargs","command":"handoff"}"#);
            send(&sender, r#"{"id":"c_close","command":"close"}"#);
            assert_eq!(
                (0..8).map(|_| take(&inbox)).collect::<Vec<_>>(),
                vec![
                    "prompt hi".to_owned(),
                    "steer more".to_owned(),
                    "drop c_steer".to_owned(),
                    "reply r_1".to_owned(),
                    "handoff c_handoff Some(\"focus on tests\")".to_owned(),
                    "handoff c_bare None".to_owned(),
                    "handoff c_noargs None".to_owned(),
                    "close".to_owned(),
                ]
            );
            log.append(&step(), None, None).unwrap();
            let own = until(&sender, |line| line["seq"].as_u64() == Some(0));
            let ids: Vec<&str> = own.iter().filter_map(command_id).collect();
            assert_eq!(
                ids,
                vec![
                    "c_prompt",
                    "c_steer",
                    "c_drop",
                    "c_reply",
                    "c_handoff",
                    "c_bare",
                    "c_noargs",
                    "c_close"
                ]
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
fn model_arrives_as_delivery_with_its_args_and_its_rejection_stays_put() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let sender = Client::connect(&socket).unwrap();
            let other = Client::connect(&socket).unwrap();
            let mut own = vec![subscribe(&sender, "c_a", "full")];
            let mut other_stream = vec![subscribe(&other, "c_b", "full")];
            send(
                &sender,
                r#"{"id":"c_m1","command":"model","args":{"model":"fake/n","thinking":"high"}}"#,
            );
            match inbox
                .recv_timeout(DEADLINE)
                .expect("the model is delivered")
            {
                Delivery::Model(args, ack) => {
                    assert_eq!(args.model, "fake/n");
                    assert_eq!(args.thinking.as_deref(), Some("high"));
                    ack.0(Ok(None));
                }
                Delivery::Prompt(..)
                | Delivery::Steer(..)
                | Delivery::SteerDrop(..)
                | Delivery::Handoff(..)
                | Delivery::Reply(..)
                | Delivery::Close(_)
                | Delivery::Job(_)
                | Delivery::JobLine(_)
                | Delivery::Interaction(_)
                | Delivery::Resolved(..)
                | Delivery::ExtensionExec(_)
                | Delivery::ExtensionLog(_)
                | Delivery::Cancelled => panic!("the model arrives as a model"),
            }
            let accepted_lines = until(&sender, |line| command_id(line) == Some("c_m1"));
            assert_eq!(kind(accepted_lines.last().unwrap()), "command_accepted");
            own.extend(accepted_lines);
            send(
                &sender,
                r#"{"id":"c_m2","command":"model","args":{"model":"fake/nope"}}"#,
            );
            match inbox
                .recv_timeout(DEADLINE)
                .expect("the second model is delivered")
            {
                Delivery::Model(args, ack) => {
                    assert_eq!(args.model, "fake/nope");
                    ack.0(Err(Rejection {
                        code: ErrorCode::InvalidArguments,
                        message: "no such model".into(),
                    }));
                }
                Delivery::Prompt(..)
                | Delivery::Steer(..)
                | Delivery::SteerDrop(..)
                | Delivery::Handoff(..)
                | Delivery::Reply(..)
                | Delivery::Close(_)
                | Delivery::Job(_)
                | Delivery::JobLine(_)
                | Delivery::Interaction(_)
                | Delivery::Resolved(..)
                | Delivery::ExtensionExec(_)
                | Delivery::ExtensionLog(_)
                | Delivery::Cancelled => panic!("the model arrives as a model"),
            }
            let rejected_lines = until(&sender, |line| command_id(line) == Some("c_m2"));
            assert_eq!(
                rejection(rejected_lines.last().unwrap()),
                ("invalid_arguments", "no such model")
            );
            own.extend(rejected_lines);
            // The other connection sees neither acknowledgement: both stay
            // on the connection that sent them.
            send(&other, r#"{"id":"c_tools","command":"tools"}"#);
            let seen = until(&other, |line| command_id(line) == Some("c_tools"));
            assert!(
                seen.iter()
                    .all(|line| !matches!(command_id(line), Some("c_m1" | "c_m2"))),
                "the other connection sees no model acknowledgement"
            );
            assert_eq!(kind(seen.last().unwrap()), "command_accepted");
            other_stream.extend(seen);
            assert_eq!(
                kinds(&own),
                [
                    "command_accepted",
                    "clients",
                    "clients",
                    "command_accepted",
                    "command_rejected"
                ]
            );
            assert_eq!(
                kinds(&other_stream),
                ["command_accepted", "clients", "command_accepted"]
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn model_without_args_is_unfit_and_before_subscribe_is_not_subscribed() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            let mut stream = Vec::new();
            send(
                &client,
                r#"{"id":"c_early","command":"model","args":{"model":"fake/n"}}"#,
            );
            let early = next(&client);
            assert_eq!(rejection(&early), ("not_subscribed", NOT_SUBSCRIBED));
            stream.push(early);
            stream.push(subscribe(&client, "c_sub", "full"));
            send(&client, r#"{"id":"c_bare","command":"model"}"#);
            let bare_lines = until(&client, |line| command_id(line) == Some("c_bare"));
            assert_eq!(
                rejection(bare_lines.last().unwrap()),
                ("invalid_arguments", UNFIT)
            );
            stream.extend(bare_lines);
            send(&client, r#"{"id":"c_empty","command":"model","args":{}}"#);
            let empty_lines = until(&client, |line| command_id(line) == Some("c_empty"));
            assert_eq!(
                rejection(empty_lines.last().unwrap()),
                ("invalid_arguments", UNFIT)
            );
            stream.extend(empty_lines);
            assert_eq!(
                kinds(&stream),
                [
                    "command_rejected",
                    "command_accepted",
                    "clients",
                    "command_rejected",
                    "command_rejected"
                ]
            );
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
                        command_id: Some(CommandId("c_later".into())),
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
        hosted: None,
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
            // The shell answers before it leaves the running list, so a
            // `cancel` right after the answer may still be accepted. The
            // retries run on a thread so the whole wait has one deadline.
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                for attempt in 0.. {
                    let id = format!("c_cancel_{attempt}");
                    send(&client, &format!(r#"{{"id":"{id}","command":"cancel"}}"#));
                    let line = response(&client, &id);
                    if kind(&line) != "command_accepted" {
                        drop(tx.send(line));
                        return;
                    }
                }
            });
            let rejected = rx
                .recv_timeout(DEADLINE)
                .expect("the finished shell left the running list");
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

/// The answer to `id`, or none when a line is not read within [`DEADLINE`].
/// Lets a test close its session before it asserts.
fn answer_within(client: &Client, id: &str) -> Option<Value> {
    answers_within(client, &[id]).pop().flatten()
}

/// The answers to `ids` in the order named, whatever order they arrive in.
/// Reading stops once each is answered, or when a line is not read within
/// [`DEADLINE`], which leaves the rest none.
fn answers_within(client: &Client, ids: &[&str]) -> Vec<Option<Value>> {
    let mut answers: Vec<Option<Value>> = vec![None; ids.len()];
    while answers.iter().any(Option::is_none) {
        let Some(line) = client.recv(DEADLINE) else {
            break;
        };
        if let Some(slot) = ids
            .iter()
            .position(|id| command_id(&line) == Some(*id))
            .and_then(|at| answers.get_mut(at))
        {
            *slot = Some(line);
        }
    }
    answers
}

#[test]
fn cancel_on_the_same_connection_stops_a_driver_shell() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let opened = Opened::open(vec![]);
    opened.session.shell(Arc::new(Hangs {
        entered: Mutex::new(Some(entered_tx)),
        saw_cancel: Arc::clone(&saw_cancel),
    }));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    let mut seen = None;
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_shell", "sleep 60"));
            entered_rx
                .recv_timeout(DEADLINE)
                .expect("the shell is running");
            send(&client, r#"{"id":"c_cancel","command":"cancel"}"#);
            // The cancel wakes the shell before its own answer is queued,
            // so either answer can come first. On a reader blocked in the
            // shell, neither would come before close.
            let mut answers = answers_within(&client, &["c_cancel", "c_shell"]).into_iter();
            let (cancel, shell) = (answers.next().flatten(), answers.next().flatten());
            let woken = !matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty));
            no_durable(&dir);
            seen = Some((cancel, woken, shell, client));
            Ok(())
        })
        .unwrap();
    opened.close();
    let (cancel, woken, shell, _client) = seen.expect("the client ran");
    let cancel = cancel.expect("cancel on the shell's own connection was answered");
    assert_eq!(kind(&cancel), "command_accepted", "{cancel}");
    assert!(!woken, "a shell cancel does not wake the loop");
    assert!(
        saw_cancel.load(Ordering::Relaxed),
        "the tool saw its cancel"
    );
    let line = shell.expect("the cancelled shell was answered");
    assert_eq!(kind(&line), "command_accepted", "{line}");
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
}

/// Hangs on its first call, as [`Hangs`]; answers every later one at once.
struct HangsOnce {
    hangs: Hangs,
    calls: AtomicUsize,
}

impl Tool for HangsOnce {
    fn definition(&self) -> ToolDefinition {
        shell_definition()
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, emit: &dyn Emit) -> Output {
        if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
            self.hangs.run(arguments, cancel, emit)
        } else {
            ended(0, "hi\n")
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

#[test]
fn a_connection_reads_on_while_its_driver_shell_runs() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let opened = Opened::open(vec![tool()]);
    opened.session.shell(Arc::new(HangsOnce {
        hangs: Hangs {
            entered: Mutex::new(Some(entered_tx)),
            saw_cancel: Arc::clone(&saw_cancel),
        },
        calls: AtomicUsize::new(0),
    }));
    let socket = opened.socket.clone();
    let mut seen = None;
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_held", "sleep 60"));
            entered_rx
                .recv_timeout(DEADLINE)
                .expect("the shell is running");
            send(&client, r#"{"id":"c_tools","command":"tools"}"#);
            let tools = answer_within(&client, "c_tools");
            send(&client, &shell_line("c_second", "echo hi"));
            let second = answer_within(&client, "c_second");
            seen = Some((tools, second, client));
            Ok(())
        })
        .unwrap();
    opened.close();
    let (tools, second, _client) = seen.expect("the client ran");
    let tools = tools.expect("tools was answered while the shell ran");
    assert_eq!(kind(&tools), "command_accepted", "{tools}");
    assert_eq!(tools["payload"]["result"]["tools"][0]["name"], "read");
    let second = second.expect("a second shell was answered while the first ran");
    assert_eq!(second["payload"]["result"]["output"], "hi\n");
    assert!(
        saw_cancel.load(Ordering::Relaxed),
        "close cancelled the first shell"
    );
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
                retry_after_ms: None,
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

fn job_stop_line(id: &str, job_id: &str) -> String {
    format!(r#"{{"id":"{id}","command":"job_stop","args":{{"job_id":"{job_id}"}}}}"#)
}

/// Opens a job on `jobs` whose stop signals `fired`.
fn open_job(jobs: &FakeJobs, fired: mpsc::Sender<()>) -> contract::jobs::Opened {
    jobs.open(Opening {
        tool: "shell".into(),
        description: "sleep 60".into(),
        stop: Stop(Box::new(move || fired.send(()).unwrap())),
        lines: false,
        input: None,
    })
    .unwrap()
}

#[test]
fn job_stop_is_answered_on_the_reader_thread_while_the_loop_is_blocked() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let (fired_tx, fired_rx) = mpsc::channel();
    let job = open_job(&jobs, fired_tx);
    let job_id = job.started.job_id.0.clone();
    let opened = Opened::open(vec![]);
    opened.session.jobs(jobs.clone());
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            // This body is the loop's side: it drains nothing until the
            // stop has been sent, so the reader thread answered alone.
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &job_stop_line("c_stop", &job_id));
            let accepted = response(&client, "c_stop");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_stop"));
            assert!(
                fired_rx.recv_timeout(DEADLINE).is_ok(),
                "the job's stop was not sent"
            );
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "job_stop waited for the drain"
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    drop(job);
    opened.close();
}

#[test]
fn job_stop_for_an_unknown_or_ended_job_is_rejected_stale() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let (fired_tx, fired_rx) = mpsc::channel();
    let job = open_job(&jobs, fired_tx);
    let ended_id = job.started.job_id.0.clone();
    (job.end.0)(contract::events::JobCompleted {
        job_id: job.started.job_id.clone(),
        status: contract::events::Outcome::Completed,
        error: None,
        process: None,
        output_tail: None,
    });
    let opened = Opened::open(vec![]);
    opened.session.jobs(jobs.clone());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            for (id, job_id) in [("c_1", "j_5e10c0ffee123456"), ("c_2", ended_id.as_str())] {
                send(&client, &job_stop_line(id, job_id));
                let rejected = response(&client, id);
                assert_eq!(
                    rejection(&rejected),
                    ("stale_request", "That job is not running.")
                );
                assert_eq!(command_id(&rejected), Some(id));
            }
            assert!(
                fired_rx.try_recv().is_err(),
                "a stale stop called a job's stop"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn job_stop_and_background_without_jobs_are_rejected_stale() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &job_stop_line("c_1", "j_5e10c0ffee123456"));
            assert_eq!(
                rejection(&response(&client, "c_1")),
                ("stale_request", "That job is not running.")
            );
            send(&client, r#"{"id":"c_2","command":"background"}"#);
            let rejected = response(&client, "c_2");
            assert_eq!(
                rejection(&rejected),
                ("stale_request", "No shell call is running.")
            );
            assert_eq!(command_id(&rejected), Some("c_2"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn job_stop_with_a_malformed_job_id_is_invalid_arguments() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"job_stop"}"#);
            assert_eq!(
                rejection(&response(&client, "c_1")),
                ("invalid_arguments", UNFIT)
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn background_asks_the_running_foreground_calls_and_writes_nothing() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let asked = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&asked);
    let call: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        seen.store(true, Ordering::SeqCst);
        true
    });
    jobs.foreground(Foreground(Arc::downgrade(&call)));
    let opened = Opened::open(vec![]);
    opened.session.jobs(jobs.clone());
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"background"}"#);
            let accepted = response(&client, "c_1");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_1"));
            assert!(
                asked.load(Ordering::SeqCst),
                "the call was not asked to move"
            );
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "background waited for the drain"
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
    drop(call);
}

#[test]
fn background_with_no_call_moving_is_rejected_stale() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let staying: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| false);
    jobs.foreground(Foreground(Arc::downgrade(&staying)));
    let opened = Opened::open(vec![]);
    opened.session.jobs(jobs.clone());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"background"}"#);
            assert_eq!(
                rejection(&response(&client, "c_1")),
                ("stale_request", "No shell call is running.")
            );
            Ok(())
        })
        .unwrap();
    opened.close();
    drop(staying);
}

#[test]
fn commands_answers_with_the_rows_it_was_given_while_the_inbox_is_unread() {
    let opened = Opened::open(vec![]);
    opened.session.commands(vec![
        CommandInfo {
            name: "review".into(),
            description: "Review a diff.".into(),
            argument_hint: Some("[base]".into()),
            tag: "template".into(),
        },
        CommandInfo {
            name: "tdd".into(),
            description: "Test first.".into(),
            argument_hint: None,
            tag: "skill".into(),
        },
    ]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            send(&client, r#"{"id":"c_early","command":"commands"}"#);
            assert_eq!(
                rejection(&response(&client, "c_early")),
                ("not_subscribed", NOT_SUBSCRIBED)
            );
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"wait"}]}}"#,
            );
            send(&client, r#"{"id":"c_1","command":"commands"}"#);
            let answer = response(&client, "c_1");
            assert_eq!(kind(&answer), "command_accepted");
            assert_eq!(
                answer["payload"]["result"],
                serde_json::json!({"commands": [
                    {"name": "review", "description": "Review a diff.",
                     "argument_hint": "[base]", "tag": "template"},
                    {"name": "tdd", "description": "Test first.", "tag": "skill"}]})
            );
            send(
                &client,
                r#"{"id":"c_2","command":"commands","args":{"future":1}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_2")),
                ("invalid_arguments", UNFIT)
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn commands_with_none_given_answers_an_empty_list() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"commands","args":{}}"#);
            let answer = response(&client, "c_1");
            assert_eq!(kind(&answer), "command_accepted");
            assert_eq!(
                answer["payload"]["result"],
                serde_json::json!({"commands": []})
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

struct FakeDoor {
    calls: Mutex<Vec<(String, String)>>,
}

impl FakeDoor {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl contract::extension::ExtensionDoor for FakeDoor {
    fn command(
        &self,
        name: &str,
        text: &str,
    ) -> Result<Box<dyn FnOnce() + Send>, contract::inbox::Rejection> {
        if name == "known" {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_owned(), text.to_owned()));
            Ok(Box::new(|| {}))
        } else {
            Err(contract::inbox::Rejection {
                code: ErrorCode::UnknownCommand,
                message: format!("`{name}` names no extension command."),
            })
        }
    }

    fn seal(&self) {}
}

struct HeldDoor {
    admitted: Mutex<Vec<mpsc::Sender<()>>>,
}

impl contract::extension::ExtensionDoor for HeldDoor {
    fn command(
        &self,
        name: &str,
        _text: &str,
    ) -> Result<Box<dyn FnOnce() + Send>, contract::inbox::Rejection> {
        if name != "slow" {
            return Err(contract::inbox::Rejection {
                code: ErrorCode::UnknownCommand,
                message: format!("`{name}` names no extension command."),
            });
        }
        let (tx, rx) = mpsc::channel();
        self.admitted.lock().unwrap().push(tx);
        // The release runs only after `command_accepted` is on the queue;
        // the fake records the order by releasing the waiter then.
        Ok(Box::new(move || {
            let _ = rx.recv_timeout(DEADLINE).ok();
        }))
    }

    fn seal(&self) {}
}

#[test]
fn command_accepted_arrives_before_run_starts_and_text_absent_is_empty() {
    // `command_accepted` is on the client's stream before the fake's release
    // is called; `text` absent is `""`.
    let opened = Opened::open(vec![]);
    let door = Arc::new(HeldDoor {
        admitted: Mutex::new(Vec::new()),
    });
    opened.session.extensions(door.clone());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_1","command":"command","args":{"name":"slow"}}"#,
            );
            let accepted = response(&client, "c_1");
            assert_eq!(kind(&accepted), "command_accepted");
            // The fake's release blocks until the test lets it go; the
            // acceptance above arrived first, which is the order asserted.
            let tx = door.admitted.lock().unwrap().pop().expect("admitted");
            tx.send(()).unwrap();
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn command_ids_are_admitted_once_across_connections() {
    // The same id sent again on the same connection, and again on a second
    // connection after the first disconnects, is rejected `duplicate_command`
    // and the fake's `command` is called once. A rejected `command` resent
    // with the same id after the name exists is admitted.
    let opened = Opened::open(vec![]);
    let door = Arc::new(FakeDoor::new());
    opened.session.extensions(door.clone());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_1","command":"command","args":{"name":"known","text":"hi"}}"#,
            );
            let accepted = response(&client, "c_1");
            assert_eq!(kind(&accepted), "command_accepted");
            send(
                &client,
                r#"{"id":"c_1","command":"command","args":{"name":"known","text":"hi"}}"#,
            );
            let duplicate = response(&client, "c_1");
            assert_eq!(rejection(&duplicate).0, "duplicate_command");
            drop(client);
            let second = Client::connect(&socket).unwrap();
            subscribe(&second, "c_sub2", "full");
            send(
                &second,
                r#"{"id":"c_1","command":"command","args":{"name":"known","text":"hi"}}"#,
            );
            let retransmit = response(&second, "c_1");
            assert_eq!(rejection(&retransmit).0, "duplicate_command");
            // Unknown name rejects without recording the id...
            send(
                &second,
                r#"{"id":"c_2","command":"command","args":{"name":"nope"}}"#,
            );
            let unknown = response(&second, "c_2");
            assert_eq!(
                rejection(&unknown),
                ("unknown_command", "`nope` names no extension command.")
            );
            // ...and missing `name` rejects `invalid_arguments`.
            send(&second, r#"{"id":"c_3","command":"command","args":{}}"#);
            let missing = response(&second, "c_3");
            assert_eq!(
                rejection(&missing),
                ("invalid_arguments", UNFIT),
                "a missing name does not fit the command"
            );
            assert_eq!(door.calls.lock().unwrap().len(), 1);
            assert_eq!(
                door.calls.lock().unwrap()[0],
                ("known".to_owned(), "hi".to_owned())
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn command_without_a_door_rejects_unknown_command() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_1","command":"command","args":{"name":"known"}}"#,
            );
            let rejected = response(&client, "c_1");
            assert_eq!(
                rejection(&rejected),
                ("unknown_command", "`known` names no extension command.")
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn quiesce_seals_the_door() {
    struct Sealed {
        sealed: AtomicBool,
    }
    impl contract::extension::ExtensionDoor for Sealed {
        fn command(
            &self,
            _name: &str,
            _text: &str,
        ) -> Result<Box<dyn FnOnce() + Send>, contract::inbox::Rejection> {
            Err(contract::inbox::Rejection {
                code: ErrorCode::UnknownCommand,
                message: "none".into(),
            })
        }
        fn seal(&self) {
            self.sealed.store(true, Ordering::SeqCst);
        }
    }
    let opened = Opened::open(vec![]);
    let door = Arc::new(Sealed {
        sealed: AtomicBool::new(false),
    });
    opened.session.extensions(door.clone());
    opened.session.quiesce();
    assert!(door.sealed.load(Ordering::SeqCst));
    opened.close();
}

/// How long a pasted-image test waits for the fake to start and for the
/// rejection after the stopper. A wait that reaches it fails the test.
const PASTE_LIMIT: Duration = Duration::from_secs(10);

struct OkImage;

impl contract::images::Images for OkImage {
    fn process(
        &self,
        _bytes: &[u8],
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<contract::provider::ImageRef, contract::images::ImageError> {
        Ok(contract::provider::ImageRef {
            path: "artifacts/i_test.png".into(),
            mime_type: "image/png".into(),
            width: 3,
            height: 2,
        })
    }
}

struct FailingImage;

impl contract::images::Images for FailingImage {
    fn process(
        &self,
        _bytes: &[u8],
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<contract::provider::ImageRef, contract::images::ImageError> {
        Err(contract::images::ImageError::Failed("boom".into()))
    }
}

struct BlockingImage {
    entered: Mutex<Option<mpsc::Sender<()>>>,
}

struct PasteWake {
    tx: Mutex<Option<mpsc::Sender<()>>>,
}

impl contract::clock::Wake for PasteWake {
    fn wake(&self) {
        if let Some(tx) = self.tx.lock().unwrap().take()
            && let Ok(()) = tx.send(())
        {}
        {}
    }
}

impl contract::images::Images for BlockingImage {
    fn process(
        &self,
        _bytes: &[u8],
        cancel: &dyn contract::tool::Cancel,
    ) -> Result<contract::provider::ImageRef, contract::images::ImageError> {
        if let Some(tx) = self.entered.lock().unwrap().take()
            && let Ok(()) = tx.send(())
        {}
        {}
        if cancel.is_cancelled() {
            return Err(contract::images::ImageError::Cancelled);
        }
        let (tx, rx) = mpsc::channel();
        let waker: Arc<dyn contract::clock::Wake> = Arc::new(PasteWake {
            tx: Mutex::new(Some(tx)),
        });
        cancel.subscribe(Arc::downgrade(&waker));
        if cancel.is_cancelled() {
            return Err(contract::images::ImageError::Cancelled);
        }
        if let Ok(()) = rx.recv_timeout(PASTE_LIMIT) {}
        if cancel.is_cancelled() {
            return Err(contract::images::ImageError::Cancelled);
        }
        Err(contract::images::ImageError::Failed(
            "the test image was never cancelled".into(),
        ))
    }
}

#[test]
fn prompt_with_an_image_is_delivered_with_the_processed_part() {
    let opened = Opened::open(vec![]);
    opened.session.images(Arc::new(OkImage));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"look"},{"type":"image","data":"YQ==","mime_type":"image/png"}]}}"#,
            );
            let delivery = inbox.recv_timeout(DEADLINE).expect("the prompt is delivered");
            let Delivery::Prompt(message, ack) = delivery else {
                panic!("a prompt: {delivery:?}");
            };
            assert_eq!(
                message.content,
                vec![
                    ContentPart::Text { text: "look".into() },
                    ContentPart::Image {
                        path: "artifacts/i_test.png".into(),
                        mime_type: "image/png".into(),
                        width: 3,
                        height: 2,
                    },
                ]
            );
            ack.0(Ok(None));
            let line = response(&client, "c_prompt");
            assert_eq!(kind(&line), "command_accepted", "{line}");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn a_full_subscriber_after_an_extension_ui_line_receives_it_as_seed() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            log.emit(&Event::ExtensionUi(contract::events::ExtensionUi {
                extension: "fiber.test/a".to_owned(),
                ui: contract::events::Ui::Status {
                    status: "syncing".to_owned(),
                },
            }));
            let full = Client::connect(&socket).unwrap();
            subscribe(&full, "c_full", "full");
            let lines = until(&full, |line| kind(line) == "extension_ui");
            assert!(
                lines
                    .iter()
                    .any(|line| line["payload"]["extension"] == "fiber.test/a"
                        && line["payload"]["status"] == "syncing"),
                "a late `full` subscriber receives the kept ui line as seed"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn steer_with_an_image_is_delivered_with_the_processed_part() {
    let opened = Opened::open(vec![]);
    opened.session.images(Arc::new(OkImage));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_steer","command":"steer","args":{"content":[{"type":"image","data":"YQ==","mime_type":"image/png"}]}}"#,
            );
            let delivery = inbox.recv_timeout(DEADLINE).expect("the steer is delivered");
            let Delivery::Steer(message, ack) = delivery else {
                panic!("a steer: {delivery:?}");
            };
            assert_eq!(
                message.content,
                vec![ContentPart::Image {
                    path: "artifacts/i_test.png".into(),
                    mime_type: "image/png".into(),
                    width: 3,
                    height: 2,
                },]
            );
            ack.0(Ok(None));
            let line = response(&client, "c_steer");
            assert_eq!(kind(&line), "command_accepted", "{line}");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn prompt_with_a_failed_image_is_rejected_io_failed() {
    let opened = Opened::open(vec![]);
    opened.session.images(Arc::new(FailingImage));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"image","data":"YQ==","mime_type":"image/png"}]}}"#,
            );
            let line = response(&client, "c_prompt");
            assert_eq!(
                rejection(&line),
                ("io_failed", "Image 1 could not be processed: boom")
            );
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "a rejected prompt reaches no inbox"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn the_stopper_cancels_a_pasted_image_in_flight() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let opened = Opened::open(vec![]);
    opened.session.images(Arc::new(BlockingImage {
        entered: Mutex::new(Some(entered_tx)),
    }));
    let socket = opened.socket.clone();
    let stopper = opened.session.stopper();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"image","data":"YQ==","mime_type":"image/png"}]}}"#,
            );
            entered_rx
                .recv_timeout(PASTE_LIMIT)
                .expect("the image started");
            stopper();
            let line = response(&client, "c_prompt");
            assert_eq!(
                rejection(&line),
                ("closing", "Image 1 was not processed: the session is closing.")
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn close_now_starts_the_shutdown_and_sends_the_loop_nothing() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let (hook_tx, hook_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    opened.session.close_now(Arc::new(move || {
        hook_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(DEADLINE)
            .expect("the test releases the hook");
    }));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_close_now","command":"close","args":{"now":true}}"#,
            );
            hook_rx.recv_timeout(DEADLINE).expect("the hook runs");
            // The answer was queued before the hook ran: if the order were
            // reversed, this read would wait out its deadline and fail.
            let answer = response(&client, "c_close_now");
            assert_eq!(kind(&answer), "command_accepted", "{answer}");
            assert_eq!(command_id(&answer), Some("c_close_now"));
            release_tx.send(()).unwrap();
            assert!(hook_rx.try_recv().is_err(), "the hook ran once");
            send(&client, r#"{"id":"c_tools","command":"tools"}"#);
            let tools = response(&client, "c_tools");
            assert_eq!(kind(&tools), "command_accepted", "{tools}");
            // The reader handles lines in order, so every line before the
            // tools answer has been handled: no `Close` reached the loop.
            assert!(inbox.try_recv().is_err(), "no Close reached the loop");
            send(
                &client,
                r#"{"id":"c_close_now_2","command":"close","args":{"now":true}}"#,
            );
            let second = response(&client, "c_close_now_2");
            assert_eq!(kind(&second), "command_accepted", "{second}");
            hook_rx
                .recv_timeout(DEADLINE)
                .expect("a repeat is answered too");
            release_tx.send(()).unwrap();
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn close_without_now_starts_no_shutdown() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let (hook_tx, hook_rx) = mpsc::channel::<()>();
    opened.session.close_now(Arc::new(move || {
        hook_tx.send(()).unwrap();
    }));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_close_bare","command":"close"}"#);
            send(
                &client,
                r#"{"id":"c_close_false","command":"close","args":{"now":false}}"#,
            );
            assert_eq!(take(&inbox), "close");
            assert_eq!(take(&inbox), "close");
            for id in ["c_close_bare", "c_close_false"] {
                let answer = response(&client, id);
                assert_eq!(kind(&answer), "command_accepted", "{answer}");
            }
            send(&client, r#"{"id":"c_tools","command":"tools"}"#);
            let tools = response(&client, "c_tools");
            assert_eq!(kind(&tools), "command_accepted", "{tools}");
            // The reader handles lines in order, so a hook call would have
            // come before the tools answer.
            assert!(hook_rx.try_recv().is_err(), "no shutdown started");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn close_now_with_no_shutdown_wired_is_a_plain_close() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_close_now","command":"close","args":{"now":true}}"#,
            );
            assert_eq!(take(&inbox), "close");
            let answer = response(&client, "c_close_now");
            assert_eq!(kind(&answer), "command_accepted", "{answer}");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn close_now_with_a_non_boolean_is_invalid_and_starts_nothing() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let (hook_tx, hook_rx) = mpsc::channel::<()>();
    opened.session.close_now(Arc::new(move || {
        hook_tx.send(()).unwrap();
    }));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_close_now","command":"close","args":{"now":"yes"}}"#,
            );
            let answer = response(&client, "c_close_now");
            assert_eq!(rejection(&answer), ("invalid_arguments", UNFIT));
            assert_eq!(command_id(&answer), Some("c_close_now"));
            assert!(inbox.try_recv().is_err(), "no Close reached the loop");
            assert!(hook_rx.try_recv().is_err(), "no shutdown started");
            Ok(())
        })
        .unwrap();
    opened.close();
}
