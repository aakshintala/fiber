use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use contract::ErrorCode;
use contract::events::{Empty, Event, FiberStarted, Notice, TextDelta};
use serde_json::Value;

const DEADLINE: Duration = Duration::from_secs(10);

fn relay(mut watcher: Watcher) -> mpsc::Receiver<Option<Envelope>> {
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

#[test]
fn watchers_attached_and_dropped_while_idle_leave_nothing_behind() {
    let sessions = fakes::TempDir::new("log-unit-idle");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let kept = log.watch();
    for _ in 0..1000 {
        drop(log.watch());
    }
    // The watcher still held, and the one dropped last, whose queue is
    // already freed: the registry does not grow.
    assert_eq!(log.lock().watchers.len(), 2);
    assert_eq!(
        log.lock()
            .watchers
            .iter()
            .filter(|w| w.strong_count() > 0)
            .count(),
        1
    );
    drop(kept);
    drop(log.watch());
    assert_eq!(log.lock().watchers.len(), 1);
    drop(log);
}

#[test]
fn an_artifact_lands_in_the_session_artifacts_and_a_path_is_refused() {
    let sessions = fakes::TempDir::new("log-unit-artifact");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let (relative, path) = log.write_artifact("a_1.txt", b"full").unwrap();
    assert_eq!(relative, "artifacts/a_1.txt");
    assert_eq!(path, sessions.path().join("s_1/artifacts/a_1.txt"));
    assert_eq!(fs::read(&path).unwrap(), b"full");
    for name in ["../escape.txt", "sub/a.txt", "..", ""] {
        assert!(
            matches!(log.write_artifact(name, b"x"), Err(Error::Io { .. })),
            "{name}"
        );
    }
    assert!(!sessions.path().join("s_1/escape.txt").exists());
    drop(log);
}

fn started() -> Event {
    Event::FiberStarted(FiberStarted {
        version: "0".into(),
        resumed: false,
    })
}

fn step() -> Event {
    Event::StepStarted(Empty {})
}

fn delta() -> Event {
    Event::AssistantMessageDelta(TextDelta { text: "x".into() })
}

/// A line appended after the queue is registered and before the log is read
/// is in both places. It is returned once, in `seq` order, and an ephemeral
/// line written in that window is returned once, after the lines the read saw.
#[test]
fn a_line_written_between_registration_and_the_read_arrives_once() {
    let sessions = fakes::TempDir::new("log-unit-watch-all");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(&started(), None, None).unwrap();
    log.append(&step(), None, None).unwrap();
    let armed = log.arm(true, None);
    let between = log.append(&step(), None, None).unwrap();
    let ephemeral = log.append(&delta(), None, None).unwrap();
    let watcher = log.finish(armed).unwrap();
    let later = log.append(&step(), None, None).unwrap();
    let rx = relay(watcher);

    let mut got = Vec::new();
    loop {
        let line = rx
            .recv_timeout(DEADLINE)
            .expect("the line written between registration and the read arrives")
            .expect("the watcher stays open until that line");
        let done = line.seq == later.seq;
        got.push(line);
        if done {
            break;
        }
    }

    let seqs: Vec<_> = got
        .iter()
        .filter_map(|line| line.seq.map(|seq| seq.0))
        .collect();
    assert_eq!(seqs, [0, 1, between.seq.unwrap().0, later.seq.unwrap().0]);
    assert_eq!(
        got.iter()
            .filter(|line| line.kind == "assistant_message_delta")
            .count(),
        1
    );
    assert_eq!(got.iter().filter(|line| line.seq == between.seq).count(), 1);
    assert!(got.iter().any(|line| line == &ephemeral));
}

fn notice(message: &str) -> Event {
    Event::Notice(Notice {
        code: ErrorCode::IoFailed,
        message: message.to_owned(),
        extension: None,
    })
}

fn payload_message(line: &Envelope) -> Option<&str> {
    line.payload.get("message").and_then(Value::as_str)
}

/// More `notice` lines than the queue holds. The oldest is gone, the newest
/// is kept, and a durable line of another kind is not queued.
#[test]
fn a_latest_watcher_keeps_the_newest_of_its_kind_once_the_queue_is_full() {
    let sessions = fakes::TempDir::new("log-unit-latest");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let watcher = log.watch_latest(&["notice"]);
    for n in 0..1_100 {
        log.append(&notice(&n.to_string()), None, None).unwrap();
    }
    log.append(&step(), None, None).unwrap();
    log.append(&notice("newest"), None, None).unwrap();
    let rx = relay(watcher);
    drop(log);

    let mut messages = Vec::new();
    while let Some(line) = rx
        .recv_timeout(DEADLINE)
        .expect("the latest watcher ends before the deadline")
    {
        assert_ne!(
            line.kind, "step_started",
            "a latest watcher drops other kinds"
        );
        messages.push(payload_message(&line).unwrap().to_owned());
    }
    assert_eq!(messages.last().map(String::as_str), Some("newest"));
    assert!(
        !messages.iter().any(|message| message == "0"),
        "the oldest notice is dropped once the queue is full"
    );
}
