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
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use common::*;
use contract::Envelope;
use contract::emit::Emit;
use fakes::Deadline;
use log::{Log, Watcher, WeakEmit};
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

#[track_caller]
fn next(rx: &Receiver<Option<Envelope>>, wait: &Deadline) -> Option<Envelope> {
    wait.recv(rx)
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
    let rx = relay(log.watch_all());
    let live = log.append(&empty("step_started"), None, None).unwrap();

    let wait = Deadline::after(DEADLINE);
    assert_eq!(next(&rx, &wait), Some(first));
    assert_eq!(next(&rx, &wait), Some(second));
    assert_eq!(next(&rx, &wait), Some(live));
    drop(log);
    assert_eq!(next(&rx, &wait), None);
    let lines = fs::read_to_string(tmp.session(&id("s_1")).join("events.jsonl")).unwrap();
    assert_eq!(lines.lines().count(), 3);
}

#[test]
fn an_injected_line_arrives_in_push_order_and_is_dropped_when_lagged() {
    let (_tmp, log) = open("inject");
    let watcher = log.watch();
    let injector = watcher.injector();
    let before = log.append(&empty("step_started"), None, None).unwrap();
    let injected = marked("notice");
    injector.push(injected.clone());
    let after = log.append(&empty("step_started"), None, None).unwrap();
    let rx = relay(watcher);
    let wait = Deadline::after(DEADLINE);
    assert_eq!(next(&rx, &wait), Some(before));
    assert_eq!(next(&rx, &wait), Some(injected));
    assert_eq!(next(&rx, &wait), Some(after));

    let lagged = log.watch();
    let lagged_injector = lagged.injector();
    for _ in 0..1_100 {
        log.append(&delta("x"), None, None).unwrap();
    }
    lagged_injector.push(marked("notice"));
    let sentinel = log.append(&session_started(), None, None).unwrap();
    let rx = relay(lagged);
    let mut saw_notice = false;
    let wait = Deadline::after(DEADLINE);
    loop {
        let line = next(&rx, &wait).unwrap();
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
fn a_kept_line_keeps_its_place_among_ordinary_lines() {
    let (_tmp, log) = open("kept-order");
    let watcher = log.watch();
    let injector = watcher.injector();
    injector.push(marked("before"));
    injector.push_kept(marked("kept"));
    injector.push(marked("after"));
    let rx = relay(watcher);
    let wait = Deadline::after(DEADLINE);
    let kinds: Vec<String> = (0..3).map(|_| next(&rx, &wait).unwrap().kind).collect();
    assert_eq!(
        kinds,
        vec!["before".to_owned(), "kept".to_owned(), "after".to_owned()],
        "a kept line stays in push order among ordinary lines"
    );
}

#[test]
fn a_kept_line_pushed_before_a_lag_is_returned_before_catch_up() {
    let (_tmp, log) = open("kept-lag");
    let watcher = log.watch();
    let injector = watcher.injector();
    injector.push_kept(marked("kept"));
    for _ in 0..1_100 {
        log.append(&empty("step_started"), None, None).unwrap();
    }
    injector.push(marked("after"));
    let rx = relay(watcher);
    let mut lines = Vec::new();
    let wait = Deadline::after(DEADLINE);
    for _ in 0..2_000 {
        let line = next(&rx, &wait).expect("the flood arrives before the deadline");
        lines.push(line);
        if lines.iter().filter(|line| line.seq.is_some()).count() == 1_100 {
            break;
        }
    }
    assert!(
        !lines.iter().any(|line| line.kind == "after"),
        "a lagged watcher drops an ordinary line pushed after the kept one"
    );
    assert_eq!(
        lines.first().map(|line| line.kind.as_str()),
        Some("kept"),
        "a kept line pushed before a flood is returned before catch-up"
    );
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
    assert_eq!(next(&rx, &Deadline::after(DEADLINE)), Some(line));
    drop(log);
    assert_eq!(next(&rx, &Deadline::after(DEADLINE)), None);
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
        let line = next(rx, &Deadline::after(DEADLINE)).unwrap();
        assert_eq!(line.kind, "assistant_message_delta");
        assert!(line.seq.is_none());
        let payload: Value = serde_json::Value::Object(line.payload);
        assert_eq!(payload["text"], "e");
    }
    let step = log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(
        next(&one, &Deadline::after(DEADLINE)).unwrap().seq,
        step.seq
    );
    assert_eq!(
        next(&two, &Deadline::after(DEADLINE)).unwrap().seq,
        step.seq
    );
    let file = fs::read_to_string(&path).unwrap();
    assert_eq!(file.lines().count(), 1);
    assert!(file.contains("step_started"));
    assert!(!file.contains("assistant_message_delta"));
    assert!(!file.contains("session_started"));
}

/// A log of `count` durable lines and what each append returned.
fn steps(name: &str, count: usize) -> (common::TestDir, Log, Vec<Envelope>) {
    let (tmp, log) = open(name);
    let written = (0..count)
        .map(|_| log.append(&empty("step_started"), None, None).unwrap())
        .collect();
    (tmp, log, written)
}

#[test]
fn watch_all_meets_an_unparseable_line_in_a_later_page_when_it_gets_there() {
    let (tmp, log, written) = steps("watch-all-bad-later", CAPACITY + 10);
    corrupt(&tmp.session(&id("s_1")), CAPACITY + 5);
    let mut watcher = log.watch_all();
    // Five lines of the failed page come before the bad line, so they are
    // returned before the error.
    for line in &written[..CAPACITY + 5] {
        assert_eq!(watcher.try_recv().unwrap().as_ref(), Some(line));
    }
    let err = watcher.try_recv().unwrap_err();
    assert!(
        err.to_string().contains(&format!("line {}", CAPACITY + 6)),
        "{err}"
    );
    assert_eq!(err.code(), contract::ErrorCode::LogCorrupt);
}

#[test]
fn a_watcher_ends_after_a_read_error_and_never_reads_again() {
    let (tmp, log, written) = steps("watch-all-ended", 5);
    let dir = tmp.session(&id("s_1"));
    let path = dir.join("events.jsonl");
    let whole = fs::read(&path).unwrap();
    corrupt(&dir, 2);
    // Made before the corruption: the log keeps writing to it, and it
    // receives the line appended after the restore.
    let mut live = log.watch();
    let mut watcher = log.watch_all();
    assert_eq!(watcher.try_recv().unwrap().as_ref(), Some(&written[0]));
    assert_eq!(watcher.try_recv().unwrap().as_ref(), Some(&written[1]));
    let err = watcher.try_recv().unwrap_err();
    assert!(err.to_string().contains("line 3"), "{err}");
    assert_eq!(err.code(), contract::ErrorCode::LogCorrupt);
    assert_eq!(watcher.try_recv().unwrap(), None);
    // Calling code that blocks is a wait too (`docs/testing.md`, "Waits
    // and timeouts"): both blocking calls run on the thread owning the
    // watcher, and the test takes each result with its own named deadline.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let first = watcher.recv();
        let second = watcher.recv_timeout(DEADLINE);
        tx.send((first, second, watcher)).unwrap_or(());
    });
    let (first, second, mut watcher) = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("waited until the deadline for the ended watcher's recv");
    assert_eq!(first.unwrap(), None);
    assert_eq!(
        second
            .expect("the ended watcher's recv_timeout returns at once")
            .unwrap(),
        None
    );
    // Restoring the line and appending changes nothing for the ended
    // watcher: it never reads again.
    fs::write(&path, &whole).unwrap();
    let appended = log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(watcher.try_recv().unwrap(), None);
    assert_eq!(live.try_recv().unwrap().as_ref(), Some(&appended));
}

