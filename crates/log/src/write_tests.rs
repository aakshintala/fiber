use super::*;
use contract::events::{Empty, Event, FiberStarted, TextDelta};

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
    let armed = log.arm(true);
    let between = log.append(&step(), None, None).unwrap();
    let ephemeral = log.append(&delta(), None, None).unwrap();
    let mut watcher = log.finish(armed).unwrap();
    let later = log.append(&step(), None, None).unwrap();

    let mut got = Vec::new();
    loop {
        let line = watcher.recv().unwrap().unwrap();
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
