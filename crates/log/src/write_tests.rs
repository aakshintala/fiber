use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use contract::ErrorCode;
use contract::events::{
    Clients, Empty, Event, ExtensionsLoaded, FiberStarted, LoadedExtension, Notice, SessionState,
    SessionStatus, TextDelta,
};
use contract::shapes::{Tokens, Usage};
use fakes::Deadline;
const DEADLINE: Duration = Duration::from_secs(10);

fn offer_event(id: &str) -> Event {
    Event::RepositoryCodeOffered(contract::events::RepositoryCodeOffered {
        request_id: contract::RequestId(id.into()),
        items: vec![],
    })
}

fn resolution_event(id: &str) -> Event {
    Event::RepositoryCodeResolved(contract::events::RepositoryCodeResolved {
        request_id: contract::RequestId(id.into()),
        decisions: vec![],
    })
}

fn offer_log() -> (fakes::TempDir, Log) {
    let sessions = fakes::TempDir::new("log-unit-offer");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    (sessions, log)
}

#[test]
fn the_latest_offer_is_kept_until_its_resolution() {
    let (_sessions, log) = offer_log();
    let offered = log.append(&offer_event("r1"), None, None).unwrap();
    assert_eq!(log.latest("repository_code_offered"), Some(offered.clone()));
    log.append(&resolution_event("r2"), None, None).unwrap();
    assert_eq!(log.latest("repository_code_offered"), Some(offered));
    log.append(&resolution_event("r1"), None, None).unwrap();
    assert!(log.latest("repository_code_offered").is_none());
}

#[test]
fn a_second_offer_replaces_the_first() {
    let (_sessions, log) = offer_log();
    log.append(&offer_event("r1"), None, None).unwrap();
    let second = log.append(&offer_event("r2"), None, None).unwrap();
    assert_eq!(log.latest("repository_code_offered"), Some(second));
}

fn offer_line(kind: &str, payload: serde_json::Value) -> Envelope {
    serde_json::from_str(&crate::fixtures::line(kind, "s_1", 1, 0, &payload)).unwrap()
}

#[test]
fn preamble_built_ends_the_pending_offer() {
    let mut latest = BTreeMap::new();
    keep_latest(
        &mut latest,
        &offer_line(
            "repository_code_offered",
            serde_json::json!({"request_id": "r1", "items": []}),
        ),
    );
    assert!(latest.contains_key("repository_code_offered"));
    keep_latest(
        &mut latest,
        &offer_line("preamble_built", serde_json::json!({})),
    );
    assert!(!latest.contains_key("repository_code_offered"));
}

fn reopened_offer(lines: &[(&str, serde_json::Value)]) -> Option<Envelope> {
    let sessions = fakes::TempDir::new("log-unit-reopen-offer");
    let bytes: String = lines
        .iter()
        .enumerate()
        .map(|(seq, (kind, payload))| {
            crate::fixtures::line(kind, "s_1", 1, u64::try_from(seq).unwrap(), payload)
        })
        .collect();
    crate::fixtures::session_log(&sessions.path().join("s_1"), bytes.as_bytes());
    let log = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.latest("repository_code_offered")
}

#[test]
fn a_reopened_log_folds_the_pending_offer() {
    let (sessions, log) = offer_log();
    let offered = log.append(&offer_event("r1"), None, None).unwrap();
    drop(log);
    let log = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(log.latest("repository_code_offered"), Some(offered));
}

#[test]
fn a_reopened_log_folds_a_resolution() {
    assert!(
        reopened_offer(&[
            (
                "repository_code_offered",
                serde_json::json!({"request_id": "r1", "items": []})
            ),
            (
                "repository_code_resolved",
                serde_json::json!({"request_id": "r1", "decisions": []})
            )
        ])
        .is_none()
    );
}

#[test]
fn a_reopened_log_folds_preamble_built_after_an_offer() {
    assert!(
        reopened_offer(&[
            (
                "repository_code_offered",
                serde_json::json!({"request_id": "r1", "items": []})
            ),
            ("preamble_built", serde_json::json!({}))
        ])
        .is_none()
    );
}

#[test]
fn a_line_whose_payload_does_not_parse_is_not_kept_and_removes_nothing() {
    let mut latest = BTreeMap::new();
    let valid = offer_line(
        "repository_code_offered",
        serde_json::json!({"request_id": "r1", "items": []}),
    );
    for kind in ["repository_code_offered", "repository_code_resolved"] {
        keep_latest(&mut latest, &offer_line(kind, serde_json::json!({})));
        assert!(latest.is_empty());
        keep_latest(&mut latest, &valid);
        keep_latest(&mut latest, &offer_line(kind, serde_json::json!({})));
        assert_eq!(latest.get("repository_code_offered"), Some(&valid));
        latest.clear();
    }
}

#[test]
fn open_refuses_an_offer_pair_line_that_fails_the_envelope_schema() {
    for kind in ["repository_code_offered", "repository_code_resolved"] {
        let sessions = fakes::TempDir::new("log-unit-offer-bad-schema");
        let bytes = format!("{{\"kind\":\"{kind}\",\"seq\":0}}\n");
        crate::fixtures::session_log(&sessions.path().join("s_1"), bytes.as_bytes());
        let Err(error) = Log::open(
            sessions.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        ) else {
            panic!("{kind} must meet the envelope schema");
        };
        assert_eq!(error.code(), ErrorCode::LogCorrupt);
        assert!(error.to_string().contains("line 1"));
    }
}

