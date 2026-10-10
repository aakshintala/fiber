//! A connection changing its subscription level (`docs/invocation.md`,
//! `subscribe`): raising `summary` to `full` folds the stream and counts in
//! `clients`, and lowering `full` to `summary` stops the stream and the
//! count. A repeated `subscribe` at the same level is `invalid_arguments`.

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
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::clock::Clock;
use contract::events::{
    Empty, Event, ExtensionsLoaded, LoadedExtension, Notice, SessionState, SessionStatus,
};
use contract::inbox::Delivery;
use contract::shapes::{Tokens, Usage};
use contract::{ErrorCode, SessionId};
use doors::{Session, mint};
use fakes::Client;
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;
use serde_json::Value;

/// A hang bound for one line, the same order as the log crate's watcher tests.
const DEADLINE: Duration = Duration::from_secs(10);

const ALREADY: &str = "This connection is already subscribed at this level.";
const UNFIT: &str = "The arguments do not fit this command.";

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
    buf: Arc<std::sync::Mutex<Vec<u8>>>,
    ready: Arc<std::sync::Condvar>,
}

impl Shared {
    fn new() -> Self {
        Self {
            buf: Arc::new(std::sync::Mutex::new(Vec::new())),
            ready: Arc::new(std::sync::Condvar::new()),
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
    log: Arc<Log>,
    dir: PathBuf,
    socket: PathBuf,
    session: Session,
}

impl Opened {
    fn open(tools: Vec<contract::events::ToolInfo>) -> Self {
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
            log,
            dir,
            socket: home.join("run").join(&id.0),
            session,
        }
    }

    #[track_caller]
    fn close(self) -> Temp {
        let Opened {
            session,
            log,
            _temp,
            ..
        } = self;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            session.close(log);
            if let Ok(()) = tx.send(()) {}
        });
        Deadline::after(DEADLINE).recv(&rx).expect("close returned");
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

fn kinds(lines: &[Value]) -> Vec<String> {
    lines.iter().map(|line| kind(line).to_owned()).collect()
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

/// Every line up to and including the acknowledgement for `id`: the
/// complete, ordered sequence, so a missing, duplicated or reordered line
/// fails.
fn response(client: &Client, id: &str) -> Vec<Value> {
    until(client, |line| command_id(line) == Some(id))
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

fn count(line: &Value) -> u64 {
    assert_eq!(kind(line), "clients", "{line}");
    line["payload"]["count"].as_u64().unwrap()
}

fn prompt_line(id: &str, command: &str) -> String {
    format!(
        r#"{{"id":"{id}","command":"{command}","args":{{"content":[{{"type":"text","text":"hi"}}]}}}}"#
    )
}

/// Takes the next inbox delivery, which is a prompt, and holds its
/// acknowledgement.
#[track_caller]
fn take_prompt(inbox: &mpsc::Receiver<Delivery>) -> contract::inbox::Ack {
    let delivery = Deadline::after(DEADLINE).recv(inbox).expect("a delivery");
    if let Delivery::Prompt(_, ack) = delivery {
        ack
    } else {
        panic!("a prompt delivery, not {delivery:?}")
    }
}

/// Takes the next inbox delivery, which is a steer, and holds its
/// acknowledgement.
#[track_caller]
fn take_steer(inbox: &mpsc::Receiver<Delivery>) -> contract::inbox::Ack {
    let delivery = Deadline::after(DEADLINE).recv(inbox).expect("a delivery");
    if let Delivery::Steer(_, ack) = delivery {
        ack
    } else {
        panic!("a steer delivery, not {delivery:?}")
    }
}

/// Reads the client to EOF: every line still arriving after the session
/// closed. Empty when the test consumed everything it was owed.
fn drain(client: &Client) -> Vec<Value> {
    let mut lines = Vec::new();
    while let Some(line) = client.recv(DEADLINE) {
        lines.push(line);
    }
    lines
}

#[test]
fn summary_then_full_folds_the_stream_and_counts_in_clients() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            for _ in 0..3 {
                log.append(&step(), None, None).unwrap();
            }
            log.append(&status("one"), None, None).unwrap();
            log.append(&extensions(), None, None).unwrap();
            // An upgrade seeds the kept lines as a first `full` subscribe
            // does: the latest `session_status`, `steering_queue` and
            // `extension_ui`, in that order (see `socket.rs`: a first `full`
            // subscribe queues the latest lines before it counts in
            // `clients`).
            log.append(
                &Event::SteeringQueue(contract::events::SteeringQueue {
                    messages: Vec::new(),
                }),
                None,
                None,
            )
            .unwrap();
            log.append(
                &Event::ExtensionUi(contract::events::ExtensionUi {
                    extension: "fiber.test/a".to_owned(),
                    ui: contract::events::Ui::Status {
                        status: "syncing".to_owned(),
                    },
                }),
                None,
                None,
            )
            .unwrap();
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_a_sub", "summary");
            assert_eq!(kind(&next(&client)), "session_status");
            assert_eq!(kind(&next(&client)), "extensions_loaded");
            send(
                &client,
                r#"{"id":"c_a_up","command":"subscribe","args":{"level":"full"}}"#,
            );
            let lines = until(&client, |line| kind(line) == "clients");
            assert_eq!(
                kinds(&lines),
                [
                    "command_accepted",
                    "step_started",
                    "step_started",
                    "step_started",
                    "extensions_loaded",
                    "session_status",
                    "steering_queue",
                    "extension_ui",
                    "clients",
                ],
                "{lines:?}"
            );
            assert_eq!(command_id(&lines[0]), Some("c_a_up"));
            assert_eq!(seqs(&lines[1..5]), vec![0, 1, 2, 3]);
            assert_eq!(
                lines[7]["payload"]["extension"], "fiber.test/a",
                "{lines:?}"
            );
            assert_eq!(count(&lines[8]), 1);
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the client outlives run");
    let _temp = opened.close();
    assert!(drain(&client).is_empty(), "nothing arrives after the fold");
}

#[test]
fn full_then_summary_stops_the_stream_and_the_count() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            log.append(&status("one"), None, None).unwrap();
            log.append(&extensions(), None, None).unwrap();
            let observer = Client::connect(&socket).unwrap();
            subscribe(&observer, "c_b_sub", "full");
            // A first `full` subscribe queues the latest lines before it
            // counts in `clients`.
            let fed = until(&observer, |line| kind(line) == "clients");
            assert_eq!(
                kinds(&fed),
                ["extensions_loaded", "session_status", "clients"],
                "{fed:?}"
            );
            assert_eq!(count(&fed[2]), 1);
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_a_sub", "summary");
            assert_eq!(kind(&next(&client)), "session_status");
            assert_eq!(kind(&next(&client)), "extensions_loaded");
            send(
                &client,
                r#"{"id":"c_a_up","command":"subscribe","args":{"level":"full"}}"#,
            );
            let raised = until(&client, |line| kind(line) == "clients");
            assert_eq!(
                kinds(&raised),
                [
                    "command_accepted",
                    "extensions_loaded",
                    "session_status",
                    "clients",
                ],
                "{raised:?}"
            );
            assert_eq!(count(&raised[3]), 2);
            assert_eq!(count(&next(&observer)), 2);
            send(
                &client,
                r#"{"id":"c_a_down","command":"subscribe","args":{"level":"summary"}}"#,
            );
            let lowered = until(&client, |line| kind(line) == "extensions_loaded");
            assert_eq!(
                kinds(&lowered),
                [
                    "clients",
                    "command_accepted",
                    "session_status",
                    "extensions_loaded",
                ],
                "{lowered:?}"
            );
            assert_eq!(count(&lowered[0]), 1);
            assert_eq!(command_id(&lowered[1]), Some("c_a_down"));
            assert_eq!(count(&next(&observer)), 1);
            log.append(&notice("n2"), None, None).unwrap();
            log.append(&status("two"), None, None).unwrap();
            let status_line = next(&client);
            assert_eq!(kind(&status_line), "session_status", "{status_line}");
            assert_eq!(status_line["payload"]["name"], "two");
            assert_eq!(kind(&next(&observer)), "notice");
            let observed = next(&observer);
            assert_eq!(kind(&observed), "session_status", "{observed}");
            assert_eq!(observed["payload"]["name"], "two");
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the client outlives run");
    let _temp = opened.close();
    assert!(
        drain(&client).is_empty(),
        "nothing arrives after the status"
    );
}

#[test]
fn a_downgrade_is_acknowledged_after_every_full_line_before_it() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            log.append(&status("one"), None, None).unwrap();
            log.append(&extensions(), None, None).unwrap();
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_a_sub", "full");
            let fed = until(&client, |line| kind(line) == "session_status");
            assert_eq!(
                kinds(&fed),
                ["extensions_loaded", "session_status"],
                "{fed:?}"
            );
            assert_eq!(count(&next(&client)), 1);
            log.append(&notice("n1"), None, None).unwrap();
            send(
                &client,
                r#"{"id":"c_a_down","command":"subscribe","args":{"level":"summary"}}"#,
            );
            let lowered = until(&client, |line| kind(line) == "extensions_loaded");
            assert_eq!(
                kinds(&lowered),
                [
                    "notice",
                    "clients",
                    "command_accepted",
                    "session_status",
                    "extensions_loaded",
                ],
                "{lowered:?}"
            );
            log.append(&notice("n2"), None, None).unwrap();
            log.append(&status("two"), None, None).unwrap();
            let status_line = next(&client);
            assert_eq!(kind(&status_line), "session_status", "{status_line}");
            assert_eq!(status_line["payload"]["name"], "two");
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the client outlives run");
    let _temp = opened.close();
    assert!(
        drain(&client).is_empty(),
        "nothing arrives after the status"
    );
}

