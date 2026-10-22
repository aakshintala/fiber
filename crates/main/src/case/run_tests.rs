//! Driving a session case and recording its verdict (`docs/testing.md`, "Testing an extension").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures and case-run assertions fail with their verdict"
)]

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant, SystemTime};

use contract::SessionId;
use contract::clock::{Clock, Wake};
use contract::events::{Event, FiberExited, TextDelta, TurnCompleted, TurnOutcome};
use contract::shapes::{Tokens, Usage};
use log::Log;
use serde_json::{Value, json};

use super::super::format::{ClockAdvance, Host, Selector, SessionCase};
use super::{CaseRun, WaitBounds, session_verdict, write_config};
use fakes::Deadline;

const LOOP_WAIT: Duration = Duration::from_millis(20);
const EXIT_WAIT: Duration = Duration::from_millis(2);
const ADVANCE_WAIT: Duration = Duration::from_millis(20);
const TEST_WAITS: WaitBounds = WaitBounds {
    advance: ADVANCE_WAIT,
    until: LOOP_WAIT,
    close: EXIT_WAIT,
};

fn line(kind: &str, seq: Option<u64>, payload: Value) -> Value {
    let mut value = json!({"kind": kind, "payload": payload});
    if let Some(seq) = seq {
        value["seq"] = json!(seq);
    }
    value
}

#[test]
fn equal_ordered_durable_lines_match_the_expected_subset() {
    let expected = [
        json!({"kind": "turn_started"}),
        json!({"kind": "turn_completed", "payload": {"outcome": "completed"}}),
    ];
    let actual = [
        line("turn_started", Some(4), json!({"extra": true})),
        line(
            "turn_completed",
            Some(5),
            json!({"outcome": "completed", "extra": 1}),
        ),
    ];
    assert!(session_verdict(&expected, &actual, &[]).is_empty());
}

#[test]
fn a_missing_or_extra_line_fails_with_its_index() {
    let expected = [
        json!({"kind": "turn_started"}),
        json!({"kind": "turn_completed"}),
    ];
    let missing = [line("turn_started", Some(1), json!({}))];
    let error = session_verdict(&expected, &missing, &[]).join("\n");
    assert!(error.contains("expect[1]"), "{error}");

    let extra = [
        line("turn_started", Some(1), json!({})),
        line("turn_completed", Some(2), json!({})),
        line("fiber_exited", Some(3), json!({})),
    ];
    let error = session_verdict(&expected, &extra, &[]).join("\n");
    assert!(error.contains("event[2]"), "{error}");
}

#[test]
fn reordered_and_wrong_fields_name_the_first_mismatch() {
    let expected = [
        json!({"kind": "turn_started"}),
        json!({"kind": "turn_completed", "payload": {"outcome": "completed"}}),
    ];
    let swapped = [
        line("turn_completed", Some(1), json!({"outcome": "completed"})),
        line("turn_started", Some(2), json!({})),
    ];
    let error = session_verdict(&expected, &swapped, &[]).join("\n");
    assert!(error.contains("expect[0].kind"), "{error}");

    let wrong = [
        line("turn_started", Some(1), json!({})),
        line("turn_completed", Some(2), json!({"outcome": "failed"})),
    ];
    let error = session_verdict(&expected, &wrong, &[]).join("\n");
    assert!(error.contains("expect[1].payload.outcome"), "{error}");
}

#[test]
fn ephemeral_lines_are_dropped_and_host_misses_fail_the_case() {
    let expected = [json!({"kind": "turn_completed"})];
    let lines = [
        line("assistant_message_delta", None, json!({"text": "fragment"})),
        line("turn_completed", Some(1), json!({})),
    ];
    assert!(session_verdict(&expected, &lines, &[]).is_empty());

    let unmet = ["host.http[1] miss: request {\"url\":\"https://example.test\"}".to_owned()];
    let error = session_verdict(&expected, &lines, &unmet).join("\n");
    assert!(error.contains("host.http[1]"), "{error}");
}

fn session_case(
    name: Option<String>,
    expected: Vec<Value>,
    host: Host,
    advances: Vec<ClockAdvance>,
    until: Option<Selector>,
    attended: bool,
) -> SessionCase {
    SessionCase {
        name,
        script: json!({}),
        prompt: String::new(),
        config: None,
        settings: None,
        host,
        clock: advances,
        until,
        expect: expected,
        attended,
    }
}

