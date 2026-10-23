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

use contract::events::{Event, TurnOutcome, UsageRecorded};
use contract::shapes::Failure;
use contract::{Envelope, ErrorCode, SessionId};
use fakes::{Scripted, call_usage, unnamed_usage};
use serde_json::Value;

use support::{
    REVIEWER_MODEL, Session, allow, calls_reply, child_usage, delivery, kinds, on_request, paris,
    reply_delivery, send_child, shell,
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

#[test]
fn a_failed_review_after_its_generation_writes_its_usage() {
    let tool = shell(None, None);
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
        move |id| inbox.send(reply_delivery(id, allow())).unwrap()
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

/// The complete ordered event kinds of a turn whose call fails after an
/// unnamed generation (`docs/testing.md`, "Event streams").
const FAILED_UNNAMED_KINDS: [&str; 9] = [
    "session_started",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

/// One turn of `session`, and its one `usage_recorded` payload. `expected`
/// is the complete ordered list of event kinds, so a duplicated, missing or
/// reordered event fails (`docs/testing.md`, "Event streams").
fn only_record(session: &mut Session, expected: &[&str]) -> UsageRecorded {
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    assert_eq!(kinds(&lines), expected);
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
    let copied = only_record(&mut child, &FAILED_UNNAMED_KINDS);
    let mut parent = Session::new(vec![unnamed_failure()], None);
    let own = only_record(&mut parent, &FAILED_UNNAMED_KINDS);
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
    let mut expected = FAILED_UNNAMED_KINDS.to_vec();
    expected.extend(["usage_recorded", "usage_recorded", "fiber_exited"]);
    assert_eq!(kinds(&all), expected);
    let exited = all.last().unwrap();
    // The parent's call and the delegate's, each once.
    let usage = &exited.payload["usage"]["tokens"];
    assert_eq!(usage["input"], 20);
    assert_eq!(usage["output"], 6);
}

#[test]
fn a_call_failed_before_its_generation_records_the_tokens_it_saw() {
    let mut session = Session::new(vec![unnamed_failure()], None);
    let recorded = only_record(&mut session, &FAILED_UNNAMED_KINDS);
    assert_eq!(recorded.tokens.input, 10);
    assert_eq!(recorded.tokens.output, 3);
    assert_eq!(recorded.tokens.cache_read, 0);
    assert!(recorded.tokens.cache_write.is_empty());
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
    let recorded = only_record(&mut session, &FAILED_UNNAMED_KINDS);
    assert_eq!(recorded.tokens.input, 10);
    assert_eq!(recorded.cost, None);
}

/// Runs one completed turn after `send`, and returns every line through
/// its `turn_completed`.
fn turned(session: &mut Session, send: impl FnOnce(&Session)) -> Vec<Envelope> {
    use support::delivery;
    send(session);
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.lines()
}

/// The `usage_recorded` lines for `generation`, in log order.
fn copies<'a>(lines: &'a [Envelope], generation: &str) -> Vec<&'a Envelope> {
    lines
        .iter()
        .filter(|l| l.kind == "usage_recorded" && l.payload["generation_id"] == generation)
        .collect()
}

#[test]
fn a_direct_child_s_usage_is_copied_with_the_child_as_origin() {
    let mut session = Session::new(vec![support::reasoning_reply("Think.", "Hi.")], None);
    let lines = turned(&mut session, |session| {
        send_child(
            session,
            "s_child",
            child_usage("g_child", Some(0.01), None, None),
        );
    });
    // The copy is written once, with no turn and no action, naming the
    // sending delegate as the origin.
    let found = copies(&lines, "g_child");
    assert_eq!(found.len(), 1);
    assert!(found[0].action_id.is_none());
    assert!(found[0].turn_id.is_none());
    assert_eq!(found[0].payload["origin_session_id"], "s_child");
    assert_eq!(found[0].payload["cost"], 0.01);
    // The ledger holds the copy beside the turn's own call: 10 input and
    // 3 output tokens each.
    let all = log::read(&session.dir).unwrap();
    let records: Vec<UsageRecorded> = all
        .iter()
        .filter(|l| l.kind == "usage_recorded")
        .map(|l| serde_json::from_value(Value::Object(l.payload.clone())).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(log::usage(&records).tokens.input, 20);
    assert_eq!(log::usage(&records).tokens.output, 6);
    assert_eq!(log::usage(&records).cost, Some(0.01));
}

#[test]
fn a_grandchild_copy_keeps_its_origin() {
    let mut session = Session::new(vec![support::reasoning_reply("Think.", "Hi.")], None);
    let lines = turned(&mut session, |session| {
        send_child(
            session,
            "s_child",
            child_usage(
                "g_grand",
                Some(0.02),
                Some(SessionId("s_grand".into())),
                None,
            ),
        );
    });
    // The middle delegate forwards the line untouched: the copy still
    // names the grandchild that made the call.
    let found = copies(&lines, "g_grand");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].payload["origin_session_id"], "s_grand");
}

