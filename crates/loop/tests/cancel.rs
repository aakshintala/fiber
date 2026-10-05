//! Cancelling a running turn (`docs/architecture.md`, "Cancellation"): the
//! signal ends the turn `interrupted`, and queued steering starts the next
//! turn at once.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::sync::Arc;

use contract::ErrorCode;
use contract::commands::{Reply, ReplyAnswer};
use contract::events::{Decision, TurnOutcome};

use support::{
    DEADLINE, Gate, Script, Session, Tap, TestTool, delivery, kinds, steer, tool_call_reply,
};

#[test]
fn cancel_mid_stream_writes_no_completion_and_ends_interrupted() {
    let (mut session, blocking) = Session::blocking(vec![steer("next")]);
    session.inbox.send(delivery("hi")).unwrap();
    let cancel = Arc::clone(&session.cancel);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        blocking.wait_started(DEADLINE);
        assert!(cancel.cancel());
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Interrupted));
    });
    let lines = session.lines();
    // No `assistant_message_completed`, and nothing after the turn but its
    // interrupted end: the steer is still waiting in the inbox.
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
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
    // The steer queued before the cancel starts the next turn at once, as
    // its `turn_started` input.
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let next = session.lines();
    assert_eq!(
        kinds(&next),
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let started = next.iter().find(|l| l.kind == "turn_started").unwrap();
    let input: Vec<&str> = started.payload["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(input, ["next"]);
}

#[test]
fn a_cancel_landing_between_steps_sends_no_request() {
    let gate = Arc::new(Gate::default());
    let mut tool = TestTool::reads("get_weather", "Sunny.");
    tool.script = vec![Script::Wait(Arc::clone(&gate))];
    let tool = Arc::new(tool);
    let mut session = Session::with_tools(
        vec![
            tool_call_reply("", &["get_weather"]),
            fakes::Scripted::text("Done."),
        ],
        None,
        vec![tool],
    );
    session.inbox.send(delivery("hi")).unwrap();
    let tap = Tap::new(&session.log);
    let cancel = Arc::clone(&session.cancel);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        // The tool started its call, so the turn is armed: the cancel
        // lands while the step runs, before the next step's request.
        tap.wait_for("tool_call_started");
        assert!(cancel.cancel());
        gate.open();
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Interrupted));
    });
    gate.check("tool");
    // The cancelled turn never sent its second request.
    assert_eq!(session.requests().len(), 1);
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
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.kind == "assistant_message_started")
            .count(),
        1
    );
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
}

use contract::events::{Event, Progress};
use contract::inbox::{Ack, Delivery};
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::{Effect, Failure};

fn paris() -> serde_json::Value {
    serde_json::json!({"city": "Paris"})
}

/// A tool whose calls execute with `subject`.
fn shell(subject: &str) -> Arc<TestTool> {
    let mut tool = TestTool::declaring("shell", "Ran it.", vec![Effect::Executes], None);
    tool.subject = Some(subject.to_owned());
    Arc::new(tool)
}

fn standing(tool: &str, decision: RuleDecision, prefix: &str) -> StandingRules {
    StandingRules {
        global: vec![Rule {
            decision,
            tool: tool.into(),
            prefix: prefix.into(),
            added: None,
            session_id: None,
        }],
        project: Vec::new(),
    }
}

/// The `tool_call_completed` lines, in request order.
fn completed(lines: &[contract::Envelope]) -> Vec<&contract::Envelope> {
    lines
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .collect()
}

