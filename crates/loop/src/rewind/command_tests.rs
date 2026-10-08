//! Tests for the `rewind` command's point: what counts as a step boundary
//! in one log (`docs/events.md`, "Rewind"), and the default point.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use contract::{ActionId, Envelope, Seq, SessionId};

use super::{Target, point_for};

fn line(kind: &str, seq: u64, action: Option<&str>) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s_0123456789abcdef".into()),
        ts: 1,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|action| ActionId(action.into())),
        seq: Some(Seq(seq)),
        payload: serde_json::Map::new(),
    }
}

fn own_target() -> Target {
    Target {
        session: SessionId("s_0123456789abcdef".into()),
        dir: std::path::PathBuf::from("/sessions/s_0123456789abcdef"),
        bound: None,
    }
}

fn ancestor_target(to: u64) -> Target {
    Target {
        session: SessionId("s_aaaaaaaaaaaaaaaa".into()),
        dir: std::path::PathBuf::from("/sessions/s_aaaaaaaaaaaaaaaa"),
        bound: Some(to),
    }
}

fn boundary() -> String {
    "Line 4 is not a step boundary: the start of a turn, just after the person's input, or just after a batch of tool results.".to_owned()
}

/// One log with a turn starting at 5: `turn_started` is the line after the
/// point 4.
fn two_turns() -> Vec<Envelope> {
    vec![
        line("session_started", 0, None),
        line("turn_started", 1, None),
        line("turn_completed", 2, None),
        line("turn_started", 5, None),
        line("turn_completed", 6, None),
    ]
}

#[test]
fn a_turn_start_after_the_point_is_a_step_boundary() {
    let lines = two_turns();
    assert_eq!(point_for(&lines, &own_target(), Some(Seq(4))), Ok(4));
}

#[test]
fn a_step_start_after_the_point_is_a_step_boundary() {
    let lines = vec![
        line("session_started", 0, None),
        line("turn_started", 1, None),
        line("step_started", 3, None),
    ];
    assert_eq!(point_for(&lines, &own_target(), Some(Seq(2))), Ok(2));
}

#[test]
fn a_step_start_after_a_turn_start_is_a_step_boundary() {
    let lines = vec![
        line("session_started", 0, None),
        line("turn_started", 4, None),
        line("step_started", 5, None),
    ];
    assert_eq!(point_for(&lines, &own_target(), Some(Seq(3))), Ok(3));
}

#[test]
fn other_kinds_after_the_point_are_not_step_boundaries() {
    for kind in [
        "tool_call_completed",
        "turn_completed",
        "fiber_started",
        "session_started",
    ] {
        let lines = vec![line("session_started", 0, None), line(kind, 5, None)];
        let refused = point_for(&lines, &own_target(), Some(Seq(4))).unwrap_err();
        assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
        assert_eq!(refused.message, boundary(), "{kind}");
    }
}

#[test]
fn the_last_line_is_not_a_step_boundary() {
    let lines = two_turns();
    let last = lines.last().unwrap().seq.unwrap().0;
    let refused = point_for(&lines, &own_target(), Some(Seq(last))).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        format!(
            "Line {last} is not a step boundary: the start of a turn, just after the person's input, or just after a batch of tool results."
        )
    );
}

#[test]
fn a_point_past_the_end_is_not_a_step_boundary() {
    let lines = two_turns();
    let refused = point_for(&lines, &own_target(), Some(Seq(60))).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        "Line 60 is not a step boundary: the start of a turn, just after the person's input, or just after a batch of tool results."
    );
}

#[test]
fn an_unanswered_call_before_the_point_refuses_it() {
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_requested", 1, Some("a_1")),
        line("turn_started", 5, None),
        line("tool_call_completed", 6, Some("a_1")),
    ];
    let refused = point_for(&lines, &own_target(), Some(Seq(4))).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(refused.message, boundary());
}

#[test]
fn a_completion_for_a_call_after_the_point_accepts_it() {
    let lines = vec![
        line("session_started", 0, None),
        line("turn_started", 5, None),
        line("tool_call_requested", 6, Some("a_1")),
        line("tool_call_completed", 7, Some("a_1")),
    ];
    assert_eq!(point_for(&lines, &own_target(), Some(Seq(4))), Ok(4));
}

#[test]
fn a_completion_for_a_call_at_the_point_refuses_it() {
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_requested", 4, Some("a_1")),
        line("turn_started", 5, None),
        line("tool_call_completed", 6, Some("a_1")),
    ];
    let refused = point_for(&lines, &own_target(), Some(Seq(4))).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(refused.message, boundary());
}

#[test]
fn a_point_at_the_bound_is_accepted() {
    let lines = two_turns();
    assert_eq!(point_for(&lines, &ancestor_target(4), Some(Seq(4))), Ok(4));
}

#[test]
fn a_point_past_the_bound_is_not_in_this_sessions_history() {
    let lines = two_turns();
    let refused = point_for(&lines, &ancestor_target(4), Some(Seq(5))).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        "Line 5 of session s_aaaaaaaaaaaaaaaa is not in this session's history."
    );
}

#[test]
fn the_default_point_is_the_latest_turn_start_minus_one() {
    let lines = two_turns();
    assert_eq!(point_for(&lines, &own_target(), None), Ok(4));
}

#[test]
fn the_default_point_with_one_turn_is_before_it() {
    let lines = vec![
        line("session_started", 0, None),
        line("fiber_started", 1, None),
        line("turn_started", 2, None),
        line("turn_completed", 3, None),
    ];
    assert_eq!(point_for(&lines, &own_target(), None), Ok(1));
}

#[test]
fn the_default_point_with_no_turn_is_invalid_arguments() {
    let lines = vec![
        line("session_started", 0, None),
        line("fiber_started", 1, None),
    ];
    let refused = point_for(&lines, &own_target(), None).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::InvalidArguments);
    assert_eq!(refused.message, "This session has no turn to rewind to.");
}

#[test]
fn the_default_point_stops_at_the_bound() {
    // The latest turn starts past the bound: the default is the latest
    // turn start at or before it.
    let lines = two_turns();
    assert_eq!(point_for(&lines, &ancestor_target(2), None), Ok(0));
}

#[test]
fn the_default_point_with_no_turn_before_the_bound_is_invalid_arguments() {
    let lines = two_turns();
    let refused = point_for(&lines, &ancestor_target(0), None).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::InvalidArguments);
    assert_eq!(refused.message, "This session has no turn to rewind to.");
}

#[test]
fn a_point_without_a_representable_successor_is_not_a_step_boundary() {
    // `u64::MAX` has no next line: the lookup for it must refuse, not
    // wrap or panic.
    let lines = two_turns();
    let refused = point_for(&lines, &own_target(), Some(Seq(u64::MAX))).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        format!(
            "Line {} is not a step boundary: the start of a turn, just after the person's input, or just after a batch of tool results.",
            u64::MAX
        )
    );
}

#[test]
fn a_completion_at_the_point_answers_a_call_before_it() {
    // Only a completion after the point blocks it: one at the point is
    // the answer, not a wait.
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_requested", 1, Some("a_1")),
        line("tool_call_completed", 4, Some("a_1")),
        line("turn_started", 5, None),
    ];
    assert_eq!(point_for(&lines, &own_target(), Some(Seq(4))), Ok(4));
}
