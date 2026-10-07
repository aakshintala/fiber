//! A failed or cancelled model call writes its `usage_recorded`
//! (`docs/events.md`, "Usage and notices").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use contract::ErrorCode;
use contract::events::TurnOutcome;
use contract::shapes::Failure;
use fakes::{Scripted, call_usage};

use support::{Session, delivery, kinds};

fn failed_after(generation: &str, input: u64, output: u64, bytes: u64) -> Scripted {
    let mut usage = call_usage(generation);
    usage.tokens.input = input;
    usage.tokens.output = output;
    usage.input_size.bytes = bytes;
    Scripted::failed_after(
        Failure {
            code: ErrorCode::InvalidRequest,
            message: "The call failed.".into(),
            retry_after_ms: None,
            provider: None,
        },
        usage,
    )
}

#[test]
fn a_call_failed_after_its_generation_writes_its_usage_before_failing() {
    let mut session = Session::new(vec![failed_after("gen_failed", 7, 2, 900)], None);
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let started = lines
        .iter()
        .find(|l| l.kind == "assistant_message_started")
        .unwrap();
    let recorded = lines.iter().find(|l| l.kind == "usage_recorded").unwrap();
    assert_eq!(recorded.action_id, started.action_id);
    assert_eq!(recorded.payload["generation_id"], "gen_failed");
    assert_eq!(recorded.payload["tokens"]["input"], 7);
    assert_eq!(recorded.payload["tokens"]["cache_read"], 0);
    assert_eq!(recorded.payload["tokens"]["output"], 2);
    assert_eq!(recorded.payload["input_bytes"], 900);
    assert!(recorded.payload.get("cost").unwrap().is_null());
    let completed = lines
        .iter()
        .find(|l| l.kind == "assistant_message_completed")
        .unwrap();
    assert_eq!(completed.payload["outcome"], "failed");
    assert_eq!(lines.last().unwrap().payload["outcome"], "failed");
}

#[test]
fn a_call_cancelled_after_its_generation_writes_its_usage_before_the_turn_ends() {
    let mut session = Session::cancelling_at_call(
        vec![Scripted::cancelled_after(call_usage("gen_cancelled"))],
        1,
        vec![],
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Interrupted));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "turn_completed",
        ]
    );
    assert!(
        lines
            .iter()
            .all(|l| l.kind != "assistant_message_completed")
    );
    let recorded = lines.iter().find(|l| l.kind == "usage_recorded").unwrap();
    assert_eq!(recorded.payload["generation_id"], "gen_cancelled");
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
}

#[test]
fn a_call_failed_before_any_generation_writes_no_usage() {
    let mut session = Session::new(
        vec![Scripted::failed(Failure {
            code: ErrorCode::InvalidRequest,
            message: "The call failed.".into(),
            retry_after_ms: None,
            provider: None,
        })],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert!(lines.iter().all(|l| l.kind != "usage_recorded"));
}

#[test]
fn a_call_cancelled_before_any_generation_writes_no_usage() {
    let mut session = Session::cancelling_at_call(vec![Scripted::text("never")], 1, vec![]);
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Interrupted));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "turn_completed",
        ]
    );
    assert!(lines.iter().all(|l| l.kind != "usage_recorded"));
}