#[test]
fn a_duplicate_copy_counts_once() {
    let mut session = Session::new(vec![support::reasoning_reply("Think.", "Hi.")], None);
    turned(&mut session, |session| {
        let line = child_usage("g_child", Some(0.01), None, None);
        send_child(session, "s_child", line.clone());
        send_child(session, "s_child", line);
    });
    // Both arrivals are written, but the spend holds one call: the
    // duplicate never doubles the ledger.
    r#loop::fiber_exited(&session.log, &session.dir, Ok(()), false, None).unwrap();
    let all = log::read(&session.dir).unwrap();
    let written = all
        .iter()
        .filter(|l| l.kind == "usage_recorded" && l.payload["generation_id"] == "g_child")
        .count();
    assert_eq!(written, 2);
    let exited = all.last().unwrap();
    assert_eq!(exited.kind, "fiber_exited");
    assert_eq!(exited.payload["usage"]["tokens"]["input"], 20);
    assert_eq!(exited.payload["usage"]["tokens"]["output"], 6);
    assert_eq!(exited.payload["usage"]["cost"], 0.01);
}

#[test]
fn a_correction_applies_only_its_delta() {
    let mut session = Session::new(vec![support::reasoning_reply("Think.", "Hi.")], None);
    turned(&mut session, |session| {
        send_child(
            session,
            "s_child",
            child_usage("g_child", Some(0.10), None, None),
        );
        send_child(
            session,
            "s_child",
            child_usage("g_child", Some(0.18), None, None),
        );
    });
    // The second arrival replaces the first in every total: the spend
    // holds the latest billed figure once, not both.
    r#loop::fiber_exited(&session.log, &session.dir, Ok(()), false, None).unwrap();
    let all = log::read(&session.dir).unwrap();
    let exited = all.last().unwrap();
    assert_eq!(exited.kind, "fiber_exited");
    assert_eq!(exited.payload["usage"]["cost"], 0.18);
}

#[test]
fn a_subscription_copy_counts_subscription_spend() {
    let mut session = Session::new(vec![support::reasoning_reply("Think.", "Hi.")], None);
    let lines = turned(&mut session, |session| {
        send_child(
            session,
            "s_child",
            child_usage("g_child", Some(0.05), None, Some(true)),
        );
    });
    // The copy keeps the subscription flag, so the spend treats its cost
    // as an estimate, not money billed.
    let found = copies(&lines, "g_child");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].payload["subscription"], true);
    r#loop::fiber_exited(&session.log, &session.dir, Ok(()), false, None).unwrap();
    let all = log::read(&session.dir).unwrap();
    let exited = all.last().unwrap();
    assert_eq!(exited.payload["usage"]["subscription_cost"], 0.05);
}

#[test]
fn a_one_hour_write_costs_twice_input_through_the_session() {
    let mut usage = call_usage("gen_1h");
    usage.tokens.input = 0;
    usage.tokens.output = 0;
    usage.tokens.cache_write.insert("1h".into(), 1_000_000);
    let model = r#loop::Model {
        reference: support::MODEL.into(),
        cost: Some(contract::provider::Cost {
            input: 2.0,
            output: 10.0,
            cache_read: None,
            cache_write: None,
            tiers: Vec::new(),
        }),
        subscription: false,
    };
    let mut session = Session::open(
        vec![Scripted::failed_after(failure(), usage)],
        Vec::new(),
        Vec::new(),
        model,
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
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let recorded = lines.iter().find(|l| l.kind == "usage_recorded").unwrap();
    assert_eq!(recorded.payload["cost"].as_f64(), Some(4.0));
}
