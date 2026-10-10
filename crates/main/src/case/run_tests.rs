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

use super::super::format::{ClockAdvance, Host, Selector};
use super::{CaseRun, WaitBounds, session_verdict};

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

fn case_run(
    expected: Vec<Value>,
    advances: Vec<ClockAdvance>,
    until: Option<Selector>,
) -> Arc<CaseRun> {
    CaseRun::with_waits(
        "run-loop".to_owned(),
        expected,
        Host::default(),
        advances,
        until,
        fakes::clock::FakeClock::new(),
        TEST_WAITS,
    )
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
fn an_expired_until_wait_dumps_parked_deadlines_events_and_next_advance() {
    let (_root, log) = session_log(&[]);
    let case = case_run(
        vec![json!({"kind": "turn_completed"})],
        vec![ClockAdvance {
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
            let _ignored = release_rx.recv_timeout(Duration::from_secs(5));
        });
    });
    parked_rx
        .recv_timeout(Duration::from_secs(5))
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
        "run-loop".to_owned(),
        vec![json!({"kind": "fiber_exited"})],
        Host::default(),
        Vec::new(),
        Some(Selector {
            kind: "assistant_message_delta".to_owned(),
            nth: 1,
        }),
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
        done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
        "the case driver did not finish within 5s"
    );
    joiner
        .join()
        .expect("join the driver watcher")
        .expect("join the case driver");
    assert_eq!(case.verdict(), Some(Vec::new()));
}