#[test]
fn watch_all_over_a_log_cut_short_returns_the_whole_lines_before_the_cut_then_an_io_error() {
    let (tmp, log, written) = steps("watch-all-cut", 5);
    let dir = tmp.session(&id("s_1"));
    let whole = fs::read(dir.join("events.jsonl")).unwrap();
    let (start, len) = line_at(&whole, 2);
    let cut_off = cut(&dir, start + len / 2);
    let mut watcher = log.watch_all();
    assert_eq!(watcher.try_recv().unwrap().as_ref(), Some(&written[0]));
    assert_eq!(watcher.try_recv().unwrap().as_ref(), Some(&written[1]));
    // The cut leaves a partial line: only whole lines come before the
    // failure, and the failure is the short read.
    let err = watcher.try_recv().unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::IoFailed);
    assert_eq!(watcher.try_recv().unwrap(), None);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    std::io::Write::write_all(&mut file, &cut_off).unwrap();
    assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), whole);
}

/// A log of small lines around three replies of 2 MiB, each larger than a
/// page, and what each append returned.
fn large(name: &str) -> (common::TestDir, Log, Vec<Envelope>) {
    let (tmp, log) = open(name);
    let reply = "x".repeat(2 * 1024 * 1024);
    let mut written = Vec::new();
    for _ in 0..3 {
        written.push(log.append(&empty("step_started"), None, None).unwrap());
        let text = event("text_completed", serde_json::json!({"text": reply}));
        written.push(log.append(&text, None, None).unwrap());
    }
    written.push(log.append(&empty("step_started"), None, None).unwrap());
    (tmp, log, written)
}