fn case_run(
    expected: Vec<Value>,
    advances: Vec<ClockAdvance>,
    until: Option<Selector>,
) -> Arc<CaseRun> {
    case_run_with_host(expected, Host::default(), advances, until)
}

fn case_run_with_host(
    expected: Vec<Value>,
    host: Host,
    advances: Vec<ClockAdvance>,
    until: Option<Selector>,
) -> Arc<CaseRun> {
    CaseRun::with_waits(
        session_case(
            Some("run-loop".to_owned()),
            expected,
            host,
            advances,
            until,
            true,
        ),
        fakes::clock::FakeClock::new(),
        TEST_WAITS,
    )
}

/// The runner carries the parsed `attended` value: flipping it to either
/// constant breaks the session setup's answerable computation, so both
/// values are pinned here (mutant `CaseRun::attended -> bool`).
#[test]
fn case_run_attended_carries_the_parsed_value() {
    for attended in [true, false] {
        let case = CaseRun::with_waits(
            session_case(
                Some("run-loop".to_owned()),
                Vec::new(),
                Host::default(),
                Vec::new(),
                None,
                attended,
            ),
            fakes::clock::FakeClock::new(),
            TEST_WAITS,
        );
        assert_eq!(case.attended(), attended);
    }
}

/// A session case without a name runs under the default: flipping the
/// fallback to anything else renames every unnamed case, so the empty
/// default is pinned here.
#[test]
fn case_run_without_a_name_uses_the_default() {
    let case = CaseRun::with_waits(
        session_case(None, Vec::new(), Host::default(), Vec::new(), None, true),
        fakes::clock::FakeClock::new(),
        TEST_WAITS,
    );
    assert_eq!(case.name, String::new());
}

fn hang_exec_entry(program: &str) -> extensions::ExecEntry {
    extensions::ExecEntry {
        request: json!({"program": program}),
        reply: Ok(extensions::ExecReply::Hang),
    }
}

/// Parks a waiter on the case clock at `until`: the closure runs once the
/// park registers, so the waiter is parked before this returns. The waiter
/// stays parked until the test releases it.
fn park_waiter(
    case: &Arc<CaseRun>,
    until: Instant,
) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let waiter_clock = case.session_clock();
    let handle = std::thread::spawn(move || {
        waiter_clock.wait_until(Some(until), &mut |_| {
            let _ignored = parked_tx.send(());
            let _ignored = Deadline::after(Duration::from_secs(5)).recv(&release_rx);
        });
    });
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_rx)
        .expect("the waiter parks before the test moves on");
    (release_tx, handle)
}

#[test]
fn a_hang_advance_holds_until_its_entry_matches() {
    let host = Host {
        exec: vec![hang_exec_entry("hang-once")],
        ..Host::default()
    };
    let case = case_run_with_host(
        vec![json!({"kind": "turn_completed"})],
        host,
        vec![ClockAdvance {
            hang: Some(1),
            after: None,
            advance_ms: 50,
        }],
        Some(Selector {
            kind: "turn_completed".to_owned(),
            nth: 1,
        }),
    );
    let (done_tx, done_rx) = mpsc::channel();
    let applier = Arc::clone(&case);
    std::thread::spawn(move || {
        let mut next = 0;
        let mut failures = Vec::new();
        let ok = applier.apply_advances(None, &mut next, &mut failures);
        let _ignored = done_tx.send((ok, next, failures));
    });
    case.host_script()
        .wait_hang_waiting(1, Duration::from_secs(5))
        .expect("the applier reaches the hold for entry 1");
    assert!(
        matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the applier remains blocked before entry 1 matches as a hang"
    );
    let deadline = case.session_clock().now() + Duration::from_millis(50);
    let (release, waiter) = park_waiter(&case, deadline);
    case.host_script()
        .hang_matched(1, &json!({"program": "hang-once"}), deadline);
    let (ok, next, failures) = Deadline::after(Duration::from_secs(5))
        .recv(&done_rx)
        .expect("the advance applies once entry 1 matches");
    assert!(ok && next == 1 && failures.is_empty(), "{failures:?}");
    let (offset, _) = case.clock.parked_offsets();
    assert_eq!(offset, Duration::from_millis(50));
    let _ignored = release.send(());
    waiter.join().expect("join the parked waiter");
}