/// The text of a completed call's content.
fn text(line: &contract::Envelope) -> String {
    line.payload["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part["text"].as_str().unwrap())
        .collect()
}

#[test]
fn a_running_call_completes_cancelled_and_a_denied_call_behind_it_does_too() {
    let mut slow = TestTool::reads("slow", "Slow done.");
    slow.script = vec![
        Script::Emit(Box::new(Event::ToolCallDelta(Progress {
            text: Some("working".into()),
            details: None,
        }))),
        Script::WaitCancel,
    ];
    let slow = Arc::new(slow);
    let blocked = shell("npm publish");
    let mut session = Session::with_tools(
        vec![
            support::calls_reply("", &[("slow", paris()), ("shell", paris())]),
            fakes::Scripted::text("Done."),
        ],
        None,
        vec![
            slow.clone() as Arc<dyn contract::tool::Tool>,
            blocked.clone() as _,
        ],
    );
    session
        .rules
        .set(standing("shell", RuleDecision::Deny, "npm publish"));
    session.inbox.send(delivery("hi")).unwrap();
    let tap = Tap::new(&session.log);
    let cancel = Arc::clone(&session.cancel);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        tap.wait_for("tool_call_started");
        assert!(cancel.cancel());
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Interrupted));
    });
    // The tool saw the cancel: it waited on the signal itself.
    assert!(slow.cancelled.lock().unwrap().contains(&true));
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
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_resolved",
            "tool_call_started",
            "tool_call_delta",
            "tool_call_completed",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    let done = completed(&lines);
    assert_eq!(done.len(), 2);
    // The running call carries its tool's own content, with no error.
    assert_eq!(done[0].payload["status"], "cancelled");
    assert_eq!(text(done[0]), "Slow done.");
    assert!(
        done[0]
            .payload
            .get("error")
            .is_none_or(serde_json::Value::is_null)
    );
    // No delta follows its completion: the flush went out before it.
    let slow_id = done[0].action_id.clone().unwrap();
    let at = lines.iter().position(|l| l == done[0]).unwrap();
    assert!(
        lines[at..]
            .iter()
            .filter(|l| l.kind == "tool_call_delta")
            .all(|l| l.action_id.as_ref() != Some(&slow_id))
    );
    // The denied call behind it completes cancelled, and its deny line stays.
    assert_eq!(done[1].payload["status"], "cancelled");
    let resolved: Vec<&contract::Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["decision"], "deny");
    assert_eq!(resolved[0].payload["decided_by"], "standing_rule");
    assert_eq!(resolved[0].action_id, done[1].action_id);
    // Neither denied call started, and the turn ends interrupted.
    assert!(blocked.ran.lock().unwrap().is_empty());
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
}

#[test]
fn a_call_returning_an_error_after_cancel_keeps_failed() {
    let gate = Arc::new(Gate::default());
    let mut tool = TestTool::failing("brittle", ErrorCode::ToolError);
    tool.script = vec![Script::Wait(Arc::clone(&gate))];
    let tool = Arc::new(tool);
    let mut session = Session::with_tools(
        vec![support::calls_reply("", &[("brittle", paris())])],
        None,
        vec![tool as Arc<dyn contract::tool::Tool>],
    );
    session.inbox.send(delivery("hi")).unwrap();
    let tap = Tap::new(&session.log);
    let cancel = Arc::clone(&session.cancel);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        tap.wait_for("tool_call_started");
        assert!(cancel.cancel());
        gate.open();
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Interrupted));
    });
    gate.check("brittle");
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
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    // An error is never cancelled: the failure and its code stay.
    assert_eq!(done[0].payload["status"], "failed");
    assert_eq!(done[0].payload["error"]["code"], "tool_error");
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
}

#[test]
fn a_pending_approval_is_denied_by_cancel_and_later_calls_never_start() {
    let ask = shell("npm publish");
    let later = Arc::new(TestTool::reads("later", "Later."));
    let mut session = Session::with_tools(
        vec![
            support::calls_reply("", &[("shell", paris()), ("later", paris())]),
            fakes::Scripted::text("Done."),
        ],
        None,
        vec![
            ask.clone() as Arc<dyn contract::tool::Tool>,
            later.clone() as _,
        ],
    );
    session
        .rules
        .set(standing("shell", RuleDecision::Ask, "npm publish"));
    session.inbox.send(delivery("hi")).unwrap();
    let inbox = session.inbox.clone();
    let cancel = Arc::clone(&session.cancel);
    let answered = support::on_request(&session, move |_| {
        // As the door does: the cancel first, then the wake.
        assert!(cancel.cancel());
        inbox.send(Delivery::Cancelled).unwrap();
    });
    assert_eq!(session.turn(), Some(TurnOutcome::Interrupted));
    answered.join().unwrap();
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
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    let requested: Vec<&contract::Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_requested")
        .collect();
    assert_eq!(requested.len(), 1);
    let request_id = requested[0].payload["request_id"].clone();
    let resolved: Vec<&contract::Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["decision"], "deny");
    assert_eq!(resolved[0].payload["decided_by"], "cancel");
    assert_eq!(resolved[0].payload["request_id"], request_id);
    let done = completed(&lines);
    assert_eq!(done.len(), 2);
    assert_eq!(done[0].payload["status"], "cancelled");
    // The call after it never started: cancelled, with no permission lines.
    assert_eq!(done[1].payload["status"], "cancelled");
    assert_eq!(text(done[1]), "Cancelled before it ran.");
    assert!(
        lines
            .iter()
            .filter(|l| l.kind == "tool_call_started")
            .count()
            == 0
    );
    assert!(ask.ran.lock().unwrap().is_empty());
    assert!(later.ran.lock().unwrap().is_empty());
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
}