#[test]
fn watch_all_seeded_over_lines_larger_than_a_page_yields_the_log_then_seeds_then_live_lines() {
    let (_tmp, log, written) = large("watch-all-seeded-large");
    let tokens = serde_json::json!({"input": 1, "cache_read": 0, "cache_write": {"5m": 0, "1h": 0}, "output": 1});
    let status = event(
        "session_status",
        serde_json::json!({"name": "n", "workspace": "/w", "model": "p/m",
            "state": "idle", "since": 1,
            "spend": {"tokens": tokens, "cost": 0.0, "subscription_cost": 0.0},
            "delegates": 0, "jobs": 0, "project": "-w", "clients": 0}),
    );
    let queue = event(
        "steering_queue",
        serde_json::json!({"messages": [{"content": [{"type": "text", "text": "t"}],
            "source": "driver", "command_id": "c"}]}),
    );
    let ui = event(
        "extension_ui",
        serde_json::json!({"extension": "e", "status": "s"}),
    );
    for seed in [&status, &queue, &ui] {
        log.append(seed, None, None).unwrap();
    }
    // A durable line appended after the subscribe returns but before
    // anything is drained still arrives last: the backlog stops at the
    // table's line count when the first page was read, and the line reaches
    // the watcher through its queue, behind the seeds.
    let watcher = log.watch_all_seeded();
    let live = log.append(&empty("step_started"), None, None).unwrap();
    let rx = relay(watcher);
    let wait = Deadline::after(DEADLINE);
    for line in &written {
        assert_eq!(next(&rx, &wait).as_ref(), Some(line));
    }
    let seeds: Vec<String> = (0..3).map(|_| next(&rx, &wait).unwrap().kind).collect();
    assert_eq!(seeds, ["session_status", "steering_queue", "extension_ui"]);
    assert_eq!(next(&rx, &wait), Some(live));
}

#[test]
fn a_weak_emit_emits_while_the_log_lives_and_never_keeps_it() {
    // Both promises through the public API: a line emitted while the log
    // lives arrives, and the emitter keeps no strong handle.
    let tmp = TestDir::new("weak-emit");
    let log = Arc::new(Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap());
    let emit = WeakEmit::new(&log);
    let weak = Arc::downgrade(&log);
    let rx = relay(log.watch());
    emit.emit(&delta("e"));
    let line = next(&rx, &Deadline::after(DEADLINE)).unwrap();
    assert_eq!(line.kind, "assistant_message_delta");
    drop(log);
    assert!(
        weak.upgrade().is_none(),
        "the emitter holds no strong handle"
    );
    emit.emit(&delta("e"));
}