#[test]
fn a_hang_advance_fails_naming_the_entry_when_nothing_matches() {
    let case = case_run_with_host(
        vec![json!({"kind": "turn_completed"})],
        Host::default(),
        vec![ClockAdvance {
            hang: Some(2),
            after: None,
            advance_ms: 50,
        }],
        Some(Selector {
            kind: "turn_completed".to_owned(),
            nth: 1,
        }),
    );
    let mut next = 0;
    let mut failures = Vec::new();
    assert!(!case.apply_advances(None, &mut next, &mut failures));
    assert_eq!(next, 0);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("clock advance[1]"), "{}", failures[0]);
    assert!(failures[0].contains("host.exec[2]"), "{}", failures[0]);
    assert!(
        failures[0].contains("did not hang within"),
        "{}",
        failures[0]
    );
}

#[test]
fn a_matched_hang_past_the_last_advance_ends_the_event_wait() {
    let (_root, log) = session_log(&[]);
    let case = case_run(
        vec![json!({"kind": "extension_log"})],
        Vec::new(),
        Some(Selector {
            kind: "extension_log".to_owned(),
            nth: 1,
        }),
    );
    let stuck_at = case.session_clock().now() + Duration::from_millis(50);
    case.host_script()
        .hang_matched(1, &json!({"program": "hang-once"}), stuck_at);
    let mut watcher = log.watch_all();
    let deadline = case.process_clock.now() + LOOP_WAIT;
    let mut failures = Vec::new();
    assert!(
        case.next_event(&mut watcher, deadline, &mut failures, true)
            .is_none()
    );
    assert!(
        failures.is_empty(),
        "a stuck hang ends the wait with no new failure: {failures:?}"
    );
}

#[test]
fn without_the_last_advance_the_event_wait_keeps_its_bound() {
    let (_root, log) = session_log(&[]);
    let case = case_run(
        vec![json!({"kind": "extension_log"})],
        Vec::new(),
        Some(Selector {
            kind: "extension_log".to_owned(),
            nth: 1,
        }),
    );
    let stuck_at = case.session_clock().now() + Duration::from_millis(50);
    case.host_script()
        .hang_matched(1, &json!({"program": "hang-once"}), stuck_at);
    let mut watcher = log.watch_all();
    let deadline = case.process_clock.now() + LOOP_WAIT;
    let mut failures = Vec::new();
    assert!(
        case.next_event(&mut watcher, deadline, &mut failures, false)
            .is_none()
    );
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("wait for until event extension_log occurrence 1"),
        "{failures:?}"
    );
}

#[test]
fn a_failed_initial_advance_still_releases_a_stuck_hang_into_the_exit_wait() {
    let (_root, log) = session_log(&[]);
    let case = case_run_with_host(
        vec![json!({"kind": "fiber_exited"})],
        Host::default(),
        vec![ClockAdvance {
            hang: Some(2),
            after: None,
            advance_ms: 50,
        }],
        Some(Selector {
            kind: "turn_completed".to_owned(),
            nth: 1,
        }),
    );
    let stuck_at = case.session_clock().now() + Duration::from_millis(50);
    case.host_script()
        .hang_matched(1, &json!({"program": "hang-once"}), stuck_at);
    let (release, waiter) = park_waiter(&case, stuck_at);
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: true,
    });

    let failures = case.drive(
        driver.clone() as Arc<dyn contract::extension::Drive>,
        log.watch_all(),
        Arc::new(r#loop::TurnCancel::default()),
    );
    let _ignored = release.send(());
    waiter.join().expect("join the parked waiter");

    assert_eq!(driver.close_calls.load(Ordering::SeqCst), 1);
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("host.exec[1] hung")),
        "{failures:?}"
    );
    assert!(
        !failures
            .iter()
            .any(|failure| failure.contains("missing durable event")),
        "after a failed initial advance the released hang drives the exit wait, which consumes the close event: {failures:?}"
    );
}

