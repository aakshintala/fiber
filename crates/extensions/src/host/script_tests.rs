//! Scripted host-call matching (`docs/testing.md`, "Testing an extension").

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{ExecEnd, ExecEntry, ExecReply, HostScript, HttpEntry, json_matches};

#[test]
fn oauth_entries_match_in_order_and_misses_consume_nothing() {
    let script = HostScript::new(
        vec![],
        vec![],
        vec![
            json!({"pkce": {"verifier": "fixed", "challenge": "challenge"}}),
            json!({"open": {"url": "https://example.test"}}),
            json!({"show": {"url": "https://example.test/device", "code": "1234"}}),
            json!({"callback": {"reply": {"query": {"code": "yes"}}}}),
            json!({"callback": {"error": {"code": "io_failed", "message": "busy"}}}),
        ],
    );
    assert_eq!(script.unmet().len(), 5);
    assert!(
        script
            .oauth("open", &json!({"url": "https://example.test"}))
            .is_err()
    );
    assert_eq!(
        script.oauth("pkce", &json!({})).unwrap()["verifier"],
        "fixed"
    );
    assert!(script.oauth("open", &json!({"url": "wrong"})).is_err());
    assert!(
        script
            .oauth("open", &json!({"url": "https://example.test"}))
            .is_ok()
    );
    assert!(
        script
            .oauth(
                "show",
                &json!({"url": "https://example.test/device", "code": "1234"})
            )
            .is_ok()
    );
    assert_eq!(
        script.oauth("callback", &json!({})).unwrap()["query"]["code"],
        "yes"
    );
    assert_eq!(
        script.oauth("callback", &json!({})).unwrap_err(),
        (ErrorCode::IoFailed, "busy".to_owned())
    );
    assert!(script.oauth("pkce", &json!({})).is_err());
    assert_eq!(script.unmet().len(), 3, "only the three misses remain");
}

fn http(request: Value) -> HttpEntry {
    HttpEntry {
        request,
        reply: Ok((200, b"ok".to_vec())),
    }
}

fn exec(request: Value) -> ExecEntry {
    ExecEntry {
        request,
        reply: Ok(ExecReply::Completed {
            end: ExecEnd::Code(0),
            stdout: "ok".to_owned(),
            stderr: String::new(),
        }),
    }
}

fn hang_entry(request: Value) -> ExecEntry {
    ExecEntry {
        request,
        reply: Ok(ExecReply::Hang),
    }
}

#[test]
fn hang_matches_record_redacted_requests_and_pending_filters_on_the_deadline() {
    let request = json!({
        "program": "sh",
        "args": [],
        "cwd": "/w",
        "env": {"FIBER_X": "s3cret-value"},
    });
    let script = HostScript::new(
        vec![],
        vec![
            hang_entry(request.clone()),
            hang_entry(request.clone()),
            hang_entry(request.clone()),
        ],
        Vec::new(),
    );
    let clock = FakeClock::new();
    let now = clock.now();
    let (first, outcome) = script.exec(request.clone());
    assert_eq!(first, 1);
    assert!(matches!(outcome, Ok(ExecReply::Hang)), "{outcome:?}");
    // At the edge a deadline of now is not stuck: only a later one is.
    script.hang_matched(first, &request, now);
    assert!(script.pending_hangs(now).is_empty());
    // Just below now is not stuck either.
    let (second, _) = script.exec(request.clone());
    assert_eq!(second, 2);
    let below = now
        .checked_sub(Duration::from_millis(1))
        .expect("a millisecond before now");
    script.hang_matched(second, &request, below);
    assert!(script.pending_hangs(now).is_empty());
    // Just above now is stuck, with the secret redacted and the name kept.
    let (third, _) = script.exec(request.clone());
    assert_eq!(third, 3);
    let deadline = now
        .checked_add(Duration::from_millis(1))
        .expect("a millisecond after now");
    script.hang_matched(third, &request, deadline);
    let pending = script.pending_hangs(now);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, 3);
    assert_eq!(pending[0].2, deadline);
    assert_eq!(pending[0].1["env"]["FIBER_X"], json!("<redacted>"));
    assert!(
        !pending[0].1.to_string().contains("s3cret-value"),
        "{}",
        pending[0].1
    );
}