#[test]
fn a_cancelled_review_completes_cancelled() {
    let tool = shell("rm -rf /tmp/vital");
    let mut session = Session::with_tools(
        vec![support::calls_reply("", &[("shell", paris())])],
        None,
        vec![tool as Arc<dyn contract::tool::Tool>],
    );
    let review_blocking = Arc::new(fakes::BlockingProvider::default());
    let looped = session.looped.take().unwrap().reviewer(
        Ok(r#loop::Reviewer {
            provider: review_blocking.clone() as Arc<dyn contract::provider::Provider>,
            model: r#loop::Model {
                reference: support::REVIEWER_MODEL.into(),
                cost: None,
                subscription: false,
            },
        }),
        r#loop::BlockLimits::default(),
    );
    session.looped = Some(looped);
    session.inbox.send(delivery("hi")).unwrap();
    let cancel = Arc::clone(&session.cancel);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        review_blocking.wait_started(DEADLINE);
        assert!(cancel.cancel());
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Interrupted));
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
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_resolved",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    // The review keeps its resolved line, decided by the cancel.
    let resolved: Vec<&contract::Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["decision"], "deny");
    assert_eq!(resolved[0].payload["decided_by"], "cancel");
    // Its completion is cancelled, not denied, and nothing ran.
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["status"], "cancelled");
    assert!(lines.iter().all(|l| l.kind != "tool_call_started"));
    assert_eq!(session.requests().len(), 1);
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
}

#[test]
fn an_approved_call_cancelled_while_a_later_call_waits_never_starts() {
    let fast = Arc::new(TestTool::reads("fast", "Fast done."));
    let ask = shell("npm publish");
    let mut session = Session::with_tools(
        vec![support::calls_reply(
            "",
            &[("fast", paris()), ("shell", paris())],
        )],
        None,
        vec![
            fast.clone() as Arc<dyn contract::tool::Tool>,
            ask.clone() as _,
        ],
    );
    session
        .rules
        .set(standing("shell", RuleDecision::Ask, "npm publish"));
    session.inbox.send(delivery("hi")).unwrap();
    let inbox = session.inbox.clone();
    let cancel = Arc::clone(&session.cancel);
    let answered = support::on_request(&session, move |_| {
        // The first call is already approved; the cancel lands while the
        // second waits for its answer.
        assert!(cancel.cancel());
        inbox.send(Delivery::Cancelled).unwrap();
    });
    assert_eq!(session.turn(), Some(TurnOutcome::Interrupted));
    answered.join().unwrap();
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
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    // The approved call never started: no `tool_call_started`, and neither
    // tool ran.
    assert!(lines.iter().all(|l| l.kind != "tool_call_started"));
    assert!(fast.ran.lock().unwrap().is_empty());
    assert!(ask.ran.lock().unwrap().is_empty());
    let done = completed(&lines);
    assert_eq!(done.len(), 2);
    assert!(done.iter().all(|l| l.payload["status"] == "cancelled"));
    assert_eq!(text(done[0]), "Cancelled before it ran.");
    // The waiting call keeps its deny-by-cancel line.
    let resolved: Vec<&contract::Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["decided_by"], "cancel");
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
}

#[test]
fn a_reply_queued_ahead_of_the_cancel_wake_is_rejected() {
    let ask = shell("npm publish");
    let mut session = Session::with_tools(
        vec![support::calls_reply("", &[("shell", paris())])],
        None,
        vec![ask.clone() as Arc<dyn contract::tool::Tool>],
    );
    session
        .rules
        .set(standing("shell", RuleDecision::Ask, "npm publish"));
    session.inbox.send(delivery("hi")).unwrap();
    let inbox = session.inbox.clone();
    let cancel = Arc::clone(&session.cancel);
    let (rejected, waiting) = std::sync::mpsc::channel();
    let answered = support::on_request(&session, move |request_id| {
        // The cancel has already landed when the person's reply arrives
        // ahead of the wake, so the reply answers nothing.
        assert!(cancel.cancel());
        inbox
            .send(Delivery::Reply(
                Reply {
                    request_id,
                    answer: ReplyAnswer::Approval {
                        decision: Decision::Allow,
                        feedback: None,
                        remember: None,
                    },
                },
                Ack(Box::new(move |answer| {
                    rejected
                        .send(
                            answer
                                .err()
                                .map(|rejection| (rejection.code, rejection.message)),
                        )
                        .unwrap();
                })),
            ))
            .unwrap();
        inbox.send(Delivery::Cancelled).unwrap();
    });
    assert_eq!(session.turn(), Some(TurnOutcome::Interrupted));
    answered.join().unwrap();
    // Even the allow was rejected stale: the request was already over.
    let (code, message) = waiting
        .recv_timeout(DEADLINE)
        .expect("the reply's answer")
        .expect("the reply was rejected");
    assert_eq!(code, ErrorCode::StaleRequest);
    assert_eq!(message, "That request is no longer pending.");
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
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    // The approval resolved deny by cancel, the call never ran, and the
    // turn ends interrupted.
    let resolved: Vec<&contract::Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["decision"], "deny");
    assert_eq!(resolved[0].payload["decided_by"], "cancel");
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["status"], "cancelled");
    assert!(ask.ran.lock().unwrap().is_empty());
    assert!(lines.iter().all(|l| l.kind != "tool_call_started"));
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
}