#[test]
fn a_failed_mid_loop_advance_still_releases_a_stuck_hang_into_the_exit_wait() {
    let (_root, log) = session_log(&[turn_completed()]);
    let case = case_run_with_host(
        vec![
            json!({"kind": "turn_completed"}),
            json!({"kind": "fiber_exited"}),
        ],
        Host::default(),
        vec![ClockAdvance {
            hang: Some(2),
            after: Some(Selector {
                kind: "turn_completed".to_owned(),
                nth: 1,
            }),
            advance_ms: 50,
        }],
        Some(Selector {
            kind: "turn_completed".to_owned(),
            nth: 1,
        }),
    );
    let stuck_at = case.session_clock().now() + Duration::from_millis(50);
    case.host_script()
        .hang_matched(1, &json!({"program": "hang-once"}), stuck_at);
    let (release, waiter) = park_waiter(&case, stuck_at);
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: true,
    });

    let failures = case.drive(
        driver.clone() as Arc<dyn contract::extension::Drive>,
        log.watch_all(),
        Arc::new(r#loop::TurnCancel::default()),
    );
    let _ignored = release.send(());
    waiter.join().expect("join the parked waiter");

    assert_eq!(driver.close_calls.load(Ordering::SeqCst), 1);
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("host.exec[1] hung")),
        "{failures:?}"
    );
    assert!(
        !failures
            .iter()
            .any(|failure| failure.contains("missing durable event")),
        "after a failed mid-loop advance the released hang drives the exit wait, which consumes the close event: {failures:?}"
    );
}

#[test]
fn a_stuck_hang_past_all_advances_skips_the_event_wait() {
    let (_root, log) = session_log(&[]);
    let case = case_run(
        Vec::new(),
        Vec::new(),
        Some(Selector {
            kind: "extension_log".to_owned(),
            nth: 1,
        }),
    );
    let stuck_at = case.session_clock().now() + Duration::from_millis(50);
    case.host_script()
        .hang_matched(1, &json!({"program": "hang-once"}), stuck_at);
    let (release, waiter) = park_waiter(&case, stuck_at);
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: false,
    });

    let failures = case.drive(
        driver.clone() as Arc<dyn contract::extension::Drive>,
        log.watch_all(),
        Arc::new(r#loop::TurnCancel::default()),
    );
    let _ignored = release.send(());
    waiter.join().expect("join the parked waiter");

    assert_eq!(driver.close_calls.load(Ordering::SeqCst), 1);
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("host.exec[1] hung")),
        "{failures:?}"
    );
    assert_eq!(
        failures
            .iter()
            .filter(|failure| failure.contains("wait for until event"))
            .count(),
        1,
        "only the exit wait expires; the main wait ends on the stuck hang: {failures:?}"
    );
}

#[test]
fn close_names_each_stuck_hang_and_releases_in_deadline_order() {
    let (_root, log) = session_log(&[]);
    let case = case_run(vec![], Vec::new(), None);
    let now = case.session_clock().now();
    // Entry 2 ends first: the release must still run in deadline order.
    let early = now + Duration::from_millis(50);
    let late = now + Duration::from_millis(100);
    case.host_script().hang_matched(
        1,
        &json!({"program": "hang-late", "env": {"FIBER_X": "s3cret-value"}}),
        late,
    );
    case.host_script()
        .hang_matched(2, &json!({"program": "hang-early"}), early);
    let (release_early, waiter_early) = park_waiter(&case, early);
    let (release_late, waiter_late) = park_waiter(&case, late);
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: false,
    });
    let mut failures = Vec::new();
    let drive = Arc::clone(&driver) as Arc<dyn contract::extension::Drive>;
    let released = case.close(
        &drive,
        &Arc::new(r#loop::TurnCancel::default()),
        &mut failures,
    );
    assert!(released);
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(
        failures[0].contains("unmet host call: host.exec[2] hung"),
        "{failures:?}"
    );
    assert!(
        failures[0].contains("still parked after the last clock advance"),
        "{failures:?}"
    );
    assert!(failures[0].contains("hang-early"), "{failures:?}");
    assert!(
        failures[1].contains("unmet host call: host.exec[1] hung"),
        "{failures:?}"
    );
    assert!(failures[1].contains("hang-late"), "{failures:?}");
    assert!(failures[1].contains("<redacted>"), "{failures:?}");
    assert!(failures[1].contains("FIBER_X"), "{failures:?}");
    assert!(
        !failures.join("\n").contains("s3cret-value"),
        "{failures:?}"
    );
    let (offset, _) = case.clock.parked_offsets();
    assert_eq!(offset, Duration::from_millis(100));
    assert_eq!(driver.close_calls.load(Ordering::SeqCst), 1);
    let _ignored = release_early.send(());
    let _ignored = release_late.send(());
    waiter_early.join().expect("join the early waiter");
    waiter_late.join().expect("join the late waiter");
}

