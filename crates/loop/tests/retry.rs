//! Retrying a failed model call (`docs/model-routing.md`, "When a model call
//! fails"): each retry a new assistant message with its attempt number, each
//! wait on the injected clock, the step failing with the last error once the
//! retries run out.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use contract::ErrorCode;
use contract::events::{TextDelta, ToolCallArgumentsDelta, TurnOutcome};
use contract::provider::{CallError, Delta};
use contract::shapes::Failure;
use fakes::Scripted;
use r#loop::Retry;

use support::{DEADLINE, Session, TestTool, delivery, kinds};

fn failed(code: ErrorCode) -> Scripted {
    Scripted::failed(Failure {
        code,
        message: "The call failed.".into(),
        retry_after: None,
        provider: None,
    })
}

fn header_failed(code: ErrorCode, should_retry: Option<bool>) -> Scripted {
    Scripted {
        deltas: Vec::new(),
        end: Err(CallError::Failed {
            failure: Failure {
                code,
                message: "The call failed.".into(),
                retry_after: None,
                provider: None,
            },
            should_retry,
        }),
    }
}

fn waited(code: ErrorCode, retry_after: f64) -> Scripted {
    Scripted {
        deltas: Vec::new(),
        end: Err(CallError::Failed {
            failure: Failure {
                code,
                message: "The call failed.".into(),
                retry_after: Some(retry_after),
                provider: None,
            },
            should_retry: None,
        }),
    }
}

/// No backoff and no cap: a retryable failure retries at once, parking never.
fn no_wait() -> Retry {
    Retry {
        attempts: 3,
        initial: Duration::ZERO,
        max: Duration::ZERO,
    }
}

/// The `attempt` of every failed `assistant_message_completed`, in order.
fn attempts(lines: &[contract::Envelope]) -> Vec<u32> {
    lines
        .iter()
        .filter(|l| l.kind == "assistant_message_completed")
        .filter_map(|l| l.payload.get("attempt").and_then(|a| a.as_u64()))
        .map(|a| u32::try_from(a).unwrap())
        .collect()
}

/// The (`attempt`, `delay_ms`) of every `retry_scheduled`, in order.
fn scheduled(lines: &[contract::Envelope]) -> Vec<(u64, u64)> {
    lines
        .iter()
        .filter(|l| l.kind == "retry_scheduled")
        .map(|l| {
            (
                l.payload["attempt"].as_u64().unwrap(),
                l.payload["delay_ms"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn completed_count(lines: &[contract::Envelope], kind: &str) -> usize {
    lines.iter().filter(|l| l.kind == kind).count()
}

#[test]
fn a_rate_limit_then_a_reply_retries_after_2s() {
    let mut session = Session::new(
        vec![failed(ErrorCode::RateLimited), Scripted::text("Recovered.")],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    let clock = Arc::clone(&session.clock);
    let provider = Arc::clone(&session.provider);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        let until = clock.origin() + Duration::from_secs(2);
        assert!(
            clock.await_parked(until, DEADLINE),
            "the first backoff parks 2s out"
        );
        assert_eq!(
            provider.requests().len(),
            1,
            "no second request before the wait ends"
        );
        clock.advance(Duration::from_secs(2));
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Completed));
    });
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
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(attempts(&lines), [1]);
    let failed_message = &lines[5];
    let completed = &lines[6].payload;
    assert_eq!(completed["outcome"], "failed");
    assert_eq!(completed["attempt"], 1);
    assert_eq!(completed["error"]["code"], "rate_limited");
    // The retry is a new action: its `retry_scheduled` names the failed
    // message, the next attempt and the wait.
    let wait = &lines[7];
    assert_eq!(wait.action_id, failed_message.action_id);
    assert_eq!(wait.payload["code"], "rate_limited");
    assert_eq!(wait.payload["attempt"], 2);
    assert_eq!(wait.payload["delay_ms"], 2000);
    assert_ne!(lines[8].action_id, failed_message.action_id);
    // The failed attempt writes no `usage_recorded`; the retry's reply does,
    // before its completion.
    assert_eq!(completed_count(&lines, "usage_recorded"), 1);
    // The retry resends the same request.
    assert_eq!(session.requests().len(), 2);
    assert_eq!(session.requests()[0], session.requests()[1]);
}

#[test]
fn three_failures_then_success_waits_2_4_8s() {
    let mut session = Session::new(
        vec![
            failed(ErrorCode::RateLimited),
            failed(ErrorCode::RateLimited),
            failed(ErrorCode::RateLimited),
            Scripted::text("Recovered."),
        ],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    let clock = Arc::clone(&session.clock);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        for (until, wait) in [
            (2, Duration::from_secs(2)),
            (6, Duration::from_secs(4)),
            (14, Duration::from_secs(8)),
        ] {
            let until = clock.origin() + Duration::from_secs(until);
            assert!(clock.await_parked(until, DEADLINE), "parks {until:?} out");
            clock.advance(wait);
        }
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Completed));
    });
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
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_completed",
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_completed",
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(attempts(&lines), [1, 2, 3]);
    assert_eq!(
        scheduled(&lines),
        [(2, 2000), (3, 4000), (4, 8000)],
        "each wait doubles, and `retry_scheduled.attempt` is the next attempt"
    );
    assert_eq!(session.requests().len(), 4);
    assert_eq!(completed_count(&lines, "usage_recorded"), 1);
}

