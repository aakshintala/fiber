//! Scripted host-call matching (`docs/testing.md`, "Testing an extension").

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock as _;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{ExecEnd, ExecEntry, ExecReply, HostScript, HttpEntry, json_matches};

/// How long a test waits for the hang to park or end before it fails.
const WAIT: Duration = Duration::from_secs(10);

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
        reply: Ok(ExecReply::Ran {
            end: ExecEnd::Code(0),
            stdout: "ok".to_owned(),
            stderr: String::new(),
        }),
    }
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
            .is_err()
    );
    assert_eq!(
        script.exec(request),
        Ok((
            0,
            ExecReply::Ran {
                end: ExecEnd::Code(0),
                stdout: "ok".to_owned(),
                stderr: String::new(),
            }
        ))
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
            .is_ok()
    );
    assert_eq!(
        script.exec(json!({"program": "git", "args": ["status"], "cwd": "/workspace"})),
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
        mismatch,
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
        script.exec(request),
        Ok((
            0,
            ExecReply::Ran {
                end: ExecEnd::Code(0),
                stdout: "ok".to_owned(),
                stderr: String::new(),
            }
        ))
    );
    assert!(script.unmet().is_empty());
}

fn hang(request: Value) -> ExecEntry {
    ExecEntry {
        request,
        reply: Ok(ExecReply::Hang),
    }
}

#[test]
fn exec_returns_a_signal_end_with_its_entry_index() {
    let request = json!({
        "program": "sh",
        "args": [],
        "cwd": "/w",
    });
    let script = HostScript::new(
        vec![],
        vec![ExecEntry {
            request: request.clone(),
            reply: Ok(ExecReply::Ran {
                end: ExecEnd::Signal(9),
                stdout: String::new(),
                stderr: String::new(),
            }),
        }],
        Vec::new(),
    );
    assert_eq!(
        script.exec(request),
        Ok((
            0,
            ExecReply::Ran {
                end: ExecEnd::Signal(9),
                stdout: String::new(),
                stderr: String::new(),
            }
        ))
    );
    assert!(script.unmet().is_empty());
}

/// Parks `script`'s hang `index` on `clock` until `deadline` on a worker:
/// the worker holds the hang so the Lua thread stays free, as the scripted
/// branch does. Returns what the worker's `park_hang` returns.
fn park_on(
    script: Arc<HostScript>,
    index: usize,
    request: Value,
    clock: Arc<FakeClock>,
    deadline: std::time::Instant,
    stop: mpsc::Receiver<()>,
) -> mpsc::Receiver<bool> {
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let timed_out = script.park_hang(index, &request, &*clock, deadline, &stop);
        done_tx
            .send(timed_out)
            .expect("the test collects the hang end");
    });
    done_rx
}

#[test]
fn park_hang_ends_only_when_the_clock_reaches_its_deadline() {
    let clock = FakeClock::new();
    let deadline = clock.now() + Duration::from_millis(1000);
    let request = json!({
        "program": "sh",
        "args": [],
        "cwd": "/w",
    });
    let script = HostScript::new(vec![], vec![hang(request.clone())], Vec::new());
    let (index, reply) = script.exec(request.clone()).expect("hang entry matches");
    assert!(matches!(reply, ExecReply::Hang));
    let (_stop, stop_rx) = mpsc::channel::<()>();
    let done_rx = park_on(
        Arc::clone(&script),
        index,
        request,
        Arc::clone(&clock),
        deadline,
        stop_rx,
    );
    assert!(
        clock.await_parked(deadline, WAIT),
        "waited {WAIT:?} for the hang to park on its deadline"
    );
    assert!(
        done_rx.try_recv().is_err(),
        "the hang is still parked before its deadline"
    );
    clock.advance(Duration::from_millis(999));
    assert!(
        done_rx.try_recv().is_err(),
        "the hang is still parked 1ms before its deadline"
    );
    clock.advance(Duration::from_millis(1));
    assert!(
        Deadline::after(WAIT)
            .recv(&done_rx)
            .expect("waited {WAIT:?} for the hang to end at its deadline"),
        "the hang ends when the clock reaches its deadline"
    );
    assert!(
        script.unmet().is_empty(),
        "a hang that times out records no miss"
    );
}