fn session_log(events: &[Event]) -> (fakes::TempDir, Arc<Log>) {
    let root = fakes::TempDir::new("fiber-case-run-loop");
    let sessions = root.path().join("sessions");
    fs::create_dir_all(&sessions).expect("create test sessions directory");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let log = Arc::new(
        Log::create(&sessions, SessionId("case-run-loop".to_owned()), clock)
            .expect("create test session log"),
    );
    for event in events {
        log.append(event, None, None).expect("append test event");
    }
    (root, log)
}

fn turn_completed() -> Event {
    Event::TurnCompleted(TurnCompleted {
        outcome: TurnOutcome::Completed,
        error: None,
        questions: None,
    })
}

fn fiber_exited() -> Event {
    Event::FiberExited(FiberExited {
        exit_code: 0,
        usage: Usage {
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        final_message: None,
        error: None,
        suspended_on: None,
        questions: None,
    })
}

struct FakeDrive {
    log: Arc<Log>,
    close_calls: AtomicUsize,
    exit_on_close: bool,
}

impl contract::extension::Drive for FakeDrive {
    fn drive(
        &self,
        _extension: &str,
        command: &str,
        _args: serde_json::Map<String, Value>,
        answer: contract::inbox::Ack,
    ) {
        if command == "close" {
            self.close_calls.fetch_add(1, Ordering::SeqCst);
            if self.exit_on_close {
                self.log
                    .append(&fiber_exited(), None, None)
                    .expect("append close event");
            }
        }
        answer.0(Ok(None));
    }
}

#[test]
fn a_recorded_failure_appears_in_the_child_verdict() {
    let case = case_run(Vec::new(), Vec::new(), None);
    case.record_failure("the case driver failed".to_owned());

    assert_eq!(
        case.verdict(),
        Some(vec!["the case driver failed".to_owned()])
    );
}

#[test]
fn successful_initial_advances_do_not_close_before_the_until_event() {
    let (_root, log) = session_log(&[turn_completed()]);
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: true,
    });
    let case = case_run(
        vec![
            json!({"kind": "turn_completed"}),
            json!({"kind": "fiber_exited"}),
        ],
        Vec::new(),
        Some(Selector {
            kind: "turn_completed".to_owned(),
            nth: 1,
        }),
    );

    let failures = case.drive(
        Arc::clone(&driver) as Arc<dyn contract::extension::Drive>,
        log.watch_all(),
        Arc::new(r#loop::TurnCancel::default()),
    );

    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(driver.close_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn the_until_event_stops_driving_and_uses_the_short_exit_deadline() {
    let (_root, log) = session_log(&[turn_completed()]);
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: false,
    });
    let case = case_run(
        vec![json!({"kind": "turn_completed"})],
        Vec::new(),
        Some(Selector {
            kind: "turn_completed".to_owned(),
            nth: 1,
        }),
    );

    let failures = case.drive(
        Arc::clone(&driver) as Arc<dyn contract::extension::Drive>,
        log.watch_all(),
        Arc::new(r#loop::TurnCancel::default()),
    );

    assert_eq!(driver.close_calls.load(Ordering::SeqCst), 1);
    assert_eq!(failures.len(), 3, "{failures:?}");
    assert!(
        failures[0].contains("wait for until event turn_completed occurrence 1"),
        "{failures:?}"
    );
    assert_eq!(
        failures[1],
        "the session did not write fiber_exited after close"
    );
    assert!(failures[2].starts_with("diagnostics:"), "{failures:?}");
    assert!(failures[2].contains("turn_completed"), "{failures:?}");
    assert!(failures[2].contains("next_advance=none"), "{failures:?}");
}

#[test]
fn exiting_before_the_next_advance_names_that_advance() {
    let (_root, log) = session_log(&[fiber_exited()]);
    let case = case_run(
        vec![json!({"kind": "fiber_exited"})],
        vec![ClockAdvance {
            hang: None,
            after: Some(Selector {
                kind: "turn_completed".to_owned(),
                nth: 1,
            }),
            advance_ms: 200,
        }],
        Some(Selector {
            kind: "turn_completed".to_owned(),
            nth: 1,
        }),
    );
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: false,
    });

    let failures = case.drive(
        driver as Arc<dyn contract::extension::Drive>,
        log.watch_all(),
        Arc::new(r#loop::TurnCancel::default()),
    );

    assert_eq!(failures[0], "the session exited before its until event");
    assert_eq!(failures[1], "clock advance[1] was not reached");
    assert_eq!(failures.len(), 3, "{failures:?}");
    assert!(failures[2].starts_with("diagnostics:"), "{failures:?}");
    assert!(failures[2].contains("fiber_exited"), "{failures:?}");
    assert!(failures[2].contains("next_advance=1"), "{failures:?}");
}