#[test]
fn a_subscribers_seed_does_not_carry_the_offer() {
    let (_sessions, log) = offer_log();
    log.append(&offer_event("r1"), None, None).unwrap();
    assert_eq!(log.lock().kept_seed().count(), 0);
    let seeded = relay(log.watch_all_seeded());
    let after = log.append(&step(), None, None).unwrap();
    let deadline = Deadline::after(DEADLINE);
    let offered = deadline
        .recv_or_fail(&seeded, "the durable offer in history")
        .unwrap();
    assert_eq!(offered.kind, "repository_code_offered");
    let got = deadline
        .recv_or_fail(&seeded, "the next live line without a duplicate offer")
        .unwrap();
    assert_eq!(got, after);
}

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
fn dir_is_the_session_directory_whether_created_or_opened() {
    let sessions = fakes::TempDir::new("log-unit-dir");
    let created = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(created.dir(), sessions.path().join("s_1"));
    drop(created);
    let opened = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(opened.dir(), sessions.path().join("s_1"));
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

#[test]
fn the_opening_boundary_is_fixed_at_the_read_count() {
    let sessions = fakes::TempDir::new("log-opening-boundary");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(log.watch_all().last_seq(), None);
    log.append(&step(), None, None).unwrap();
    assert_eq!(log.watch_all().last_seq(), Some(Seq(0)));
    log.append(&step(), None, None).unwrap();
    let armed = log.arm(true, |_, _| {});
    log.append(&step(), None, None).unwrap();
    let mut watcher = log.finish(armed);
    assert_eq!(watcher.last_seq(), Some(Seq(2)));
    assert_eq!(log.watch().last_seq(), None);
    let total = crate::read::CAPACITY + 1;
    for _ in 0..total {
        log.append(&step(), None, None).unwrap();
    }
    assert_eq!(
        watcher.last_seq(),
        Some(Seq(2)),
        "later appends do not move the boundary"
    );
    for seq in 0..u64::try_from(total + 3).unwrap() {
        assert_eq!(watcher.try_recv().unwrap().unwrap().seq, Some(Seq(seq)));
    }
    assert!(watcher.try_recv().unwrap().is_none());
    assert_eq!(
        watcher.last_seq(),
        Some(Seq(2)),
        "catch-up does not move the boundary"
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
    let armed = log.arm(true, |_, _| {});
    let between = log.append(&step(), None, None).unwrap();
    let ephemeral = log.append(&delta(), None, None).unwrap();
    let watcher = log.finish(armed);
    let later = log.append(&step(), None, None).unwrap();
    let rx = relay(watcher);

    let mut got = Vec::new();
    let wait = Deadline::after(DEADLINE);
    loop {
        let line = wait
            .recv(&rx)
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
        named: false,
        workspace: "/w".into(),
        parent: None,
        model: "m".into(),
        thinking: None,
        state: SessionState::Idle,
        since: 0,
        started: 0,
        calls: 0,
        turns: Some(0),
        step: None,
        git: None,
        context: None,
        cache_expires: None,
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
        project: "-w".into(),
        clients: 0,
        closing: false,
        supervised: false,
    })
}

fn clients(count: u32) -> Event {
    Event::Clients(Clients { count })
}

fn extensions(name: &str) -> Event {
    Event::ExtensionsLoaded(ExtensionsLoaded {
        extensions: vec![LoadedExtension {
            name: name.to_owned(),
            version: "1".into(),
            replaces: Vec::new(),
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
    let mut last_clients = None;
    for n in 0..1_100 {
        log.append(&notice(&n.to_string()), None, None).unwrap();
        last_status = Some(log.append(&status(&format!("s{n}")), None, None).unwrap());
        let count = u32::try_from(n).unwrap_or(u32::MAX);
        last_clients = Some(log.append(&clients(count), None, None).unwrap());
    }
    log.append(&step(), None, None).unwrap();
    let last_extensions = log.append(&extensions("last"), None, None).unwrap();
    assert_eq!(log.latest("session_status").as_ref(), last_status.as_ref());
    assert_eq!(log.latest("clients").as_ref(), last_clients.as_ref());
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
    assert!(reopened.latest("clients").is_none());
}

/// Reopening folds the whole log in one pass: across more lines than a
/// queue holds, `latest` is the newest durable line of its kind, not the
/// first, and `seq` carries on after the last line.
#[test]
fn reopening_a_long_log_keeps_the_newest_line_of_each_kind() {
    let sessions = fakes::TempDir::new("log-unit-reopen-latest");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let mut last = None;
    for n in 0..crate::read::CAPACITY + 10 {
        log.append(&step(), None, None).unwrap();
        if n % 100 == 0 {
            last = Some(log.append(&extensions(&n.to_string()), None, None).unwrap());
        }
    }
    let tail = log.append(&step(), None, None).unwrap();
    drop(log);
    let reopened = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(reopened.latest("extensions_loaded"), last);
    let next = reopened.append(&step(), None, None).unwrap();
    assert_eq!(next.seq.map(|s| s.0), tail.seq.map(|s| s.0 + 1));
    assert_eq!(reopened.count(), next.seq.map_or(0, |s| s.0 + 1));
}

/// Reopening refuses a log with a complete line that does not parse, naming
/// it, and leaves the file as it was.
#[test]
fn reopening_refuses_an_unparseable_line_and_names_it() {
    let sessions = fakes::TempDir::new("log-unit-reopen-bad");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(&step(), None, None).unwrap();
    let path = log.dir().join(crate::EVENTS);
    drop(log);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"not json\n");
    let whole = bytes.clone();
    bytes.extend_from_slice(b"{\"torn");
    fs::write(&path, &bytes).unwrap();
    let Err(err) = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    ) else {
        panic!("opened a log with an unparseable line");
    };
    assert_eq!(err.code(), ErrorCode::LogCorrupt);
    assert!(err.to_string().contains("line 2"), "{err}");
    assert_eq!(
        fs::read(&path).unwrap(),
        [whole, b"{\"torn".to_vec()].concat()
    );
}

/// `try_recv` waits for nothing: with no line it is `None`, with queued
/// lines it returns them in order, and a watcher that overflowed its queue
/// gets every durable line it missed, re-read from the log, then `None`.
#[test]
fn try_recv_returns_what_is_available_including_the_catch_up_and_never_waits() {
    let sessions = fakes::TempDir::new("log-unit-try-recv");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let mut watcher = log.watch();
    assert!(watcher.try_recv().unwrap().is_none());
    let first = log.append(&step(), None, None).unwrap();
    let ephemeral = log.append(&delta(), None, None).unwrap();
    assert_eq!(watcher.try_recv().unwrap(), Some(first));
    assert_eq!(watcher.try_recv().unwrap(), Some(ephemeral));
    assert!(watcher.try_recv().unwrap().is_none());

    // Overflow the queue: nothing was read, so the queue holds `CAPACITY`
    // lines and the rest were dropped.
    let total = crate::read::CAPACITY + 100;
    for _ in 0..total {
        log.append(&step(), None, None).unwrap();
    }
    let mut seqs = Vec::new();
    while let Some(line) = watcher.try_recv().unwrap() {
        seqs.extend(line.seq.map(|seq| seq.0));
    }
    let expected: Vec<u64> = (1..=u64::try_from(total).unwrap()).collect();
    assert_eq!(seqs, expected);
    assert!(watcher.try_recv().unwrap().is_none());
}

fn ui_status(extension: &str, status: &str) -> Event {
    Event::ExtensionUi(contract::events::ExtensionUi {
        extension: extension.to_owned(),
        ui: contract::events::Ui::Status {
            status: status.to_owned(),
        },
    })
}

fn ui_widget(extension: &str, widget: &str, lines: &[&str]) -> Event {
    Event::ExtensionUi(contract::events::ExtensionUi {
        extension: extension.to_owned(),
        ui: contract::events::Ui::Widget {
            widget: widget.to_owned(),
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
        },
    })
}

#[test]
fn newer_status_replaces_older_and_a_clear_removes_it() {
    // Newer status replaces older; a clearing line removes its key; a
    // non-empty update after a clear is kept again.
    let sessions = fakes::TempDir::new("log-unit-ui-status");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(&ui_status("fiber.test/a", "one"), None, None)
        .unwrap();
    log.append(&ui_status("fiber.test/a", "two"), None, None)
        .unwrap();
    let seeded = log.watch_all_seeded();
    let rx = relay(seeded);
    let first = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the seeded status arrives")
        .expect("open");
    assert_eq!(first.kind, "extension_ui");
    log.append(&ui_status("fiber.test/a", ""), None, None)
        .unwrap();
    let log2 = log;
    let seeded = log2.watch_all_seeded();
    let rx = relay(seeded);
    // A clearing line removes its key: no `extension_ui` seed follows.
    let mut seen_ui = false;
    let wait = Deadline::after(Duration::from_millis(200));
    for _ in 0..16 {
        match wait.recv(&rx) {
            Ok(Some(line)) if line.kind == "extension_ui" => {
                seen_ui = true;
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(!seen_ui, "a cleared status leaves no seed");
    log2.append(&ui_status("fiber.test/a", "back"), None, None)
        .unwrap();
    let seeded = log2.watch_all_seeded();
    let rx = relay(seeded);
    let line = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the status after a clear arrives")
        .expect("open");
    assert_eq!(line.kind, "extension_ui");
}

#[test]
fn widgets_are_kept_per_extension_and_id() {
    let sessions = fakes::TempDir::new("log-unit-ui-widget");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(&ui_widget("fiber.test/a", "m", &["x"]), None, None)
        .unwrap();
    log.append(&ui_widget("fiber.test/a", "n", &["y"]), None, None)
        .unwrap();
    log.append(&ui_widget("fiber.test/b", "m", &["z"]), None, None)
        .unwrap();
    // Empty lines remove only that widget.
    log.append(&ui_widget("fiber.test/a", "m", &[]), None, None)
        .unwrap();
    let seeded = log.watch_all_seeded();
    let rx = relay(seeded);
    let mut widgets = Vec::new();
    let wait = Deadline::after(Duration::from_millis(200));
    for _ in 0..8 {
        match wait.recv(&rx) {
            Ok(Some(line)) if line.kind == "extension_ui" => widgets.push(line),
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert_eq!(
        widgets.len(),
        2,
        "only the two live widgets seed: {widgets:?}"
    );
}

#[test]
fn watch_all_seeded_delivers_seeds_before_later_lines() {
    // The seed lines come off the watcher before a line appended after it returns.
    let sessions = fakes::TempDir::new("log-unit-seeded-order");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(&status("idle"), None, None).unwrap();
    log.append(&ui_status("fiber.test/a", "syncing"), None, None)
        .unwrap();
    let seeded = log.watch_all_seeded();
    let rx = relay(seeded);
    log.append(&ui_status("fiber.test/a", "later"), None, None)
        .unwrap();
    let wait = Deadline::after(DEADLINE);
    let first = wait
        .recv(&rx)
        .expect("the first seed arrives")
        .expect("open");
    assert_eq!(first.kind, "session_status");
    let second = wait
        .recv(&rx)
        .expect("the second seed arrives")
        .expect("open");
    assert_eq!(second.kind, "extension_ui");
    let payload: contract::events::ExtensionUi =
        serde_json::from_value(serde_json::Value::Object(second.payload.clone())).unwrap();
    assert_eq!(
        payload.ui,
        contract::events::Ui::Status {
            status: "syncing".to_owned(),
        }
    );
}

#[test]
fn watch_all_seeded_prunes_a_dropped_watcher_and_keeps_a_live_one() {
    // The `>` becoming `==`, `<` or `>=` must fail: `>=` keeps the dead
    // entry, so the registry grows; `==` and `<` drop the live watcher, so
    // it never receives a later line.
    let sessions = fakes::TempDir::new("log-unit-seeded-prune");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let live = log.watch();
    {
        let dead = log.watch();
        assert_eq!(log.lock().watchers.len(), 2);
        drop(dead);
    }
    assert_eq!(log.lock().watchers.len(), 2);
    assert_eq!(
        log.lock()
            .watchers
            .iter()
            .filter(|w| w.strong_count() > 0)
            .count(),
        1
    );
    let seeded = log.watch_all_seeded();
    assert_eq!(
        log.lock().watchers.len(),
        2,
        "the dead weak entry is pruned"
    );
    assert_eq!(
        log.lock()
            .watchers
            .iter()
            .filter(|w| w.strong_count() > 0)
            .count(),
        2,
        "the live watcher is kept"
    );
    let live_rx = relay(live);
    let seeded_rx = relay(seeded);
    let line = log.append(&step(), None, None).unwrap();
    let got = Deadline::after(DEADLINE)
        .recv(&live_rx)
        .expect("the live watcher still receives a later line")
        .expect("open");
    assert_eq!(got.seq, line.seq);
    let got = Deadline::after(DEADLINE)
        .recv(&seeded_rx)
        .expect("the reseeded watcher receives a later line")
        .expect("open");
    assert_eq!(got.seq, line.seq);
}

#[test]
fn range_and_count_return_while_append_is_in_its_fsync() {
    let sessions = fakes::TempDir::new("log-unit-fsync-race");
    let log = Arc::new(
        Log::create(
            sessions.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap(),
    );
    log.append(&step(), None, None).unwrap();
    log.append(&step(), None, None).unwrap();
    let (in_fsync_tx, in_fsync_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    let appender_log = Arc::clone(&log);
    thread::spawn(move || {
        before_sync(move || {
            in_fsync_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        let appended = appender_log.append(&Event::AssistantMessageStarted(Empty {}), None, None);
        done_tx.send(appended).unwrap();
    });
    let wait = Deadline::after(DEADLINE);
    wait.recv(&in_fsync_rx)
        .expect("the appender reaches its fsync");
    let reader_log = Arc::clone(&log);
    let (read_tx, read_rx) = mpsc::channel();
    thread::spawn(move || {
        let count = reader_log.count();
        let range = reader_log.range(0, 10);
        read_tx.send((count, range)).unwrap();
    });
    let (count, range) = wait
        .recv(&read_rx)
        .expect("count and range return during the fsync");
    assert_eq!(count, 3);
    let range = range.unwrap();
    assert_eq!(range.len(), 3);
    assert_eq!(range[2].kind, "assistant_message_started");
    release_tx.send(()).unwrap();
    let appended = wait
        .recv(&done_rx)
        .expect("the appender finishes after release")
        .unwrap();
    assert_eq!(appended, range[2]);
}

use crate::fixtures::{line as fixture_line, session_log};
use serde_json::json;

#[test]
fn open_fully_parses_only_the_lines_it_folds() {
    let sessions = fakes::TempDir::new("log-unit-open-count");
    let dir = sessions.path().join("s_1");
    let mut bytes = Vec::new();
    let mut folded = 0;
    let mut last_extensions_seq = 0;
    for seq in 0..100_000u64 {
        let (kind, payload, action) = if seq % 10_000 == 0 {
            folded += 1;
            ("usage_recorded", json!({}), Some(format!("a_{seq}")))
        } else if seq % 5_000 == 0 {
            folded += 1;
            ("preamble_built", json!({}), None)
        } else if seq % 1_000 == 0 {
            folded += 1;
            last_extensions_seq = seq;
            ("extensions_loaded", json!({"extensions": []}), None)
        } else {
            ("turn_started", json!({}), None)
        };
        if let Some(action) = action {
            let line = json!({
                "kind": kind,
                "session_id": "s_1",
                "ts": 1,
                "schema_version": 1,
                "action_id": action,
                "seq": seq,
                "payload": payload,
            });
            bytes.extend_from_slice(format!("{line}\n").as_bytes());
        } else {
            bytes.extend_from_slice(fixture_line(kind, "s_1", 1, seq, &payload).as_bytes());
        }
    }
    session_log(&dir, &bytes);
    let before = full_parses();
    let log = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(full_parses() - before, folded);
    assert_eq!(log.count(), 100_000);
    let latest = log.latest("extensions_loaded").unwrap();
    assert_eq!(latest.seq.map(|s| s.0), Some(last_extensions_seq));
    let next = log.append(&step(), None, None).unwrap();
    assert_eq!(next.seq.map(|s| s.0), Some(100_000));
}

#[test]
fn open_opens_a_line_of_an_unfolded_kind_that_fails_the_envelope_schema() {
    let sessions = fakes::TempDir::new("log-unit-open-unfolded-bad");
    let dir = sessions.path().join("s_1");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(fixture_line("turn_started", "s_1", 1, 0, &json!({})).as_bytes());
    bytes.extend_from_slice(br#"{"kind":"turn_started","seq":1}"#.as_slice());
    bytes.push(b'\n');
    session_log(&dir, &bytes);
    let log = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(log.count(), 2);
    let err = log.range(1, 1).unwrap_err();
    assert_eq!(err.code(), ErrorCode::LogCorrupt);
    assert!(err.to_string().contains("line 2"), "{err}");
}

#[test]
fn open_refuses_a_folded_line_that_fails_the_envelope_schema() {
    let sessions = fakes::TempDir::new("log-unit-open-folded-bad");
    let dir = sessions.path().join("s_1");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(fixture_line("turn_started", "s_1", 1, 0, &json!({})).as_bytes());
    bytes.extend_from_slice(br#"{"kind":"extensions_loaded","seq":1}"#.as_slice());
    bytes.push(b'\n');
    session_log(&dir, &bytes);
    let whole = std::fs::read(dir.join(crate::EVENTS)).unwrap();
    let Err(err) = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    ) else {
        panic!("a folded line that fails the envelope schema is refused");
    };
    assert_eq!(err.code(), ErrorCode::LogCorrupt);
    assert!(err.to_string().contains("line 2"), "{err}");
    assert_eq!(std::fs::read(dir.join(crate::EVENTS)).unwrap(), whole);
}

#[test]
fn open_refuses_a_line_with_no_seq_or_no_kind() {
    for (name, second) in [
        (
            "no-seq",
            r#"{"kind":"turn_started","session_id":"s","ts":1,"schema_version":1,"payload":{}}"#,
        ),
        (
            "no-kind",
            r#"{"session_id":"s","ts":1,"schema_version":1,"seq":1,"payload":{}}"#,
        ),
    ] {
        let sessions = fakes::TempDir::new(&format!("log-unit-open-{name}"));
        let dir = sessions.path().join("s_1");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(fixture_line("turn_started", "s_1", 1, 0, &json!({})).as_bytes());
        bytes.extend_from_slice(second.as_bytes());
        bytes.push(b'\n');
        session_log(&dir, &bytes);
        let Err(err) = Log::open(
            sessions.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        ) else {
            panic!("a line with no seq or no kind is refused: {name}");
        };
        assert_eq!(err.code(), ErrorCode::LogCorrupt, "{name}");
        assert!(err.to_string().contains("line 2"), "{name}: {err}");
    }
}

#[test]
fn open_refuses_a_line_that_is_not_utf8() {
    let sessions = fakes::TempDir::new("log-unit-open-not-utf8");
    let dir = sessions.path().join("s_1");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(fixture_line("turn_started", "s_1", 1, 0, &json!({})).as_bytes());
    bytes.extend_from_slice(
        b"{\"kind\":\"turn_started\",\"session_id\":\"s_1\",\"ts\":1,\"schema_version\":1,\"seq\":1,\"payload\":{\"text\":\"",
    );
    bytes.push(0xff);
    bytes.extend_from_slice(b"\"}}\n");
    session_log(&dir, &bytes);
    let whole = std::fs::read(dir.join(crate::EVENTS)).unwrap();
    let Err(err) = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    ) else {
        panic!("a line that is not UTF-8 is refused");
    };
    assert_eq!(err.code(), ErrorCode::LogCorrupt);
    assert!(err.to_string().contains("line 2"), "{err}");
    assert_eq!(std::fs::read(dir.join(crate::EVENTS)).unwrap(), whole);
}

#[test]
fn open_refuses_a_line_whose_seq_leaves_no_next() {
    let sessions = fakes::TempDir::new("log-unit-open-seq-max");
    let dir = sessions.path().join("s_1");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(fixture_line("turn_started", "s_1", 1, 0, &json!({})).as_bytes());
    bytes.extend_from_slice(fixture_line("turn_started", "s", 1, u64::MAX, &json!({})).as_bytes());
    session_log(&dir, &bytes);
    let whole = std::fs::read(dir.join(crate::EVENTS)).unwrap();
    let Err(err) = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    ) else {
        panic!("a line whose seq leaves no next seq is refused");
    };
    assert_eq!(err.code(), ErrorCode::LogCorrupt);
    assert!(err.to_string().contains("line 2"), "{err}");
    assert_eq!(std::fs::read(dir.join(crate::EVENTS)).unwrap(), whole);
}

#[test]
fn open_reads_an_escaped_kind() {
    let sessions = fakes::TempDir::new("log-unit-open-escaped");
    let dir = sessions.path().join("s_1");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(fixture_line("turn_started", "s_1", 1, 0, &json!({})).as_bytes());
    let escaped = r#"{"kind":"extensions\u005floaded","session_id":"s_1","ts":1,"schema_version":1,"seq":1,"payload":{"extensions":[]}}"#;
    bytes.extend_from_slice(escaped.as_bytes());
    bytes.push(b'\n');
    session_log(&dir, &bytes);
    let raw = std::fs::read(dir.join(crate::EVENTS)).unwrap();
    assert!(
        !raw.windows(b"extensions_loaded".len())
            .any(|w| w == b"extensions_loaded"),
        "the file holds the kind escaped"
    );
    let before = full_parses();
    let log = Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let latest = log.latest("extensions_loaded").unwrap();
    assert_eq!(latest.seq.map(|s| s.0), Some(1));
    assert_eq!(full_parses() - before, 1);
}

#[test]
fn cut_notice_names_the_saved_file_or_the_failure() {
    let path = std::path::Path::new("/s/s_1/artifacts/out.txt");
    assert_eq!(
        cut_notice(5, Ok(path)),
        "[5 bytes cut. The full output is in /s/s_1/artifacts/out.txt; read it with `read`.]"
    );
    assert_eq!(
        cut_notice(5, Err("disk full")),
        "[5 bytes cut. The full output could not be saved: disk full.]"
    );
    assert_eq!(
        cut_notice(0, Ok(path)),
        "[0 bytes cut. The full output is in /s/s_1/artifacts/out.txt; read it with `read`.]"
    );
}

#[test]
fn cut_joined_puts_the_notice_between_the_ends() {
    assert_eq!(
        cut_joined("head", "[5 bytes cut.]", "tail"),
        "head\n[5 bytes cut.]\ntail"
    );
}

#[test]
fn cut_joined_with_no_tail_adds_no_trailing_newline() {
    assert_eq!(
        cut_joined("head", "[5 bytes cut.]", ""),
        "head\n[5 bytes cut.]"
    );
}

/// Records each offered `(&Event, &Envelope)` pair's kind and seq.
fn record(tx: &mpsc::Sender<(String, Option<u64>)>) -> Arc<super::Subscriber> {
    let tx = tx.clone();
    Arc::new(move |event: &Event, line: &Envelope| {
        let _ignored = tx.send((event.kind().to_owned(), line.seq.map(|seq| seq.0)));
    })
}

#[test]
fn a_subscriber_is_offered_every_durable_append_with_its_seq() {
    let (_sessions, log) = offer_log();
    let (tx, rx) = mpsc::channel();
    let _guard = log.subscribe(record(&tx));
    let first = log
        .append(&Event::StepStarted(Empty {}), None, None)
        .unwrap();
    let second = log
        .append(&Event::StepStarted(Empty {}), None, None)
        .unwrap();
    assert_eq!(
        Deadline::after(DEADLINE).recv(&rx).unwrap(),
        ("step_started".to_owned(), first.seq.map(|seq| seq.0))
    );
    assert_eq!(
        Deadline::after(DEADLINE).recv(&rx).unwrap(),
        ("step_started".to_owned(), second.seq.map(|seq| seq.0))
    );
    assert!(first.seq.is_some() && second.seq.is_some());
    assert_ne!(first.seq, second.seq);
}

#[test]
fn a_subscriber_is_offered_ephemeral_appends_with_no_seq() {
    let (_sessions, log) = offer_log();
    let (tx, rx) = mpsc::channel();
    let _guard = log.subscribe(record(&tx));
    log.append(
        &Event::Notice(Notice {
            code: ErrorCode::ExtensionFailed,
            message: "boom".to_owned(),
            extension: None,
        }),
        None,
        None,
    )
    .unwrap();
    // An ephemeral offer carries no seq: the cursor has nothing to advance to.
    assert_eq!(
        Deadline::after(DEADLINE).recv(&rx).unwrap(),
        ("notice".to_owned(), None)
    );
}

#[test]
fn every_live_subscriber_is_offered_each_append() {
    let (_sessions, log) = offer_log();
    let (first_tx, first_rx) = mpsc::channel();
    let (second_tx, second_rx) = mpsc::channel();
    let _first = log.subscribe(record(&first_tx));
    let _second = log.subscribe(record(&second_tx));
    log.append(&Event::StepStarted(Empty {}), None, None)
        .unwrap();
    assert_eq!(
        Deadline::after(DEADLINE).recv(&first_rx).unwrap().0,
        "step_started".to_owned()
    );
    assert_eq!(
        Deadline::after(DEADLINE).recv(&second_rx).unwrap().0,
        "step_started".to_owned()
    );
}

#[test]
fn a_dropped_subscription_gets_no_further_offer() {
    let (_sessions, log) = offer_log();
    let (tx, rx) = mpsc::channel();
    let guard = log.subscribe(record(&tx));
    log.append(&Event::StepStarted(Empty {}), None, None)
        .unwrap();
    assert_eq!(
        Deadline::after(DEADLINE).recv(&rx).unwrap().0,
        "step_started".to_owned()
    );
    drop(guard);
    // The drop unregisters once this append prunes the dead entry: the
    // other subscriber still gets its offer, the dropped one gets nothing.
    let (live_tx, live_rx) = mpsc::channel();
    let _live = log.subscribe(record(&live_tx));
    log.append(&Event::StepStarted(Empty {}), None, None)
        .unwrap();
    assert_eq!(
        Deadline::after(DEADLINE).recv(&live_rx).unwrap().0,
        "step_started".to_owned()
    );
    assert!(rx.try_recv().is_err());
}

fn ended_log(name: &str) -> (fakes::TempDir, Log) {
    let sessions = fakes::TempDir::new(name);
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    (sessions, log)
}

#[test]
fn end_watchers_ends_a_watcher_after_its_queued_lines_and_keeps_the_lock() {
    let (sessions, log) = ended_log("log-unit-end-watchers");
    let mut watcher = log.watch();
    log.append(&status("idle"), None, None).unwrap();
    log.end_watchers();
    assert!(
        matches!(
            Log::open(
                sessions.path(),
                SessionId("s_1".into()),
                fakes::clock::FakeClock::new()
            ),
            Err(Error::Held { .. })
        ),
        "the session lock stays held"
    );
    let queued = watcher
        .recv_timeout(DEADLINE)
        .expect("the queued line arrives")
        .unwrap()
        .expect("the queued line first");
    assert_eq!(queued.kind, "session_status");
    assert!(
        watcher
            .recv_timeout(DEADLINE)
            .expect("the end arrives")
            .unwrap()
            .is_none(),
        "then the end"
    );
    // Idempotent.
    log.end_watchers();
    assert!(
        watcher
            .recv_timeout(DEADLINE)
            .expect("the end still arrives")
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_watcher_registered_after_end_watchers_starts_ended() {
    let (_sessions, log) = ended_log("log-unit-end-late-watch");
    log.append(&started(), None, None).unwrap();
    log.append(&status("idle"), None, None).unwrap();
    log.end_watchers();
    let mut plain = log.watch();
    assert!(
        plain
            .recv_timeout(DEADLINE)
            .expect("the late watcher starts ended")
            .unwrap()
            .is_none()
    );
    let mut all = log.watch_all();
    assert_eq!(
        all.recv_timeout(DEADLINE)
            .expect("the first durable line arrives")
            .unwrap()
            .expect("seq 0")
            .kind,
        "fiber_started"
    );
    assert!(
        all.recv_timeout(DEADLINE)
            .expect("the end arrives after the durable lines")
            .unwrap()
            .is_none()
    );
    let mut seeded = log.watch_all_seeded();
    let mut kinds = Vec::new();
    while let Some(line) = seeded
        .recv_timeout(DEADLINE)
        .expect("the seeded lines arrive before the end")
        .unwrap()
    {
        kinds.push(line.kind);
    }
    assert_eq!(kinds, ["fiber_started", "session_status"]);
}

#[test]
fn dropping_the_log_still_ends_the_watchers() {
    let (_sessions, log) = ended_log("log-unit-drop-ends");
    let mut watcher = log.watch();
    drop(log);
    assert!(
        watcher
            .recv_timeout(DEADLINE)
            .expect("dropping the log ends its watchers")
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_append_after_the_end_writes_nothing_and_returns_ended() {
    let (sessions, log) = ended_log("log-unit-append-after-end");
    let mut watcher = log.watch();
    log.append(&status("idle"), None, None).unwrap();
    log.end_watchers();
    let events = sessions.path().join("s_1").join("events.jsonl");
    let length = std::fs::metadata(&events).unwrap().len();
    let count = log.count();
    let latest = log.latest("session_status");
    let steering = log.latest("steering_queue");
    for event in [
        status("running"),
        Event::SteeringQueue(contract::events::SteeringQueue { messages: vec![] }),
    ] {
        let error = log.append(&event, None, None).unwrap_err();
        assert!(matches!(error, Error::Ended { .. }), "{error:?}");
        assert_eq!(error.code(), ErrorCode::Closing);
        assert_eq!(
            error.to_string(),
            "session s_1 has ended; nothing more is written to it"
        );
    }
    assert_eq!(std::fs::metadata(&events).unwrap().len(), length);
    assert_eq!(log.count(), count);
    assert_eq!(log.latest("session_status"), latest);
    assert_eq!(log.latest("steering_queue"), steering);
    assert_eq!(
        watcher.recv().unwrap().expect("the line before").kind,
        "session_status"
    );
    assert!(
        watcher
            .recv_timeout(DEADLINE)
            .expect("the end arrives")
            .unwrap()
            .is_none(),
        "no new line before the end"
    );
}

fn artifact_log(name: &str) -> (fakes::TempDir, Log) {
    let sessions = fakes::TempDir::new(name);
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    (sessions, log)
}

#[test]
fn replace_artifact_at_replaces_one_direct_child() {
    let (sessions, log) = artifact_log("log-unit-replace-at");
    let path = sessions.path().join("s_1/artifacts/a.txt");
    fs::write(&path, b"old").unwrap();
    log.replace_artifact_at(&path, b"new").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new");
}

#[test]
fn replace_artifact_at_creates_a_missing_direct_child() {
    let (sessions, log) = artifact_log("log-unit-replace-at-missing");
    let path = sessions.path().join("s_1/artifacts/a.txt");
    log.replace_artifact_at(&path, b"new").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new");
}

#[test]
fn replace_and_remove_refuse_paths_outside_artifacts_and_leave_them() {
    let (sessions, log) = artifact_log("log-unit-at-refused");
    let dir = sessions.path().join("s_1");
    fs::create_dir_all(dir.join("artifacts/sub")).unwrap();
    let nested = dir.join("artifacts/sub/out.log");
    fs::write(&nested, b"kept").unwrap();
    let events = dir.join("events.jsonl");
    let before = fs::read(&events).unwrap();
    let elsewhere = sessions.path().join("elsewhere.txt");
    for bad in [
        nested.clone(),
        events.clone(),
        dir.join("artifacts/../events.jsonl"),
        dir.join("artifacts/."),
        dir.join("artifacts/.."),
        dir.join("artifacts"),
        elsewhere.clone(),
    ] {
        let replaced = log.replace_artifact_at(&bad, b"x");
        assert!(
            matches!(&replaced, Err(Error::Io { path, .. }) if path == &bad),
            "{bad:?}: {replaced:?}"
        );
        assert_eq!(replaced.unwrap_err().code(), ErrorCode::IoFailed);
        let removed = log.remove_artifact_at(&bad);
        assert!(
            matches!(&removed, Err(Error::Io { path, .. }) if path == &bad),
            "{bad:?}: {removed:?}"
        );
        assert_eq!(removed.unwrap_err().code(), ErrorCode::IoFailed);
    }
    assert_eq!(fs::read(&nested).unwrap(), b"kept");
    assert_eq!(fs::read(&events).unwrap(), before);
    assert!(!elsewhere.exists());
}

#[test]
fn remove_artifact_at_removes_one_direct_child_and_names_a_missing_one() {
    let (sessions, log) = artifact_log("log-unit-remove-at");
    let path = sessions.path().join("s_1/artifacts/gone.txt");
    fs::write(&path, b"x").unwrap();
    log.remove_artifact_at(&path).unwrap();
    assert!(!path.exists());
    let missing = sessions.path().join("s_1/artifacts/missing.txt");
    let error = log.remove_artifact_at(&missing).unwrap_err();
    assert!(
        matches!(&error, Error::Io { path, .. } if path == &missing),
        "{error:?}"
    );
    assert_eq!(error.code(), ErrorCode::IoFailed);
}

fn moved_event() -> Event {
    Event::Moved(contract::events::Moved {
        new_session_id: SessionId("s_new".into()),
        seq: Seq(3),
        workspace: "/work/other".into(),
    })
}

#[test]
fn a_failed_pre_write_barrier_writes_no_moved() {
    after_sync(|| None);
    let (sessions, log) = offer_log();
    after_sync(|| Some(std::io::Error::other("no barrier sync")));
    let failed = log.append(&moved_event(), None, None);
    assert!(
        matches!(&failed, Err(crate::Error::Io { .. })),
        "{failed:?}"
    );
    assert_eq!(failed.unwrap_err().code(), ErrorCode::IoFailed);
    assert_eq!(
        crate::read(&sessions.path().join("s_1")).unwrap(),
        Vec::new(),
        "the barrier failure writes nothing"
    );
    let later = log.append(&moved_event(), None, None);
    assert!(
        matches!(&later, Err(crate::Error::Poisoned { .. })),
        "{later:?}"
    );
    after_sync(|| None);
}

#[test]
fn a_failed_moved_sync_keeps_its_line_without_truncation() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    after_sync(|| None);
    let (sessions, log) = offer_log();
    let dir = sessions.path().join("s_1");
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    after_sync(move || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            None
        } else {
            Some(std::io::Error::other("no moved sync"))
        }
    });
    let failed = log.append(&moved_event(), None, None);
    assert!(
        matches!(&failed, Err(crate::Error::Io { .. })),
        "{failed:?}"
    );
    assert_eq!(failed.unwrap_err().code(), ErrorCode::IoFailed);
    // The failure sticks, as after any failed write.
    let later = log.append(&moved_event(), None, None);
    assert!(
        matches!(&later, Err(crate::Error::Poisoned { .. })),
        "{later:?}"
    );
    // The line was written before its sync failed: reopening keeps it,
    // with no truncation, so the hub starts the same child from it.
    let lines = crate::read(&dir).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].kind, "moved");
    drop(log);
    after_sync(|| None);
    Log::open(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(crate::read(&dir).unwrap().len(), 1);
    after_sync(|| None);
}