#[test]
fn a_repeated_subscribe_at_the_same_level_is_invalid_arguments() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            log.append(&status("one"), None, None).unwrap();
            let summary = Client::connect(&socket).unwrap();
            let full = Client::connect(&socket).unwrap();
            subscribe(&summary, "c_a_1", "summary");
            subscribe(&full, "c_b_1", "full");
            for line in [next(&summary), next(&full)] {
                assert_eq!(kind(&line), "session_status", "{line}");
            }
            send(
                &summary,
                r#"{"id":"c_a_2","command":"subscribe","args":{"level":"summary"}}"#,
            );
            let repeated = response(&summary, "c_a_2");
            assert_eq!(kinds(&repeated), ["command_rejected"], "{repeated:?}");
            assert_eq!(
                rejection(repeated.last().unwrap()),
                ("invalid_arguments", ALREADY)
            );
            send(
                &full,
                r#"{"id":"c_b_2","command":"subscribe","args":{"level":"full"}}"#,
            );
            let held = response(&full, "c_b_2");
            assert_eq!(kinds(&held), ["clients", "command_rejected"], "{held:?}");
            assert_eq!(count(&held[0]), 1);
            assert_eq!(
                rejection(held.last().unwrap()),
                ("invalid_arguments", ALREADY)
            );
            log.append(&notice("n"), None, None).unwrap();
            log.append(&status("two"), None, None).unwrap();
            // The summary level held: no notice, then the status.
            assert_eq!(
                kinds(&until(&summary, |line| kind(line) == "session_status")),
                ["session_status"]
            );
            // The full level held: the notice, then the status.
            assert_eq!(
                kinds(&until(&full, |line| kind(line) == "session_status")),
                ["notice", "session_status"]
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn an_upgrade_over_an_unreadable_line_folds_the_lines_before_it_then_ends() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            for _ in 0..3 {
                log.append(&step(), None, None).unwrap();
            }
            corrupt(&dir, 1);
            let raw = UnixStream::connect(&socket).unwrap();
            raw.set_read_timeout(Some(DEADLINE)).unwrap();
            let mut write = raw.try_clone().unwrap();
            let mut read = BufReader::new(raw);
            send_raw(
                &mut write,
                r#"{"id":"c_a_sub","command":"subscribe","args":{"level":"summary"}}"#,
            );
            assert_eq!(kind(&read_raw(&mut read).unwrap()), "command_accepted");
            send_raw(
                &mut write,
                r#"{"id":"c_a_up","command":"subscribe","args":{"level":"full"}}"#,
            );
            let ack = read_raw(&mut read).unwrap();
            assert_eq!(kind(&ack), "command_accepted", "{ack}");
            assert_eq!(command_id(&ack), Some("c_a_up"));
            let mut lines = Vec::new();
            while let Some(line) = read_raw(&mut read) {
                lines.push(line);
            }
            assert_eq!(seqs(&lines), vec![0]);
            assert_eq!(lines.len(), 1);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn an_acknowledgement_pending_across_a_change_reaches_the_client() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            for _ in 0..3 {
                log.append(&step(), None, None).unwrap();
            }
            log.append(&status("one"), None, None).unwrap();
            log.append(&extensions(), None, None).unwrap();
            // Raise with the prompt unanswered: its acknowledgement arrives
            // after the fold and the latest lines.
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_a_sub", "summary");
            assert_eq!(kind(&next(&client)), "session_status");
            assert_eq!(kind(&next(&client)), "extensions_loaded");
            send(&client, &prompt_line("c_a_p", "prompt"));
            let ack = take_prompt(&inbox);
            send(
                &client,
                r#"{"id":"c_a_up","command":"subscribe","args":{"level":"full"}}"#,
            );
            let up = response(&client, "c_a_up");
            assert_eq!(kinds(&up), ["command_accepted"], "{up:?}");
            assert_eq!(command_id(up.last().unwrap()), Some("c_a_up"));
            (ack.0)(Ok(None));
            let raised = until(&client, |line| command_id(line) == Some("c_a_p"));
            assert_eq!(
                kinds(&raised),
                [
                    "step_started",
                    "step_started",
                    "step_started",
                    "extensions_loaded",
                    "session_status",
                    "clients",
                    "command_accepted",
                ],
                "{raised:?}"
            );
            assert_eq!(seqs(&raised[..3]), vec![0, 1, 2]);
            // The other order: answered first, it arrives before the change.
            send(&client, &prompt_line("c_a_q", "prompt"));
            let ack = take_prompt(&inbox);
            (ack.0)(Ok(None));
            let answered = response(&client, "c_a_q");
            assert_eq!(kinds(&answered), ["command_accepted"], "{answered:?}");
            assert_eq!(kind(answered.last().unwrap()), "command_accepted");
            send(
                &client,
                r#"{"id":"c_a_down","command":"subscribe","args":{"level":"summary"}}"#,
            );
            let lowered = until(&client, |line| kind(line) == "extensions_loaded");
            assert_eq!(
                kinds(&lowered),
                [
                    "clients",
                    "command_accepted",
                    "session_status",
                    "extensions_loaded",
                ],
                "{lowered:?}"
            );
            // Lower with the steer unanswered: its acknowledgement arrives
            // after the prelude.
            send(
                &client,
                r#"{"id":"c_a_up2","command":"subscribe","args":{"level":"full"}}"#,
            );
            let _ = until(&client, |line| kind(line) == "clients");
            send(&client, &prompt_line("c_a_s", "steer"));
            let ack = take_steer(&inbox);
            send(
                &client,
                r#"{"id":"c_a_down2","command":"subscribe","args":{"level":"summary"}}"#,
            );
            let down = response(&client, "c_a_down2");
            assert_eq!(kinds(&down), ["clients", "command_accepted"], "{down:?}");
            assert_eq!(command_id(down.last().unwrap()), Some("c_a_down2"));
            assert_eq!(count(&down[0]), 0);
            (ack.0)(Ok(None));
            let steered = until(&client, |line| command_id(line) == Some("c_a_s"));
            assert_eq!(
                kinds(&steered),
                ["session_status", "extensions_loaded", "command_accepted",],
                "{steered:?}"
            );
            // Answered first, it arrives at full level, before the change.
            send(&client, &prompt_line("c_a_t", "steer"));
            let ack = take_steer(&inbox);
            (ack.0)(Ok(None));
            let steered_ack = response(&client, "c_a_t");
            assert_eq!(kinds(&steered_ack), ["command_accepted"], "{steered_ack:?}");
            assert_eq!(kind(steered_ack.last().unwrap()), "command_accepted");
            send(
                &client,
                r#"{"id":"c_a_up3","command":"subscribe","args":{"level":"full"}}"#,
            );
            let _ = until(&client, |line| kind(line) == "clients");
            send(
                &client,
                r#"{"id":"c_a_down3","command":"subscribe","args":{"level":"summary"}}"#,
            );
            let lowered = until(&client, |line| kind(line) == "extensions_loaded");
            assert_eq!(
                kinds(&lowered),
                [
                    "clients",
                    "command_accepted",
                    "session_status",
                    "extensions_loaded",
                ],
                "{lowered:?}"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn two_changes_in_one_write_apply_in_order() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            for _ in 0..3 {
                log.append(&step(), None, None).unwrap();
            }
            log.append(&status("one"), None, None).unwrap();
            log.append(&extensions(), None, None).unwrap();
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_a_sub", "summary");
            assert_eq!(kind(&next(&client)), "session_status");
            assert_eq!(kind(&next(&client)), "extensions_loaded");
            send(
                &client,
                concat!(
                    r#"{"id":"c_a_up","command":"subscribe","args":{"level":"full"}}"#,
                    "\n",
                    r#"{"id":"c_a_down","command":"subscribe","args":{"level":"summary"}}"#,
                ),
            );
            let mut lines = Vec::new();
            for _ in 0..11 {
                lines.push(next(&client));
            }
            assert_eq!(
                kinds(&lines),
                [
                    "command_accepted",
                    "step_started",
                    "step_started",
                    "step_started",
                    "extensions_loaded",
                    "session_status",
                    "clients",
                    "clients",
                    "command_accepted",
                    "session_status",
                    "extensions_loaded",
                ],
                "{lines:?}"
            );
            assert_eq!(command_id(&lines[0]), Some("c_a_up"));
            assert_eq!(seqs(&lines[1..5]), vec![0, 1, 2, 3]);
            assert_eq!(count(&lines[6]), 1);
            assert_eq!(count(&lines[7]), 0);
            assert_eq!(command_id(&lines[8]), Some("c_a_down"));
            log.append(&notice("n"), None, None).unwrap();
            log.append(&status("two"), None, None).unwrap();
            // Lowered: no notice, then the status.
            assert_eq!(
                kinds(&until(&client, |line| kind(line) == "session_status")),
                ["session_status"]
            );
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the client outlives run");
    let _temp = opened.close();
    assert!(
        drain(&client).is_empty(),
        "nothing arrives after the status"
    );
}

#[test]
fn a_later_subscribe_that_does_not_parse_is_unfit() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_bogus","command":"subscribe","args":{"level":"bogus"}}"#,
            );
            let bogus = response(&client, "c_bogus");
            assert_eq!(kinds(&bogus), ["clients", "command_rejected"], "{bogus:?}");
            assert_eq!(count(&bogus[0]), 1);
            assert_eq!(
                rejection(bogus.last().unwrap()),
                ("invalid_arguments", UNFIT)
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

/// Reads one JSON line from a raw socket, or `None` at EOF.
fn read_raw(read: &mut BufReader<UnixStream>) -> Option<Value> {
    let mut buf = Vec::new();
    match read.read_until(b'\n', &mut buf) {
        Ok(0) => None,
        Ok(_) => Some(serde_json::from_slice(&buf).unwrap()),
        Err(_) => panic!("a line arrived"),
    }
}

fn send_raw(write: &mut UnixStream, line: &str) {
    write.write_all(line.as_bytes()).unwrap();
    if !line.ends_with('\n') {
        write.write_all(b"\n").unwrap();
    }
    write.flush().unwrap();
}

#[test]
fn a_writer_that_fails_on_a_later_page_ends_the_connection() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            for _ in 0..1_030 {
                log.append(&step(), None, None).unwrap();
            }
            let observer = Client::connect(&socket).unwrap();
            subscribe(&observer, "c_b_sub", "full");
            let fed = until(&observer, |line| kind(line) == "clients");
            assert_eq!(count(&fed[fed.len() - 1]), 1);
            assert_eq!(seqs(&fed[..fed.len() - 1]), (0..1_030).collect::<Vec<_>>());
            corrupt(&dir, 1_027);
            // A upgrades after the corruption: accepted, then EOF.
            let raw = UnixStream::connect(&socket).unwrap();
            raw.set_read_timeout(Some(DEADLINE)).unwrap();
            let mut write = raw.try_clone().unwrap();
            let mut read = BufReader::new(raw);
            send_raw(
                &mut write,
                r#"{"id":"c_a_sub","command":"subscribe","args":{"level":"summary"}}"#,
            );
            assert_eq!(kind(&read_raw(&mut read).unwrap()), "command_accepted");
            send_raw(
                &mut write,
                r#"{"id":"c_a_up","command":"subscribe","args":{"level":"full"}}"#,
            );
            let mut lines = Vec::new();
            while let Some(line) = read_raw(&mut read) {
                lines.push(line);
            }
            assert_eq!(kinds(&lines)[0], "command_accepted");
            assert_eq!(command_id(&lines[0]), Some("c_a_up"));
            assert_eq!(seqs(&lines[1..]), (0..1_027).collect::<Vec<_>>());
            assert_eq!(lines.len(), 1_028);
            // The disconnect cleanup ran while the session runs.
            assert_eq!(count(&next(&observer)), 2);
            assert_eq!(count(&next(&observer)), 1);
            // A first subscribe fails the same way: the fold, then EOF.
            let raw = UnixStream::connect(&socket).unwrap();
            raw.set_read_timeout(Some(DEADLINE)).unwrap();
            let mut write = raw.try_clone().unwrap();
            let mut read = BufReader::new(raw);
            send_raw(
                &mut write,
                r#"{"id":"c_c_sub","command":"subscribe","args":{"level":"full"}}"#,
            );
            let mut lines = Vec::new();
            while let Some(line) = read_raw(&mut read) {
                lines.push(line);
            }
            assert_eq!(kinds(&lines)[0], "command_accepted");
            assert_eq!(command_id(&lines[0]), Some("c_c_sub"));
            assert_eq!(seqs(&lines[1..]), (0..1_027).collect::<Vec<_>>());
            assert_eq!(lines.len(), 1_028);
            assert_eq!(count(&next(&observer)), 2);
            assert_eq!(count(&next(&observer)), 1);
            Ok(())
        })
        .unwrap();
    opened.close();
}