#[test]
fn a_missing_advance_fails_on_the_short_advance_wait() {
    let (_root, log) = session_log(&[turn_completed()]);
    let case = case_run(
        vec![json!({"kind": "turn_completed"})],
        vec![ClockAdvance {
            hang: None,
            after: Some(Selector {
                kind: "turn_completed".to_owned(),
                nth: 1,
            }),
            advance_ms: 200,
        }],
        Some(Selector {
            kind: "turn_completed".to_owned(),
            nth: 1,
        }),
    );
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: false,
    });

    let failures = case.drive(
        driver.clone() as Arc<dyn contract::extension::Drive>,
        log.watch_all(),
        Arc::new(r#loop::TurnCancel::default()),
    );

    assert_eq!(driver.close_calls.load(Ordering::SeqCst), 1);
    assert_eq!(failures.len(), 3, "{failures:?}");
    assert!(
        failures[0].contains("clock advance[1]: no waiter parked"),
        "{failures:?}"
    );
    assert_eq!(failures[1], "clock advance[1] was not reached");
    assert!(failures[2].starts_with("diagnostics:"), "{failures:?}");
    assert!(failures[2].contains("turn_completed"), "{failures:?}");
    assert!(failures[2].contains("next_advance=1"), "{failures:?}");
}

#[test]
fn an_advance_never_reached_is_dumped_after_the_until_event_and_exit() {
    let (_root, log) = session_log(&[turn_completed(), fiber_exited()]);
    let case = case_run(
        vec![
            json!({"kind": "turn_completed"}),
            json!({"kind": "fiber_exited"}),
        ],
        vec![ClockAdvance {
            hang: None,
            after: Some(Selector {
                kind: "never_written".to_owned(),
                nth: 1,
            }),
            advance_ms: 200,
        }],
        Some(Selector {
            kind: "turn_completed".to_owned(),
            nth: 1,
        }),
    );
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: false,
    });

    let failures = case.drive(
        driver.clone() as Arc<dyn contract::extension::Drive>,
        log.watch_all(),
        Arc::new(r#loop::TurnCancel::default()),
    );

    assert_eq!(driver.close_calls.load(Ordering::SeqCst), 1);
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert_eq!(failures[0], "clock advance[1] was not reached");
    assert!(failures[1].starts_with("diagnostics:"), "{failures:?}");
    assert!(failures[1].contains("next_advance=1"), "{failures:?}");
}

