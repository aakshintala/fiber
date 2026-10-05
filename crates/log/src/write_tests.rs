use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use contract::ErrorCode;
use contract::events::{
    Empty, Event, ExtensionsLoaded, FiberStarted, LoadedExtension, Notice, SessionState,
    SessionStatus, TextDelta,
};
use contract::shapes::{Tokens, Usage};
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

#[test]
fn cut_output_splits_on_char_boundaries_and_keeps_the_whole_text() {
    let sessions = fakes::TempDir::new("log-unit-cut");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    // Each é is two bytes. A bound of 3 lands inside the second character.
    let full = "ééééé";
    let (kept, artifact) =
        log.cut_output(full, contract::tool::Bound { start: 3, end: 3 }, "out.txt");
    let artifact = artifact.unwrap();
    assert_eq!(artifact, "artifacts/out.txt");
    let path = sessions.path().join("s_1").join(&artifact);
    assert_eq!(fs::read_to_string(&path).unwrap(), full);
    let mut lines = kept.lines();
    let head = lines.next().unwrap();
    let notice = lines.next().unwrap();
    let tail = lines.next().unwrap();
    assert_eq!(head, "é");
    assert!(notice.contains("bytes cut"), "{notice}");
    assert!(notice.contains(&path.display().to_string()), "{notice}");
    assert_eq!(tail, "é");
    assert!(lines.next().is_none());
}

#[test]
fn cut_output_with_no_tail_does_not_add_a_trailing_newline() {
    let sessions = fakes::TempDir::new("log-unit-cut-end");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let full = "abcdefghij";
    let (kept, artifact) =
        log.cut_output(full, contract::tool::Bound { start: 4, end: 0 }, "out.txt");
    let path = sessions.path().join("s_1").join(artifact.unwrap());
    assert_eq!(fs::read_to_string(&path).unwrap(), full);
    assert_eq!(
        kept,
        format!(
            "abcd\n[6 bytes cut. The full output is in {}; read it with `read`.]",
            path.display()
        )
    );
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
    let armed = log.arm(true);
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
        spend: Usage {
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: std::collections::BTreeMap::new(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        delegates: 0,
        jobs: 0,
    })
}

fn extensions(name: &str) -> Event {
    Event::ExtensionsLoaded(ExtensionsLoaded {
        extensions: vec![LoadedExtension {
            name: name.to_owned(),
            version: "1".into(),
        }],
    })
}

/// No watcher is attached. After more lines than a queue would hold, `latest`
/// is the newest line of each latest-wins kind, and reopening keeps the
/// durable one.
#[test]
fn latest_is_the_newest_line_of_each_kind_with_no_watcher() {
    let sessions = fakes::TempDir::new("log-unit-latest");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let mut last_status = None;
    for n in 0..1_100 {
        log.append(&notice(&n.to_string()), None, None).unwrap();
        last_status = Some(log.append(&status(&format!("s{n}")), None, None).unwrap());
    }
    log.append(&step(), None, None).unwrap();
    let last_extensions = log.append(&extensions("last"), None, None).unwrap();
    assert_eq!(log.latest("session_status").as_ref(), last_status.as_ref());
    assert_eq!(
        log.latest("extensions_loaded").as_ref(),
        Some(&last_extensions)
    );
    assert!(log.latest("notice").is_none());
    assert!(log.latest("step_started").is_none());
    drop(log);
    let reopened = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(
        reopened.latest("extensions_loaded").as_ref(),
        Some(&last_extensions)
    );
    assert!(reopened.latest("session_status").is_none());
}