#[test]
fn a_cancel_after_the_last_reply_interrupts_the_turn() {
    let mut session = Session::cancelling_after_reply(vec![fakes::Scripted::text("Done.")], 1);
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
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(session.requests().len(), 1);
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
}

#[test]
fn a_cancel_after_a_failed_reply_leaves_the_turn_failed() {
    let mut session = Session::cancelling_after_reply(
        vec![fakes::Scripted::failed(Failure {
            code: ErrorCode::ToolError,
            message: "It broke.".into(),
            retry_after: None,
            provider: None,
        })],
        1,
    );
    session.inbox.send(delivery("hi")).unwrap();
    // A failed turn stays failed: the cancel never turns it interrupted.
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
    let last = lines.last().unwrap();
    assert_eq!(last.payload["outcome"], "failed");
    assert_eq!(last.payload["error"]["code"], "tool_error");
}

#[test]
fn a_stale_cancel_wake_does_not_end_a_later_approval() {
    let ask = shell("npm publish");
    let mut session = Session::with_tools(
        vec![
            support::calls_reply("", &[("shell", paris())]),
            fakes::Scripted::text("Done."),
        ],
        None,
        vec![ask.clone() as Arc<dyn contract::tool::Tool>],
    );
    session
        .rules
        .set(standing("shell", RuleDecision::Ask, "npm publish"));
    session.inbox.send(delivery("hi")).unwrap();
    let inbox = session.inbox.clone();
    let answered = support::on_request(&session, move |request_id| {
        // A wake from an earlier turn's cancel, queued ahead of the reply
        // with no cancel behind it: the wait goes on.
        inbox.send(Delivery::Cancelled).unwrap();
        inbox
            .send(Delivery::Reply(
                Reply {
                    request_id,
                    answer: ReplyAnswer::Approval {
                        decision: Decision::Allow,
                        feedback: None,
                        remember: None,
                    },
                },
                support::ignore(),
            ))
            .unwrap();
    });
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    answered.join().unwrap();
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
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    // The person's reply still decided: allowed by the person, and the
    // call ran.
    let resolved: Vec<&contract::Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["decision"], "allow");
    assert_eq!(resolved[0].payload["decided_by"], "person");
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["status"], "completed");
    assert_eq!(ask.ran.lock().unwrap().len(), 1);
    assert_eq!(lines.last().unwrap().payload["outcome"], "completed");
}

#[test]
fn kept_steering_starts_the_next_turn_without_waiting() {
    let ask = shell("npm publish");
    let mut session = Session::with_tools(
        vec![
            support::calls_reply("", &[("shell", paris())]),
            fakes::Scripted::text("Done."),
        ],
        None,
        vec![ask.clone() as Arc<dyn contract::tool::Tool>],
    );
    session
        .rules
        .set(standing("shell", RuleDecision::Ask, "npm publish"));
    session.inbox.send(delivery("hi")).unwrap();
    let inbox = session.inbox.clone();
    let cancel = Arc::clone(&session.cancel);
    let answered = support::on_request(&session, move |_| {
        // Held during the approval wait, then the cancel ends the turn
        // with the steer still queued.
        inbox.send(steer("next")).unwrap();
        assert!(cancel.cancel());
        inbox.send(Delivery::Cancelled).unwrap();
    });
    assert_eq!(session.turn(), Some(TurnOutcome::Interrupted));
    answered.join().unwrap();
    let first = session.lines();
    assert_eq!(
        kinds(&first),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
            "steering_queue",
            "permission_resolved",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    // The kept steer starts the next turn at once, with nothing new sent:
    // the inbox is empty, so waiting on it would hang the turn instead.
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "steering_queue",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let started = lines.iter().find(|l| l.kind == "turn_started").unwrap();
    let input: Vec<&str> = started.payload["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(input, ["next"]);
    // The queue drained into the turn: the last queue line is empty.
    let queues: Vec<&contract::Envelope> = lines
        .iter()
        .filter(|l| l.kind == "steering_queue")
        .collect();
    assert!(!queues.is_empty());
    assert_eq!(
        queues.last().unwrap().payload["messages"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}