#[test]
fn an_expired_until_wait_dumps_parked_deadlines_events_and_next_advance() {
    let (_root, log) = session_log(&[]);
    let case = case_run(
        vec![json!({"kind": "turn_completed"})],
        vec![ClockAdvance {
            hang: None,
            after: Some(Selector {
                kind: "extension_log".to_owned(),
                nth: 1,
            }),
            advance_ms: 200,
        }],
        Some(Selector {
            kind: "extension_log".to_owned(),
            nth: 1,
        }),
    );
    // A waiter parked at now + 200ms stays parked while the driver waits
    // for an until event that never arrives.
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let waiter_clock = case.session_clock();
    let waiter = std::thread::spawn(move || {
        let until = waiter_clock.now() + Duration::from_millis(200);
        waiter_clock.wait_until(Some(until), &mut |_| {
            let _ignored = parked_tx.send(());
            let _ignored = Deadline::after(Duration::from_secs(5)).recv(&release_rx);
        });
    });
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_rx)
        .expect("the waiter parks before the driver runs");
    // Ephemeral lines appended after the watcher registers still reach
    // the driver, so the dump must list them in order.
    let watcher = log.watch_all();
    log.append(
        &Event::AssistantMessageDelta(TextDelta { text: "a".into() }),
        None,
        None,
    )
    .expect("append the ephemeral delta");
    log.append(&turn_completed(), None, None)
        .expect("append the durable event");
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: false,
    });

    let failures = case.drive(
        driver as Arc<dyn contract::extension::Drive>,
        watcher,
        Arc::new(r#loop::TurnCancel::default()),
    );
    let _ignored = release_tx.send(());
    waiter.join().expect("join the parked waiter");

    assert_eq!(failures.len(), 3, "{failures:?}");
    assert!(
        failures[0].contains("wait for until event extension_log occurrence 1"),
        "{failures:?}"
    );
    assert!(
        failures[0].contains("20ms"),
        "the until wait stays at the test bound: {failures:?}"
    );
    assert_eq!(failures[1], "clock advance[1] was not reached");
    let dump = &failures[2];
    assert!(dump.starts_with("diagnostics:"), "{failures:?}");
    assert!(dump.contains("now_offset_ms=0"), "{failures:?}");
    assert!(dump.contains("parked_ms=[200ms]"), "{failures:?}");
    let events = dump
        .split("events=[")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap_or_default();
    assert_eq!(
        events, "assistant_message_delta, turn_completed",
        "the complete ordered kinds seen: {failures:?}"
    );
    assert!(dump.contains("next_advance=1"), "{failures:?}");
}

#[test]
fn a_parked_after_timer_waits_out_the_short_until_bound_without_advances() {
    let (_root, log) = session_log(&[]);
    let case = case_run(
        vec![json!({"kind": "extension_log"})],
        Vec::new(),
        Some(Selector {
            kind: "extension_log".to_owned(),
            nth: 1,
        }),
    );
    // A waiter parked at now + 5000ms mirrors host.after(5000) while the
    // driver waits for an until event that never arrives and no advance
    // ever runs.
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let waiter_clock = case.session_clock();
    let waiter = std::thread::spawn(move || {
        let until = waiter_clock.now() + Duration::from_millis(5000);
        waiter_clock.wait_until(Some(until), &mut |_| {
            let _ignored = parked_tx.send(());
            // The test owns release_tx, so this blocks until the test sends
            // or drops it: the waiter stays parked with no deadline of its own.
            let _ignored = release_rx.recv();
        });
    });
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_rx)
        .expect("the waiter parks before the driver runs");
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: false,
    });

    let failures = case.drive(
        driver as Arc<dyn contract::extension::Drive>,
        log.watch_all(),
        Arc::new(r#loop::TurnCancel::default()),
    );
    let _ignored = release_tx.send(());
    waiter.join().expect("join the parked waiter");

    assert_eq!(failures.len(), 3, "{failures:?}");
    assert!(
        failures[0].contains("wait for until event extension_log occurrence 1"),
        "{failures:?}"
    );
    assert!(
        failures[0].contains("20ms"),
        "the until wait stays at the test bound: {failures:?}"
    );
    assert_eq!(failures[1], "expect[0]: missing durable event");
    let dump = &failures[2];
    assert!(dump.starts_with("diagnostics:"), "{failures:?}");
    assert!(dump.contains("now_offset_ms=0"), "{failures:?}");
    assert!(dump.contains("parked_ms=[5000ms]"), "{failures:?}");
    assert!(dump.contains("events=[]"), "{failures:?}");
    assert!(
        !dump.contains("extension_log"),
        "no until event arrived before the bound: {failures:?}"
    );
    assert!(dump.contains("next_advance=none"), "{failures:?}");
}

/// A clock whose first `now()` waits until the test releases it, holding
/// the driver in the window where it has started but not yet registered
/// its watcher.
struct GateClock {
    origin: Instant,
    entered: Arc<(Mutex<bool>, Condvar)>,
    released: Arc<(Mutex<bool>, Condvar)>,
    calls: AtomicUsize,
}

