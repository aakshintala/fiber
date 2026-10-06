//! Tests for the internal hub command's process handling: the exit code of
//! a failed start, what `exited` reports, the stdout drain keeping the last
//! `fiber_exited` line, and parsing that line.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::process::{Command, Stdio};

use hub::Started;

use super::*;

fn exited_line(code: &str, message: &str) -> String {
    serde_json::json!({
        "kind": "fiber_exited",
        "ts": 1,
        "schema_version": 1,
        "payload": {"error": {"code": code, "message": message}},
    })
    .to_string()
}

/// A child printing `lines` on stdout, then exiting 0.
fn printing(lines: &[String]) -> Child {
    let mut script = String::from("printf '%s\\n'");
    for line in lines {
        script.push_str(" '");
        script.push_str(&line.replace('\'', "'\\''"));
        script.push('\'');
    }
    Command::new("sh")
        .arg("-c")
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn drained(child: Child) -> Arc<Mutex<State>> {
    let state = Arc::new(Mutex::new(State {
        exited: false,
        failure: None,
    }));
    drain(&Arc::new(Mutex::new(child)), &state);
    state
}

fn failure_of(state: &Arc<Mutex<State>>) -> Option<Failure> {
    lock(state).failure.clone()
}

#[test]
fn fail_exits_1() {
    assert_eq!(fail("the current directory: gone"), 1);
}

#[test]
fn exited_is_none_while_the_session_runs() {
    let spawned = Spawned {
        id: SessionId("s_0123456789abcdef".to_owned()),
        state: Arc::new(Mutex::new(State {
            exited: false,
            failure: None,
        })),
    };
    assert!(spawned.exited().is_none());
}

#[test]
fn exited_reports_the_drained_failure() {
    let child = printing(&[exited_line("no_model", "No model is configured.")]);
    let state = drained(child);
    assert!(lock(&state).exited);
    let spawned = Spawned {
        id: SessionId("s_0123456789abcdef".to_owned()),
        state,
    };
    let failure = spawned.exited().expect("the drained failure");
    assert_eq!(failure.code, ErrorCode::NoModel);
    assert_eq!(failure.message, "No model is configured.");
}

#[test]
fn exited_without_a_verdict_is_io_failed() {
    let child = printing(&["not a verdict".to_owned()]);
    let state = drained(child);
    let spawned = Spawned {
        id: SessionId("s_0123456789abcdef".to_owned()),
        state,
    };
    let failure = spawned.exited().expect("the fallback failure");
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert!(failure.message.contains("s_0123456789abcdef"));
}

#[test]
fn drain_keeps_the_last_fiber_exited_line() {
    let child = printing(&[
        exited_line("no_model", "No model is configured."),
        exited_line("invalid_request", "The request was bad."),
        "noise".to_owned(),
    ]);
    let state = drained(child);
    assert!(lock(&state).exited);
    let failure = failure_of(&state).expect("the last verdict");
    assert_eq!(failure.code, ErrorCode::InvalidRequest);
    assert_eq!(failure.message, "The request was bad.");
}

#[test]
fn drain_marks_exited_once_stdout_closes() {
    let child = printing(&[]);
    let state = drained(child);
    assert!(lock(&state).exited);
    assert!(failure_of(&state).is_none());
}

#[test]
fn fiber_exited_error_parses_code_and_message() {
    let failure = fiber_exited_error(&exited_line("no_model", "No model is configured."))
        .expect("the verdict");
    assert_eq!(failure.code, ErrorCode::NoModel);
    assert_eq!(failure.message, "No model is configured.");
    assert_eq!(failure.retry_after, None);
    assert_eq!(failure.provider, None);
}

#[test]
fn fiber_exited_error_rejects_anything_but_a_verdict() {
    for line in [
        "not json".to_owned(),
        serde_json::json!({"kind": "turn_completed", "payload": {}}).to_string(),
        serde_json::json!({
            "kind": "fiber_exited",
            "payload": {"error": {"code": 7, "message": "x"}},
        })
        .to_string(),
        serde_json::json!({
            "kind": "fiber_exited",
            "payload": {"error": {"code": "no_model"}},
        })
        .to_string(),
        serde_json::json!({"kind": "fiber_exited", "payload": {}}).to_string(),
    ] {
        assert!(fiber_exited_error(&line).is_none(), "{line}");
    }
}