#[test]
fn four_failures_exhaust_the_retries_with_the_last_error() {
    let mut session = Session::new(
        vec![
            failed(ErrorCode::RateLimited),
            failed(ErrorCode::RateLimited),
            failed(ErrorCode::RateLimited),
            failed(ErrorCode::RateLimited),
        ],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    let clock = Arc::clone(&session.clock);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        for (until, wait) in [
            (2, Duration::from_secs(2)),
            (6, Duration::from_secs(4)),
            (14, Duration::from_secs(8)),
        ] {
            let until = clock.origin() + Duration::from_secs(until);
            assert!(clock.await_parked(until, DEADLINE), "parks {until:?} out");
            clock.advance(wait);
        }
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Failed));
    });
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
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_completed",
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_completed",
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(attempts(&lines), [1, 2, 3, 4]);
    // No `retry_scheduled` after the last attempt.
    assert_eq!(scheduled(&lines), [(2, 2000), (3, 4000), (4, 8000)]);
    let last = lines
        .iter()
        .rfind(|l| l.kind == "assistant_message_completed")
        .unwrap();
    assert_eq!(last.payload["outcome"], "failed");
    assert_eq!(last.payload["attempt"], 4);
    assert_eq!(last.payload["error"]["code"], "rate_limited");
    assert_eq!(lines.last().unwrap().payload["outcome"], "failed");
    assert_eq!(
        lines.last().unwrap().payload["error"],
        last.payload["error"]
    );
    assert_eq!(completed_count(&lines, "usage_recorded"), 0);
    assert_eq!(session.requests().len(), 4);
}