impl Clock for GateClock {
    fn now(&self) -> Instant {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let (entered, cvar) = &*self.entered;
            *entered.lock().unwrap_or_else(PoisonError::into_inner) = true;
            cvar.notify_all();
            let (released, cvar) = &*self.released;
            let guard = released.lock().unwrap_or_else(PoisonError::into_inner);
            let (_guard, wait) = cvar
                .wait_timeout_while(guard, Duration::from_secs(5), |open| !*open)
                .unwrap_or_else(PoisonError::into_inner);
            assert!(
                !wait.timed_out(),
                "the test did not release the gate clock within 5s"
            );
        }
        self.origin
    }

    fn wall(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(1_700_000_000))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }

    fn sleep(&self, _for: Duration) {}

    fn wait_until(&self, _until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        wait(None);
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

#[test]
fn an_ephemeral_line_written_right_after_start_reaches_the_driver() {
    let entered = Arc::new((Mutex::new(false), Condvar::new()));
    let released = Arc::new((Mutex::new(false), Condvar::new()));
    let clock: Arc<dyn Clock> = Arc::new(GateClock {
        #[expect(
            clippy::disallowed_methods,
            reason = "the gate clock's origin is one Instant::now, as FakeClock::new"
        )]
        origin: Instant::now(),
        entered: Arc::clone(&entered),
        released: Arc::clone(&released),
        calls: AtomicUsize::new(0),
    });
    let (_root, log) = session_log(&[]);
    let driver = Arc::new(FakeDrive {
        log: Arc::clone(&log),
        close_calls: AtomicUsize::new(0),
        exit_on_close: true,
    });
    let case = CaseRun::with_waits(
        session_case(
            Some("run-loop".to_owned()),
            vec![json!({"kind": "fiber_exited"})],
            Host::default(),
            Vec::new(),
            Some(Selector {
                kind: "assistant_message_delta".to_owned(),
                nth: 1,
            }),
            true,
        ),
        clock,
        TEST_WAITS,
    );

    let handle = case
        .start(
            Arc::clone(&driver) as Arc<dyn contract::extension::Drive>,
            Arc::clone(&log),
            Arc::new(r#loop::TurnCancel::default()),
        )
        .expect("start the case driver");

    let (flag, cvar) = &*entered;
    let guard = flag.lock().unwrap_or_else(PoisonError::into_inner);
    let (_guard, wait) = cvar
        .wait_timeout_while(guard, Duration::from_secs(5), |seen| !*seen)
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        !wait.timed_out(),
        "the case driver did not park in process_clock.now() within 5s"
    );

    log.append(
        &Event::AssistantMessageDelta(TextDelta { text: "a".into() }),
        None,
        None,
    )
    .expect("append the race-window delta");

    let (flag, cvar) = &*released;
    *flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
    cvar.notify_all();

    let (done_tx, done_rx) = mpsc::channel();
    let joiner = std::thread::spawn(move || {
        let result = handle.join();
        let _ignored = done_tx.send(());
        result
    });
    assert!(
        Deadline::after(Duration::from_secs(5))
            .recv(&done_rx)
            .is_ok(),
        "the case driver did not finish within 5s"
    );
    joiner
        .join()
        .expect("join the driver watcher")
        .expect("join the case driver");
    assert_eq!(case.verdict(), Some(Vec::new()));
}

#[test]
fn the_case_config_is_written_whole_into_a_missing_home() {
    let root = fakes::TempDir::new("fiber-case-config");
    let home = root.path().join("home");
    assert!(!home.exists(), "the home starts missing");
    let config = json!({"handoff": {"tokens": 5}, "model": "a/b"});
    let file = home.join("config.json");
    write_config(&file, &config).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), config);
    let leftovers: Vec<std::path::PathBuf> = fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn an_unwritable_home_names_the_home() {
    let root = fakes::TempDir::new("fiber-case-config-unwritable");
    let home = root.path().join("home");
    fs::write(&home, "in the way").unwrap();
    let file = home.join("config.json");
    let err = write_config(&file, &json!({"model": "a/b"})).unwrap_err();
    assert!(
        err.contains(&home.display().to_string()),
        "the error names the home: {err}"
    );
}

#[test]
fn each_settings_entry_is_written_as_that_extensions_global_file() {
    let root = fakes::TempDir::new("fiber-case-settings");
    let home = root.path().join("home");
    super::write_settings(
        &home,
        &json!({"hooks": {"hooks": {"probe": {"point": "session_start"}}}, "other": {}}),
    )
    .unwrap();
    let text = fs::read_to_string(home.join("config").join("hooks.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"hooks": {"probe": {"point": "session_start"}}}),
    );
    let text = fs::read_to_string(home.join("config").join("other.json")).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({}));
}
