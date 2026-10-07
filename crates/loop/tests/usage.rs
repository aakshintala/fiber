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

use std::sync::Arc;

use contract::commands::{Reply, ReplyAnswer};
use contract::events::{Decision, Event, TurnOutcome, UsageRecorded};
use contract::inbox::Delivery;
use contract::shapes::{Effect, Failure};
use contract::{Envelope, ErrorCode, RequestId, SessionId};
use fakes::{Scripted, call_usage, unnamed_usage};
use serde_json::{Value, json};

use support::{
    REVIEWER_MODEL, Session, TestTool, calls_reply, delivery, ignore, kinds, on_request,
};

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

fn failure() -> Failure {
    Failure {
        code: ErrorCode::InvalidRequest,
        message: "The call failed.".into(),
        retry_after_ms: None,
        provider: None,
    }
}

/// Asserts `recorded` carries an id Fiber minted: `fiber-` and 16 hex
/// digits (`docs/events.md`, `usage_recorded`).
fn assert_minted(recorded: &Envelope) {
    let id = recorded.payload["generation_id"].as_str().unwrap();
    let digits = id.strip_prefix("fiber-").unwrap();
    assert_eq!(digits.len(), 16, "{id}");
    assert!(digits.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
}

#[test]
fn a_call_failed_before_any_generation_writes_a_minted_usage() {
    let mut session = Session::new(vec![Scripted::failed(failure())], None);
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
    assert_minted(recorded);
    assert_eq!(recorded.action_id, started.action_id);
    assert_eq!(recorded.payload["tokens"]["input"], 0);
    assert_eq!(recorded.payload["tokens"]["output"], 0);
    assert_eq!(recorded.payload["input_bytes"], 1000);
    assert!(recorded.payload.get("cost").unwrap().is_null());
}

#[test]
fn a_call_cancelled_before_its_generation_writes_a_minted_usage() {
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
            "usage_recorded",
            "turn_completed",
        ]
    );
    let started = lines
        .iter()
        .find(|l| l.kind == "assistant_message_started")
        .unwrap();
    let recorded = lines.iter().find(|l| l.kind == "usage_recorded").unwrap();
    assert_minted(recorded);
    assert_eq!(recorded.action_id, started.action_id);
    assert_eq!(recorded.payload["tokens"]["input"], 0);
    assert_eq!(recorded.payload["tokens"]["output"], 0);
    assert_eq!(recorded.payload["input_bytes"], 1000);
    assert!(recorded.payload.get("cost").unwrap().is_null());
}

#[test]
fn a_failed_call_s_usage_counts_toward_the_budget() {
    let mut usage = call_usage("gen_failed");
    usage.tokens.input = 1_000;
    usage.tokens.output = 0;
    let model = r#loop::Model {
        reference: support::MODEL.into(),
        cost: Some(contract::provider::Cost {
            input: 1.0,
            output: 0.0,
            cache_read: None,
            cache_write: None,
            tiers: Vec::new(),
        }),
        subscription: false,
    };
    let mut session = Session::open(
        vec![Scripted::failed_after(
            Failure {
                code: ErrorCode::InvalidRequest,
                message: "The call failed.".into(),
                retry_after_ms: None,
                provider: None,
            },
            usage,
        )],
        Vec::new(),
        Vec::new(),
        model,
    )
    .budget(Some(0.0005));
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let first = session.lines();
    assert_eq!(first.last().unwrap().payload["outcome"], "failed");
    assert_eq!(
        first.last().unwrap().payload["error"]["code"],
        "invalid_request"
    );
    session.inbox.send(delivery("again")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let second = session.lines();
    assert_eq!(second.last().unwrap().payload["outcome"], "failed");
    assert_eq!(
        second.last().unwrap().payload["error"]["code"],
        "budget_exceeded"
    );
    assert_eq!(session.requests().len(), 1);
}

fn paris() -> Value {
    json!({"city": "Paris"})
}

fn shell() -> Arc<TestTool> {
    let mut tool = TestTool::declaring("shell", "Ran it.", vec![Effect::Executes], None);
    tool.subject = None;
    tool.prefix = None;
    Arc::new(tool)
}

fn allow() -> ReplyAnswer {
    ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: None,
        remember: None,
    }
}

fn answer(id: RequestId, answer: ReplyAnswer) -> Delivery {
    Delivery::Reply(
        Reply {
            request_id: id,
            answer,
        },
        ignore(),
    )
}