#[test]
fn park_hang_stopped_before_its_deadline_names_the_parked_request() {
    let clock = FakeClock::new();
    let deadline = clock.now() + Duration::from_millis(1000);
    let request = json!({
        "program": "sh",
        "args": [],
        "cwd": "/w",
        "env": {"FIBER_X": "s3cret-value"},
    });
    let script = HostScript::new(vec![], vec![hang(request.clone())], Vec::new());
    let (index, reply) = script.exec(request.clone()).expect("hang entry matches");
    assert!(matches!(reply, ExecReply::Hang));
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let done_rx = park_on(
        Arc::clone(&script),
        index,
        request,
        Arc::clone(&clock),
        deadline,
        stop_rx,
    );
    assert!(
        clock.await_parked(deadline, WAIT),
        "waited {WAIT:?} for the hang to park on its deadline"
    );
    drop(stop_tx);
    assert!(
        !Deadline::after(WAIT)
            .recv(&done_rx)
            .expect("waited {WAIT:?} for the stopped hang to end"),
        "a stop before the deadline is not a timeout"
    );
    let unmet = script.unmet();
    assert_eq!(unmet.len(), 1, "only the hung miss remains: {unmet:?}");
    assert!(unmet[0].contains("host.exec[1] hung"), "{}", unmet[0]);
    assert!(unmet[0].contains("still parked at close"), "{}", unmet[0]);
    assert!(unmet[0].contains("sh"), "{}", unmet[0]);
    assert!(!unmet[0].contains("s3cret-value"), "{}", unmet[0]);
    assert!(unmet[0].contains("<redacted>"), "{}", unmet[0]);
    assert!(unmet[0].contains("FIBER_X"), "{}", unmet[0]);
}

#[test]
fn park_hang_at_its_deadline_beats_a_late_stop() {
    let clock = FakeClock::new();
    let deadline = clock.now() + Duration::from_millis(1000);
    let request = json!({
        "program": "sh",
        "args": [],
        "cwd": "/w",
    });
    let script = HostScript::new(vec![], vec![hang(request.clone())], Vec::new());
    let (index, reply) = script.exec(request.clone()).expect("hang entry matches");
    assert!(matches!(reply, ExecReply::Hang));
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let done_rx = park_on(
        Arc::clone(&script),
        index,
        request,
        Arc::clone(&clock),
        deadline,
        stop_rx,
    );
    assert!(
        clock.await_parked(deadline, WAIT),
        "waited {WAIT:?} for the hang to park on its deadline"
    );
    clock.advance(Duration::from_millis(1000));
    drop(stop_tx);
    assert!(
        Deadline::after(WAIT)
            .recv(&done_rx)
            .expect("waited {WAIT:?} for the hang to end at its deadline"),
        "the deadline wins when the stop lands after it"
    );
    assert!(
        script.unmet().is_empty(),
        "a hang that times out records no miss: {:?}",
        script.unmet()
    );
}

#[test]
fn a_hang_without_a_callback_deadline_fails_and_names_the_request() {
    let request = json!({
        "program": "sh",
        "args": [],
        "cwd": "/w",
        "env": {"FIBER_X": "s3cret-value"},
    });
    let script = HostScript::new(vec![], vec![hang(request.clone())], Vec::new());
    let (index, reply) = script.exec(request.clone()).expect("hang entry matches");
    assert!(matches!(reply, ExecReply::Hang));
    let (code, message) = script.hang_without_deadline(index, &request);
    assert_eq!(code, ErrorCode::IoFailed);
    assert!(message.contains("deadline"), "{message}");
    let unmet = script.unmet();
    assert_eq!(unmet.len(), 1, "only the hung miss remains: {unmet:?}");
    assert!(unmet[0].contains("host.exec[1] hung"), "{}", unmet[0]);
    assert!(unmet[0].contains("still parked at close"), "{}", unmet[0]);
    assert!(unmet[0].contains("sh"), "{}", unmet[0]);
    assert!(!unmet[0].contains("s3cret-value"), "{}", unmet[0]);
    assert!(unmet[0].contains("FIBER_X"), "{}", unmet[0]);
}
