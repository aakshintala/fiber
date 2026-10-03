//! `Log::watch_all`, an injected line, and ephemeral emit
//! (`docs/architecture.md`, "Streaming").

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

mod common;

use std::fs;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use common::*;
use contract::Envelope;
use contract::emit::Emit;
use log::{Log, Watcher};
use serde_json::{Map, Value};

const DEADLINE: Duration = Duration::from_secs(10);

fn relay(mut watcher: Watcher) -> Receiver<Option<Envelope>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        loop {
            let got = watcher.recv().unwrap();
            let end = got.is_none();
            if tx.send(got).is_err() || end {
                break;
            }
        }
    });
    rx
}

fn next(rx: &Receiver<Option<Envelope>>) -> Option<Envelope> {
    rx.recv_timeout(DEADLINE)
        .expect("the watcher to receive a line before the deadline")
}

fn marked(kind: &str) -> Envelope {
    Envelope {
        kind: kind.to_owned(),
        session_id: id("s_1"),
        ts: 1,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: Map::new(),
    }
}

fn open(name: &str) -> (common::TestDir, Log) {
    let tmp = TestDir::new(name);
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    (tmp, log)
}

#[test]
fn watch_all_yields_every_durable_line_from_the_start_then_live_lines() {
    let (tmp, log) = open("watch-all");
    let first = log.append(&session_started(), None, None).unwrap();
    let second = log.append(&empty("step_started"), None, None).unwrap();
    let rx = relay(log.watch_all().unwrap());
    let live = log.append(&empty("step_started"), None, None).unwrap();

    assert_eq!(next(&rx), Some(first));
    assert_eq!(next(&rx), Some(second));
    assert_eq!(next(&rx), Some(live));
    drop(log);
    assert_eq!(next(&rx), None);
    let lines = fs::read_to_string(tmp.session(&id("s_1")).join("events.jsonl")).unwrap();
    assert_eq!(lines.lines().count(), 3);
}

#[test]
fn an_injected_line_arrives_in_push_order_and_is_dropped_when_lagged() {
    let (_tmp, log) = open("inject");
    let mut watcher = log.watch();
    let injector = watcher.injector();
    let before = log.append(&empty("step_started"), None, None).unwrap();
    let injected = marked("notice");
    injector.push(injected.clone());
    let after = log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(watcher.recv().unwrap(), Some(before));
    assert_eq!(watcher.recv().unwrap(), Some(injected));
    assert_eq!(watcher.recv().unwrap(), Some(after));

    let lagged = log.watch();
    let lagged_injector = lagged.injector();
    for _ in 0..1_100 {
        log.append(&delta("x"), None, None).unwrap();
    }
    lagged_injector.push(marked("notice"));
    let sentinel = log.append(&session_started(), None, None).unwrap();
    let rx = relay(lagged);
    let mut saw_notice = false;
    loop {
        let line = next(&rx).unwrap();
        if line.kind == "notice" {
            saw_notice = true;
        }
        if line.seq == sentinel.seq {
            break;
        }
    }
    assert!(!saw_notice, "a lagged watcher drops an injected line");
}

#[test]
fn pushing_to_a_dropped_watchers_injector_does_nothing() {
    let (_tmp, log) = open("inject-drop");
    let watcher = log.watch();
    let injector = watcher.injector();
    drop(watcher);
    injector.push(marked("notice"));
    let rx = relay(log.watch());
    let line = log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(next(&rx), Some(line));
    drop(log);
    assert_eq!(next(&rx), None);
}

#[test]
fn emit_sends_an_ephemeral_line_to_every_watcher_and_refuses_a_durable_one() {
    let (tmp, log) = open("emit");
    let one = relay(log.watch());
    let two = relay(log.watch());
    Emit::emit(&log, &delta("e"));
    Emit::emit(&log, &session_started());
    let path = tmp.session(&id("s_1")).join("events.jsonl");
    assert_eq!(fs::read_to_string(&path).unwrap(), "");
    for rx in [&one, &two] {
        let line = next(rx).unwrap();
        assert_eq!(line.kind, "assistant_message_delta");
        assert!(line.seq.is_none());
        let payload: Value = serde_json::Value::Object(line.payload);
        assert_eq!(payload["text"], "e");
    }
    let step = log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(next(&one).unwrap().seq, step.seq);
    assert_eq!(next(&two).unwrap().seq, step.seq);
    let file = fs::read_to_string(&path).unwrap();
    assert_eq!(file.lines().count(), 1);
    assert!(file.contains("step_started"));
    assert!(!file.contains("assistant_message_delta"));
    assert!(!file.contains("session_started"));
}