#[test]
fn wait_hang_matched_returns_once_the_entry_matches() {
    let script = HostScript::new(vec![], vec![], Vec::new());
    let waiter = Arc::clone(&script);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = waiter.wait_hang_matched(1, MUST_SUCCEED_WITHIN);
        let _ignored = done_tx.send(outcome);
    });
    script
        .wait_hang_waiting(1, MUST_SUCCEED_WITHIN)
        .expect("the waiter reaches the hang-match wait");
    script.hang_matched(1, &json!({"program": "sh"}), FakeClock::new().now());
    let outcome = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&done_rx)
        .expect("the waiter returns once entry 1 matches");
    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(
        script.wait_hang_waiting(1, Duration::ZERO).is_err(),
        "a completed waiter is removed"
    );
}

#[test]
fn wait_hang_matched_ignores_other_entries_until_its_own_matches() {
    let script = HostScript::new(vec![], vec![], Vec::new());
    let waiter = Arc::clone(&script);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = waiter.wait_hang_matched(1, Duration::from_secs(30));
        let _ignored = done_tx.send(outcome);
    });
    script
        .wait_hang_waiting(1, MUST_SUCCEED_WITHIN)
        .expect("the waiter reaches the hang-match wait");
    // A match on another entry must not release the waiter: only entry 1
    // ends its wait. The waiter announces itself through hang_waiters, so
    // no wall-clock timing fixes this order.
    script.hang_matched(2, &json!({"program": "other"}), FakeClock::new().now());
    assert!(
        matches!(
            Deadline::after(Duration::from_millis(200)).recv(&done_rx),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "waiting for entry 1 ignores a match on entry 2"
    );
    script.hang_matched(1, &json!({"program": "sh"}), FakeClock::new().now());
    let outcome = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&done_rx)
        .expect("the waiter returns once entry 1 matches");
    assert!(outcome.is_ok(), "{outcome:?}");
}

#[test]
fn wait_hang_matched_fails_naming_the_entry_when_nothing_matches() {
    let script = HostScript::new(vec![], vec![], Vec::new());
    assert!(
        script.wait_hang_waiting(2, Duration::ZERO).is_err(),
        "no waiter is waiting for entry 2"
    );
    let error = script
        .wait_hang_matched(2, Duration::ZERO)
        .expect_err("no hang matches entry 2");
    assert!(error.contains("host.exec[2]"), "{error}");
    assert!(error.contains("did not hang within"), "{error}");
}