#[test]
fn a_failed_review_after_its_generation_writes_its_usage() {
    let tool = shell();
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn contract::tool::Tool>],
    );
    let prices = contract::provider::Cost {
        input: 2.0,
        output: 10.0,
        cache_read: None,
        cache_write: None,
        tiers: Vec::new(),
    };
    let reviewer = session.reviewer_priced(
        vec![Scripted::failed_after(
            Failure {
                code: ErrorCode::Timeout,
                message: "the reviewer timed out".into(),
                retry_after_ms: None,
                provider: None,
            },
            call_usage("gen_review"),
        )],
        r#loop::BlockLimits::default(),
        Some(prices.clone()),
    );
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| inbox.send(answer(id, allow())).unwrap()
    });
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    answered.join().unwrap();
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
            "usage_recorded",
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
    let review = &lines[11];
    assert_eq!(review.kind, "usage_recorded");
    assert!(review.action_id.is_none());
    assert_eq!(review.payload["generation_id"], "gen_review");
    assert_eq!(review.payload["model"], REVIEWER_MODEL);
    let tokens = &call_usage("gen_review").tokens;
    let want = (2.0 * tokens.input as f64 + 10.0 * tokens.output as f64) / 1_000_000.0;
    assert_eq!(review.payload["cost"].as_f64(), Some(want));
    assert_eq!(reviewer.requests().len(), 1);
    assert_eq!(tool.ran().len(), 1);
}

/// A call that fails before its provider names a generation, having seen
/// 10 input and 3 output tokens.
fn unnamed_failure() -> Scripted {
    Scripted::failed_after(failure(), unnamed_usage())
}

/// One turn of `session`, and its one `usage_recorded` payload.
fn only_record(session: &mut Session) -> UsageRecorded {
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    let recorded: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "usage_recorded")
        .collect();
    assert_eq!(recorded.len(), 1);
    assert_minted(recorded[0]);
    serde_json::from_value(Value::Object(recorded[0].payload.clone())).unwrap()
}

#[test]
fn a_delegate_s_unnamed_call_copied_into_its_parent_is_counted_once() {
    let mut child = Session::new(vec![unnamed_failure()], None);
    let copied = only_record(&mut child);
    let mut parent = Session::new(vec![unnamed_failure()], None);
    let own = only_record(&mut parent);
    assert_ne!(copied.generation_id, own.generation_id);
    // The copy as the delegate's stream delivers it, then the copy a resume
    // writes for a delegate marked `orphaned` (`docs/delegates.md`,
    // "Streams"; `docs/loop.md`, "Spending budget").
    let copy = UsageRecorded {
        origin_session_id: Some(SessionId("s_child".into())),
        ..copied
    };
    for _ in 0..2 {
        parent
            .log
            .append(&Event::UsageRecorded(copy.clone()), None, None)
            .unwrap();
    }
    r#loop::fiber_exited(&parent.log, &parent.dir, Ok(()), false, None).unwrap();
    let all = log::read(&parent.dir).unwrap();
    let exited = all.last().unwrap();
    assert_eq!(exited.kind, "fiber_exited");
    // The parent's call and the delegate's, each once.
    let usage = &exited.payload["usage"]["tokens"];
    assert_eq!(usage["input"], 20);
    assert_eq!(usage["output"], 6);
}

#[test]
fn a_call_failed_before_its_generation_records_the_tokens_it_saw() {
    let mut session = Session::new(vec![unnamed_failure()], None);
    let recorded = only_record(&mut session);
    assert_eq!(recorded.tokens.input, 10);
    assert_eq!(recorded.tokens.output, 3);
    assert_eq!(recorded.input_bytes, 1000);
    assert_eq!(recorded.cost, None);
}

#[test]
fn a_minted_record_s_cost_is_null_on_a_priced_model() {
    let model = r#loop::Model {
        reference: support::MODEL.into(),
        cost: Some(contract::provider::Cost {
            input: 1.0,
            output: 2.0,
            cache_read: None,
            cache_write: None,
            tiers: Vec::new(),
        }),
        subscription: false,
    };
    let mut session = Session::open(vec![unnamed_failure()], Vec::new(), Vec::new(), model);
    let recorded = only_record(&mut session);
    assert_eq!(recorded.tokens.input, 10);
    assert_eq!(recorded.cost, None);
}