#[test]
fn a_never_retried_code_fails_at_once() {
    let mut session = Session::new(vec![failed(ErrorCode::InvalidRequest)], None);
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
    assert_eq!(attempts(&lines), [1]);
    assert!(lines.iter().all(|l| l.kind != "retry_scheduled"));
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn x_should_retry_false_stops_a_5xx_at_once() {
    let mut session = Session::new(
        vec![header_failed(ErrorCode::ProviderUnavailable, Some(false))],
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
    assert_eq!(attempts(&lines), [1]);
    assert!(lines.iter().all(|l| l.kind != "retry_scheduled"));
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn x_should_retry_true_retries_an_invalid_request() {
    let mut session = Session::new(
        vec![
            header_failed(ErrorCode::InvalidRequest, Some(true)),
            Scripted::text("Recovered."),
        ],
        None,
    )
    .retry(no_wait());
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
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
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(scheduled(&lines), [(2, 0)]);
    assert_eq!(completed_count(&lines, "usage_recorded"), 1);
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn an_asked_wait_within_the_cap_waits_the_larger() {
    let mut session = Session::new(
        vec![
            waited(ErrorCode::RateLimited, 30.0),
            Scripted::text("Recovered."),
        ],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    let clock = Arc::clone(&session.clock);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        let until = clock.origin() + Duration::from_secs(30);
        assert!(
            clock.await_parked(until, DEADLINE),
            "the 30s asked wait beats the 2s backoff"
        );
        clock.advance(Duration::from_secs(30));
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Completed));
    });
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
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(scheduled(&lines), [(2, 30_000)]);
}

#[test]
fn an_asked_wait_over_the_cap_fails_at_once_as_rate_limited() {
    let mut session = Session::new(vec![waited(ErrorCode::RateLimited, 90.0)], None);
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
    assert_eq!(attempts(&lines), [1]);
    let completed = &lines[6].payload;
    assert_eq!(completed["error"]["code"], "rate_limited");
    assert_eq!(completed["error"]["retry_after"], 90.0);
    assert!(lines.iter().all(|l| l.kind != "retry_scheduled"));
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn a_failed_stream_drops_its_partial_text_and_tool_calls() {
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let mut session = Session::with_tools(
        vec![
            Scripted {
                deltas: vec![
                    Delta::Text(TextDelta {
                        text: "partial".into(),
                    }),
                    Delta::ToolCallArguments(ToolCallArgumentsDelta {
                        index: 0,
                        name: Some("get_weather".into()),
                        text: "{\"city\": \"Paris\"}".into(),
                    }),
                ],
                end: Err(CallError::Failed {
                    failure: Failure {
                        code: ErrorCode::StreamIncomplete,
                        message: "The stream ended early.".into(),
                        retry_after: None,
                        provider: None,
                    },
                    should_retry: None,
                }),
            },
            Scripted::text("Recovered."),
        ],
        None,
        vec![tool.clone()],
    )
    .retry(no_wait());
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let dropped = session.lines();
    assert_eq!(
        kinds(&dropped),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "assistant_message_completed",
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    // The finished tool call never runs, and the partial text never reaches
    // the conversation: the retry's request holds neither.
    assert!(tool.ran().is_empty());
    assert!(dropped.iter().all(|l| l.kind != "tool_call_requested"));
    assert_eq!(session.requests().len(), 2);
    assert_eq!(session.requests()[0], session.requests()[1]);
}

#[test]
fn zero_attempts_never_retries() {
    let mut session = Session::new(vec![failed(ErrorCode::RateLimited)], None).retry(Retry {
        attempts: 0,
        ..Retry::default()
    });
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
    assert_eq!(attempts(&lines), [1]);
    assert!(lines.iter().all(|l| l.kind != "retry_scheduled"));
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn a_cancel_during_the_wait_ends_the_turn_interrupted() {
    let mut session = Session::new(
        vec![failed(ErrorCode::RateLimited), Scripted::text("Recovered.")],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    let clock = Arc::clone(&session.clock);
    let cancel = Arc::clone(&session.cancel);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        let until = clock.origin() + Duration::from_secs(2);
        assert!(clock.await_parked(until, DEADLINE), "the backoff parks");
        assert!(cancel.cancel());
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Interrupted));
    });
    let lines = session.lines();
    // The failed attempt's message already completed; no message is open,
    // and no second request was sent.
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
            "retry_scheduled",
            "turn_completed",
        ]
    );
    assert_eq!(attempts(&lines), [1]);
    assert_eq!(scheduled(&lines), [(2, 2000)]);
    assert_eq!(session.provider.requests().len(), 1);
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
}

#[test]
fn a_cancel_during_the_failing_call_is_interrupted_with_no_wait() {
    // The cancel lands after the failure is in hand but before the wait
    // starts, so no `retry_scheduled` follows the failed attempt.
    let mut session = Session::cancelling_after_reply(vec![failed(ErrorCode::RateLimited)], 1);
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
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(attempts(&lines), [1]);
    assert!(lines.iter().all(|l| l.kind != "retry_scheduled"));
    assert_eq!(session.provider.requests().len(), 1);
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
}

#[test]
fn a_clock_advance_during_the_failing_call_does_not_shorten_the_wait() {
    // The 10s advance lands during the failing call, before the wait
    // starts. The wait is the full 2s delay from its start: it parks 12s
    // out and ends only once 2s more are advanced.
    let mut session = Session::advancing(
        vec![failed(ErrorCode::RateLimited), Scripted::text("Recovered.")],
        Duration::from_secs(10),
    );
    session.inbox.send(delivery("hi")).unwrap();
    let clock = Arc::clone(&session.clock);
    let provider = Arc::clone(&session.provider);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        let until = clock.origin() + Duration::from_secs(12);
        assert!(
            clock.await_parked(until, DEADLINE),
            "the wait parks the full delay from its start"
        );
        assert_eq!(
            provider.requests().len(),
            1,
            "the earlier advance buys no retry"
        );
        clock.advance(Duration::from_secs(2));
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Completed));
    });
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
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(attempts(&lines), [1]);
    assert_eq!(scheduled(&lines), [(2, 2000)]);
    assert_eq!(session.provider.requests().len(), 2);
}