#[test]
fn park_hang_ends_when_the_clock_reaches_the_deadline() {
    let script = HostScript::new(vec![], vec![], Vec::new());
    let clock = FakeClock::new();
    let deadline = clock
        .now()
        .checked_add(Duration::from_millis(100))
        .expect("a deadline after now");
    let (_stop, stop_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    let (worker_script, worker_clock) = (Arc::clone(&script), Arc::clone(&clock));
    std::thread::spawn(move || {
        worker_script.park_hang(worker_clock.as_ref(), deadline, &stop_rx);
        let _ignored = done_tx.send(());
    });
    assert!(
        clock.await_parked(deadline, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the worker parks at its deadline before the advance"
    );
    clock.advance(Duration::from_millis(100));
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&done_rx)
        .expect("the park ends at its deadline");
    assert!(script.unmet().is_empty(), "the park records no miss");
}

#[test]
fn a_stopped_park_pushes_no_miss() {
    let script = HostScript::new(vec![], vec![], Vec::new());
    let clock = FakeClock::new();
    let deadline = clock
        .now()
        .checked_add(Duration::from_secs(60))
        .expect("a deadline after now");
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    let (worker_script, worker_clock) = (Arc::clone(&script), Arc::clone(&clock));
    std::thread::spawn(move || {
        worker_script.park_hang(worker_clock.as_ref(), deadline, &stop_rx);
        let _ignored = done_tx.send(());
    });
    assert!(
        clock.await_parked(deadline, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the worker parks at its deadline before the stop"
    );
    drop(stop_tx);
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&done_rx)
        .expect("the park ends once the stop sender drops");
    assert!(script.unmet().is_empty(), "a stop records no miss");
}

#[test]
fn json_matches_the_expected_subset_and_reports_the_first_path() {
    let cases = [
        (
            "object subset, nested and extra keys",
            json!({"payload": {"cost": 0.5}}),
            json!({"payload": {"cost": 0.5, "other": true}, "extra": 1}),
            true,
        ),
        (
            "null matches a missing key",
            json!({"payload": {"cost": null}}),
            json!({"payload": {}}),
            true,
        ),
        (
            "a non-null expected key does not match a missing key",
            json!({"cost": 1}),
            json!({}),
            false,
        ),
        (
            "null matches null",
            json!({"cost": null}),
            json!({"cost": null}),
            true,
        ),
        (
            "null does not match a present value",
            json!({"cost": null}),
            json!({"cost": 1}),
            false,
        ),
        (
            "equal arrays match by subset and number rules",
            json!([{"cost": 1}, {"nested": ["two"]}]),
            json!([{"cost": 1.0, "extra": true}, {"nested": ["two"]}]),
            true,
        ),
        (
            "unequal arrays of the same length do not match",
            json!([1, 2]),
            json!([1, 3]),
            false,
        ),
        (
            "arrays of different lengths do not match",
            json!([1]),
            json!([1, 2]),
            false,
        ),
        (
            "nested arrays match recursively",
            json!({"items": [[{"cost": 1}]]}),
            json!({"items": [[{"cost": 1.0, "extra": true}]]}),
            true,
        ),
        (
            "nested unequal arrays do not match",
            json!({"items": [[1, 2]]}),
            json!({"items": [[1, 3]]}),
            false,
        ),
        ("integer matches float", json!(1), json!(1.0), true),
        ("float matches integer", json!(1.0), json!(1), true),
        ("equal integers match", json!(2), json!(2), true),
        ("different integers do not match", json!(2), json!(3), false),
        (
            "equal large integers match",
            json!(9007199254740993u64),
            json!(9007199254740993u64),
            true,
        ),
        (
            "different large integers do not match",
            json!(9007199254740993u64),
            json!(9007199254740992u64),
            false,
        ),
        ("string does not match number", json!("1"), json!(1), false),
    ];

    for (name, expected, actual, matches) in cases {
        assert_eq!(json_matches(&expected, &actual).is_ok(), matches, "{name}");
    }

    let error = json_matches(
        &json!({"payload": {"cost": 0.5}}),
        &json!({"payload": {"cost": 0.4}}),
    )
    .unwrap_err();
    assert!(error.contains("payload.cost"), "{error}");
}

#[test]
fn http_misses_do_not_consume_entries_and_unmet_lists_misses_before_unused_entries() {
    let script = HostScript::new(
        vec![
            http(json!({"url": "https://example.test/one"})),
            http(json!({"url": "https://example.test/two"})),
        ],
        vec![],
        Vec::new(),
    );

    let mismatch = script.http(json!({"url": "https://example.test/wrong"}));
    assert_eq!(
        mismatch,
        Err((
            ErrorCode::ConnectionFailed,
            "host.http: the case scripts no reply for this request".to_owned(),
        ))
    );
    assert_eq!(
        script.http(json!({"url": "https://example.test/one"})),
        Ok((200, b"ok".to_vec()))
    );

    let unmet = script.unmet();
    assert_eq!(unmet.len(), 2);
    assert!(unmet[0].contains("host.http[1]"), "{}", unmet[0]);
    assert!(
        unmet[0].contains("https://example.test/wrong"),
        "{}",
        unmet[0]
    );
    assert!(unmet[1].contains("host.http[2]"), "{}", unmet[1]);
    assert!(unmet[1].contains("unused"), "{}", unmet[1]);
}

#[test]
fn http_call_past_the_script_is_a_miss_at_the_next_index() {
    let script = HostScript::new(
        vec![http(json!({"url": "https://example.test/one"}))],
        vec![],
        Vec::new(),
    );
    assert!(
        script
            .http(json!({"url": "https://example.test/one"}))
            .is_ok()
    );
    assert!(
        script
            .http(json!({"url": "https://example.test/two"}))
            .is_err()
    );
    let unmet = script.unmet();
    assert_eq!(unmet.len(), 1);
    assert!(unmet[0].contains("host.http[2]"), "{}", unmet[0]);
    assert!(
        unmet[0].contains("https://example.test/two"),
        "{}",
        unmet[0]
    );
}

#[test]
fn http_header_names_are_matched_without_case() {
    let script = HostScript::new(
        vec![http(json!({
            "method": "GET",
            "url": "https://example.test/",
            "headers": {"X-Case": "value"},
            "body": null
        }))],
        vec![],
        Vec::new(),
    );
    assert!(
        script
            .http(json!({
                "method": "GET",
                "url": "https://example.test/",
                "headers": {"x-case": "value"},
                "body": null
            }))
            .is_ok()
    );
    assert!(script.unmet().is_empty());
}

#[test]
fn exec_matches_program_arguments_and_working_directory_in_order() {
    let request = json!({
        "program": "git",
        "args": ["status", "--short"],
        "cwd": "/workspace"
    });
    let script = HostScript::new(vec![], vec![exec(request.clone())], Vec::new());

    assert!(
        script
            .exec(json!({"program": "other", "args": ["status", "--short"], "cwd": "/workspace"}))
            .1
            .is_err()
    );
    assert_eq!(
        script.exec(request).1,
        Ok(ExecReply::Completed {
            end: ExecEnd::Code(0),
            stdout: "ok".to_owned(),
            stderr: String::new(),
        })
    );

    let unmet = script.unmet();
    assert_eq!(unmet.len(), 1);
    assert!(unmet[0].contains("host.exec[1]"), "{}", unmet[0]);
    assert!(unmet[0].contains("program"), "{}", unmet[0]);
    assert!(unmet[0].contains("other"), "{}", unmet[0]);
}

#[test]
fn exec_call_past_the_script_is_a_miss_at_the_next_index() {
    let script = HostScript::new(
        vec![],
        vec![exec(json!({
            "program": "git",
            "args": [],
            "cwd": "/workspace"
        }))],
        Vec::new(),
    );
    assert!(
        script
            .exec(json!({"program": "git", "args": [], "cwd": "/workspace"}))
            .1
            .is_ok()
    );
    assert_eq!(
        script
            .exec(json!({"program": "git", "args": ["status"], "cwd": "/workspace"}))
            .1,
        Err((
            ErrorCode::IoFailed,
            "host.exec: the case scripts no reply for this request".to_owned(),
        ))
    );
    let unmet = script.unmet();
    assert_eq!(unmet.len(), 1);
    assert!(unmet[0].contains("host.exec[2]"), "{}", unmet[0]);
    assert!(unmet[0].contains("status"), "{}", unmet[0]);
}

#[test]
fn an_exec_miss_redacts_env_values_but_keeps_names() {
    let script = HostScript::new(
        vec![],
        vec![exec(json!({
            "program": "sh",
            "args": [],
            "cwd": "/w",
            "env": {"FIBER_X": "s3cret-value"}
        }))],
        Vec::new(),
    );
    let mismatch = script.exec(json!({
        "program": "sh",
        "args": [],
        "cwd": "/w",
        "env": {"FIBER_X": "actual-s3cret"}
    }));
    assert_eq!(
        mismatch.1,
        Err((
            ErrorCode::IoFailed,
            "host.exec: the case scripts no reply for this request".to_owned(),
        ))
    );
    let unmet = script.unmet();
    assert_eq!(unmet.len(), 2);
    for line in &unmet {
        assert!(!line.contains("s3cret-value"), "{line}");
        assert!(!line.contains("actual-s3cret"), "{line}");
        assert!(line.contains("<redacted>"), "{line}");
        assert!(line.contains("FIBER_X"), "{line}");
    }
}

#[test]
fn an_unused_exec_entry_redacts_env_values_but_keeps_names() {
    let script = HostScript::new(
        vec![],
        vec![exec(json!({
            "program": "sh",
            "args": [],
            "cwd": "/w",
            "env": {"FIBER_X": "s3cret-value"}
        }))],
        Vec::new(),
    );
    let unmet = script.unmet();
    assert_eq!(unmet.len(), 1);
    assert!(!unmet[0].contains("s3cret-value"), "{}", unmet[0]);
    assert!(unmet[0].contains("<redacted>"), "{}", unmet[0]);
    assert!(unmet[0].contains("FIBER_X"), "{}", unmet[0]);
}

#[test]
fn an_exec_request_with_the_same_env_matches_its_entry() {
    let request = json!({
        "program": "sh",
        "args": [],
        "cwd": "/w",
        "env": {"FIBER_X": "s3cret-value"}
    });
    let script = HostScript::new(vec![], vec![exec(request.clone())], Vec::new());
    assert_eq!(
        script.exec(request).1,
        Ok(ExecReply::Completed {
            end: ExecEnd::Code(0),
            stdout: "ok".to_owned(),
            stderr: String::new(),
        })
    );
    assert!(script.unmet().is_empty());
}

#[test]
fn exec_returns_a_signal_end() {
    let request = json!({
        "program": "sh",
        "args": [],
        "cwd": "/w",
    });
    let script = HostScript::new(
        vec![],
        vec![ExecEntry {
            request: request.clone(),
            reply: Ok(ExecReply::Completed {
                end: ExecEnd::Signal(9),
                stdout: String::new(),
                stderr: String::new(),
            }),
        }],
        Vec::new(),
    );
    assert_eq!(
        script.exec(request).1,
        Ok(ExecReply::Completed {
            end: ExecEnd::Signal(9),
            stdout: String::new(),
            stderr: String::new(),
        })
    );
    assert!(script.unmet().is_empty());
}
