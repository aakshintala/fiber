//! Handoff (`docs/handoff.md`): the automatic trigger, the nudge, the note
//! request and how a handoff is recorded, failed and cancelled. The trigger is
//! 1,000 tokens here, so a scripted reply puts the context exactly at,
//! below or above it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use contract::events::{TextCompleted, TurnOutcome};
use contract::provider::{Finish, Input, ReplyAction};
use contract::shapes::Failure;
use contract::{Envelope, ErrorCode};
use fakes::Scripted;
use r#loop::{HandoffSettings, Retry, rebuild};
use serde_json::json;

use support::{
    MODEL, Session, TestTool, calls_reply, delivery, kinds, reasoning_reply, tool_call_reply,
    with_tokens,
};

/// The trigger in these tests.
const TRIGGER: u64 = 1_000;

fn settings() -> HandoffSettings {
    HandoffSettings {
        tokens: TRIGGER,
        nudge: false,
        ..HandoffSettings::default()
    }
}

fn nudging() -> HandoffSettings {
    HandoffSettings {
        nudge: true,
        ..settings()
    }
}

fn weather() -> Arc<TestTool> {
    // Its result is four bytes: one token.
    Arc::new(TestTool::reads("get_weather", "abcd"))
}

/// A reply calling the weather tool, with `tokens` in its prompt. With the
/// call's one-token result, the context is `tokens + 1` at the next check.
fn called(tokens: u64) -> Scripted {
    with_tokens(tool_call_reply("", &["get_weather"]), tokens, 0)
}

/// A reply of `text`, with `tokens` in its prompt.
fn said(text: &str, tokens: u64) -> Scripted {
    with_tokens(Scripted::text(text), tokens, 0)
}

fn session(script: Vec<Scripted>, settings: HandoffSettings) -> Session {
    Session::with_tools(script, None, vec![weather()])
        .handoff(settings)
        .retry(no_wait())
}

/// A retryable failure retries at once and never parks, one retry.
fn no_wait() -> Retry {
    Retry {
        attempts: 1,
        initial: Duration::ZERO,
        max: Duration::ZERO,
    }
}

fn failed(code: ErrorCode) -> Scripted {
    Scripted::failed(Failure {
        code,
        message: "The call failed.".into(),
        retry_after: None,
        provider: None,
    })
}

fn run(session: &mut Session, prompt: &str) -> (Option<TurnOutcome>, Vec<Envelope>) {
    session.inbox.send(delivery(prompt)).unwrap();
    let outcome = session.turn();
    (outcome, session.lines())
}

const OPENING: &[&str] = &[
    "session_started",
    "preamble_built",
    "opening_message",
    "turn_started",
];
const STEP: &[&str] = &["step_started"];
/// A reply that calls the weather tool, with its streamed fragments, and
/// the call's run.
const CALL_BODY: &[&str] = &[
    "assistant_message_started",
    "assistant_message_delta",
    "tool_call_arguments_delta",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_completed",
];
/// A reply of one text part, streamed as two fragments.
const REPLY: &[&str] = &[
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
];
/// A handoff that completed on a one-part note, to the new opening message.
const HANDED_OFF: &[&str] = &[
    "handoff_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "handoff_completed",
    "opening_message",
];
/// A handoff whose one-text-part note reply did not make a note.
const FAILED_AFTER_TEXT: &[&str] = &[
    "handoff_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "handoff_completed",
];
const ENDED: &[&str] = &["turn_completed"];

/// Asserts the complete, ordered event kinds of `lines`, streamed
/// fragments included: `parts`, concatenated (`docs/testing.md`, "Event
/// streams").
fn assert_kinds(lines: &[Envelope], parts: &[&[&str]]) {
    assert_eq!(kinds(lines), parts.concat());
}

fn of_kind<'a>(lines: &'a [Envelope], kind: &str) -> Vec<&'a Envelope> {
    lines.iter().filter(|line| line.kind == kind).collect()
}

fn user(text: &str) -> Input {
    Input::User { text: text.into() }
}

fn text_of(input: &Input) -> &str {
    match input {
        Input::User { text } | Input::Assistant { text, .. } => text,
        other @ (Input::Reasoning { .. } | Input::ToolCall { .. } | Input::ToolResult { .. }) => {
            panic!("not a message: {other:?}")
        }
    }
}

fn is_opening(input: &Input) -> bool {
    matches!(input, Input::User { text } if text.starts_with("This message is from Fiber"))
}

#[test]
fn a_context_below_the_trigger_does_not_hand_off() {
    let mut session = session(vec![called(100), said("Done.", 100)], settings());

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(&lines, &[OPENING, STEP, CALL_BODY, STEP, REPLY, ENDED]);

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert!(of_kind(&lines, "handoff_started").is_empty());
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn a_context_one_token_short_of_the_trigger_does_not_hand_off() {
    // 998 prompt tokens and a one-token result: 999.
    let mut session = session(vec![called(TRIGGER - 2), said("Done.", 100)], settings());

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(&lines, &[OPENING, STEP, CALL_BODY, STEP, REPLY, ENDED]);

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert!(of_kind(&lines, "handoff_started").is_empty());
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn a_context_exactly_at_the_trigger_hands_off() {
    let forgot = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&forgot);
    let mut session = session(
        vec![
            called(TRIGGER - 1),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        settings(),
    )
    .on_handoff(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, REPLY, ENDED],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let started = of_kind(&lines, "handoff_started");
    assert_eq!(started[0].payload["trigger"], "auto");
    let note_message = of_kind(&lines, "assistant_message_started")[1];
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done.len(), 1);
    assert_eq!(
        serde_json::Value::Object(done[0].payload.clone()),
        json!({
            "outcome": "completed",
            "note": [note_message.action_id.as_ref().unwrap().0],
            "tokens_before": TRIGGER,
        })
    );
    // The note request is a reply like any: its usage is logged.
    assert_eq!(of_kind(&lines, "usage_recorded").len(), 3);
    assert_eq!(forgot.load(Ordering::SeqCst), 1);

    let requests = session.requests();
    assert_eq!(requests.len(), 3);
    // The note request: the conversation so far, then the instruction.
    let asked = &requests[1].conversation;
    assert_eq!(asked.len(), requests[0].conversation.len() + 3);
    assert_eq!(asked[..2], requests[0].conversation[..]);
    assert!(matches!(asked[2], Input::ToolCall { .. }));
    assert!(matches!(asked[3], Input::ToolResult { .. }));
    let instruction = text_of(asked.last().unwrap());
    assert!(instruction.starts_with("Fiber is about to restart your context."));
    assert!(instruction.ends_with("Reply with the note only, and make no tool calls."));
    let opening = of_kind(&lines, "opening_message")[0];
    let log = opening.payload["environment"]["session_log"]
        .as_str()
        .unwrap();
    assert!(instruction.contains(&format!("The session log is at {log}.")));
    assert!(!instruction.contains("focus on"));
    // It reuses the previous request's end, and is never logged.
    assert_eq!(
        requests[1].previous_end,
        Some(requests[0].conversation.len())
    );
    assert!(!lines.iter().any(|line| {
        serde_json::to_string(&line.payload)
            .unwrap()
            .contains("Fiber is about to restart")
    }));
    // The next request starts from the note, and carries no cache marker.
    let next = &requests[2].conversation;
    assert_eq!(next.len(), 3);
    assert!(is_opening(&next[0]));
    assert_eq!(next[1], user("hi"));
    assert_eq!(next[2], user("The note."));
    assert_eq!(requests[2].previous_end, None);
    // The note request has the same preamble, key and tools.
    assert_eq!(requests[1].system_prompt, requests[0].system_prompt);
    assert_eq!(requests[1].tools, requests[0].tools);
    assert_eq!(requests[1].cache_key, requests[0].cache_key);
}

#[test]
fn the_live_conversation_after_a_handoff_is_what_a_rebuild_renders() {
    let mut session = session(
        vec![
            called(TRIGGER - 1),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        settings(),
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, REPLY, ENDED],
    );

    let requests = session.requests();
    let rebuilt = rebuild(&lines, MODEL).unwrap();
    // The request after the handoff, then the reply it got.
    assert_eq!(rebuilt.len(), 4);
    assert_eq!(rebuilt[..3], requests[2].conversation[..]);
    assert_eq!(text_of(&rebuilt[3]), "Done.");
}

#[test]
fn a_note_of_several_text_parts_joins_them_with_a_newline() {
    let mut parts = Scripted::text("");
    let reply = parts.end.as_mut().unwrap();
    for text in ["First.", "Second."] {
        reply.actions.push(ReplyAction::Text(TextCompleted {
            text: text.into(),
            provider_item: None,
        }));
    }
    let mut session = session(
        vec![called(TRIGGER - 1), parts, said("Done.", 50)],
        settings(),
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &[
                "handoff_started",
                "assistant_message_started",
                "text_completed",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "handoff_completed",
                "opening_message",
            ],
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(
        session.requests()[2].conversation[2],
        user("First.\nSecond.")
    );
}

#[test]
fn a_failed_note_request_leaves_the_context_and_blocks_the_rest_of_the_turn() {
    let forgot = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&forgot);
    // Two attempts fail: the retry the policy allows, then the failure.
    let mut session = session(
        vec![
            called(TRIGGER - 1),
            failed(ErrorCode::ProviderUnavailable),
            failed(ErrorCode::ProviderUnavailable),
            called(TRIGGER + 500),
            said("Done.", TRIGGER + 600),
        ],
        settings(),
    )
    .on_handoff(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &[
                "handoff_started",
                "assistant_message_started",
                "assistant_message_completed",
                "retry_scheduled",
                "assistant_message_started",
                "assistant_message_completed",
                "handoff_completed",
            ],
            CALL_BODY,
            STEP,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(of_kind(&lines, "handoff_started").len(), 1);
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["outcome"], "failed");
    assert_eq!(done[0].payload["error"]["code"], "provider_unavailable");
    assert_eq!(done[0].payload["tokens_before"], TRIGGER);
    assert!(done[0].payload.get("note").is_none());
    // The retry went through the normal rules.
    assert_eq!(of_kind(&lines, "retry_scheduled").len(), 1);
    assert_eq!(forgot.load(Ordering::SeqCst), 0);
    let requests = session.requests();
    // call, note, note retry, then the step's own request and the next: the
    // later step is over the trigger and does not hand off.
    assert_eq!(requests.len(), 5);
    let asked = &requests[1].conversation;
    assert_eq!(requests[3].conversation[..], asked[..asked.len() - 1]);
    assert_eq!(requests[3].previous_end, Some(asked.len() - 1));
}

#[test]
fn a_new_turn_may_hand_off_after_a_failed_one() {
    let mut session = session(
        vec![
            called(TRIGGER - 1),
            failed(ErrorCode::InvalidRequest),
            said("Done.", TRIGGER + 200),
            Scripted::text("Second note."),
            said("Again.", 40),
        ],
        settings(),
    );
    let (_, first) = run(&mut session, "hi");
    assert_kinds(
        &first,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &[
                "handoff_started",
                "assistant_message_started",
                "assistant_message_completed",
                "handoff_completed",
            ],
            REPLY,
            ENDED,
        ],
    );
    assert_eq!(
        of_kind(&first, "handoff_completed")[0].payload["outcome"],
        "failed"
    );

    let (outcome, second) = run(&mut session, "again");
    assert_kinds(
        &second,
        &[&["turn_started"], STEP, HANDED_OFF, REPLY, ENDED],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let done = of_kind(&second, "handoff_completed");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["outcome"], "completed");
    // 1,200 from the reply, and the new prompt: five bytes, two tokens.
    assert_eq!(done[0].payload["tokens_before"], TRIGGER + 202);
    let requests = session.requests();
    let carried = &requests.last().unwrap().conversation;
    assert!(is_opening(&carried[0]));
    assert_eq!(carried[1], user("again"));
    assert_eq!(carried[2], user("Second note."));
}

#[test]
fn a_cancel_during_the_note_request_ends_the_turn_interrupted() {
    let forgot = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&forgot);
    // The second call, the note request, is cancelled as it is made.
    let mut session = Session::cancelling_at_call(
        vec![called(TRIGGER - 1), Scripted::text("never read")],
        2,
        vec![weather()],
    )
    .handoff(settings())
    .on_handoff(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &[
                "handoff_started",
                "assistant_message_started",
                "handoff_completed",
            ],
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    let tail = kinds(&lines);
    assert_eq!(
        tail[tail.len() - 4..],
        [
            "handoff_started",
            "assistant_message_started",
            "handoff_completed",
            "turn_completed"
        ]
    );
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(
        serde_json::Value::Object(done[0].payload.clone()),
        json!({"outcome": "cancelled", "tokens_before": TRIGGER})
    );
    assert_eq!(lines.last().unwrap().payload["outcome"], "interrupted");
    // Nothing was sent after the note request, and nothing was forgotten.
    assert_eq!(session.requests().len(), 2);
    assert_eq!(forgot.load(Ordering::SeqCst), 0);
}

#[test]
fn a_note_with_no_text_fails_unreadable_reply() {
    let mut session = session(
        vec![called(TRIGGER - 1), Scripted::text(""), said("Done.", 50)],
        settings(),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &[
                "handoff_started",
                "assistant_message_started",
                "usage_recorded",
                "assistant_message_completed",
                "handoff_completed",
            ],
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done[0].payload["outcome"], "failed");
    assert_eq!(done[0].payload["error"]["code"], "unreadable_reply");
    // The context stays as it was.
    let requests = session.requests();
    let asked = &requests[1].conversation;
    assert_eq!(requests[2].conversation[..], asked[..asked.len() - 1]);
}

#[test]
fn a_whitespace_note_fails_unreadable_reply() {
    let mut session = session(
        vec![
            called(TRIGGER - 1),
            Scripted::text(" \n "),
            said("Done.", 50),
        ],
        settings(),
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            FAILED_AFTER_TEXT,
            REPLY,
            ENDED,
        ],
    );

    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done[0].payload["error"]["code"], "unreadable_reply");
}

#[test]
fn a_note_cut_off_by_the_output_limit_fails_output_truncated() {
    let mut cut = Scripted::text("Half a note");
    cut.end.as_mut().unwrap().finish = Finish::OutputLimit;
    let mut session = session(
        vec![called(TRIGGER - 1), cut, said("Done.", 50)],
        settings(),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            FAILED_AFTER_TEXT,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done[0].payload["outcome"], "failed");
    assert_eq!(done[0].payload["error"]["code"], "output_truncated");
    let requests = session.requests();
    let asked = &requests[1].conversation;
    assert_eq!(requests[2].conversation[..], asked[..asked.len() - 1]);
}

#[test]
fn a_tool_call_in_the_note_reply_never_runs_and_does_not_stop_the_note() {
    let tool = weather();
    let mut session = Session::with_tools(
        vec![
            called(TRIGGER - 1),
            calls_reply("The note.", &[("get_weather", json!({"city": "Rome"}))]),
            said("Done.", 50),
        ],
        None,
        vec![tool.clone()],
    )
    .handoff(settings());

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &[
                "handoff_started",
                "assistant_message_started",
                "assistant_message_delta",
                "tool_call_arguments_delta",
                "text_completed",
                "tool_call_requested",
                "usage_recorded",
                "assistant_message_completed",
                "tool_call_completed",
                "handoff_completed",
                "opening_message",
            ],
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    // Only the first step's call ran.
    assert_eq!(tool.ran().len(), 1);
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done[0].payload["outcome"], "completed");
    let completions = of_kind(&lines, "tool_call_completed");
    assert_eq!(completions.len(), 2);
    assert_eq!(completions[1].payload["status"], "cancelled");
    // The call and its result are gone with the rest of the window.
    let next = &session.requests()[2].conversation;
    assert_eq!(next.len(), 3);
    assert_eq!(next[2], user("The note."));
}

#[test]
fn an_exhausted_budget_fails_the_handoff_and_then_the_turn() {
    let model = r#loop::Model {
        reference: MODEL.into(),
        cost: Some(contract::provider::Cost {
            input: 1.0,
            output: 0.0,
            cache_read: None,
            cache_write: None,
            tiers: Vec::new(),
        }),
        subscription: false,
    };
    // 999 tokens at one dollar per million is under a cent; the budget is
    // smaller still.
    let mut session = Session::open(
        vec![called(TRIGGER - 1), Scripted::text("never sent")],
        Vec::new(),
        vec![weather()],
        model,
    )
    .handoff(settings())
    .budget(Some(0.0005));

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &["handoff_started", "handoff_completed"],
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Failed));
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done[0].payload["outcome"], "failed");
    assert_eq!(done[0].payload["error"]["code"], "budget_exceeded");
    let ended = of_kind(&lines, "turn_completed");
    assert_eq!(ended[0].payload["error"]["code"], "budget_exceeded");
    // No note request was sent.
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn automatic_handoff_off_never_hands_off() {
    let mut session = session(
        vec![called(TRIGGER * 5), said("Done.", TRIGGER * 5)],
        HandoffSettings {
            enabled: false,
            nudge: true,
            ..settings()
        },
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(&lines, &[OPENING, STEP, CALL_BODY, STEP, REPLY, ENDED]);

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert!(of_kind(&lines, "handoff_started").is_empty());
    assert!(of_kind(&lines, "context_nudged").is_empty());
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn a_fresh_context_is_unmeasured_until_a_reply_measures_it() {
    // A trigger of one token is passed by any context. Before the first
    // reply nothing has measured the context, so the first request is sent;
    // after the handoff the new context is unmeasured again, so the request
    // from the note is sent too, and the turn does not hand off twice.
    let mut session = session(
        vec![called(5), Scripted::text("The note."), said("Done.", 5)],
        HandoffSettings {
            tokens: 1,
            ..settings()
        },
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, REPLY, ENDED],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(of_kind(&lines, "handoff_started").len(), 1);
    assert_eq!(session.requests().len(), 3);
}

#[test]
fn the_nudge_is_given_at_two_thirds_of_the_trigger() {
    // 666 prompt tokens and a one-token result: 667, and 667 x 3 passes
    // 1,000 x 2.
    let mut session = session(
        vec![called(666), called(900), said("Done.", 950)],
        nudging(),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &["context_nudged"],
            CALL_BODY,
            STEP,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let nudged = of_kind(&lines, "context_nudged");
    // Once per context: the second step is over it too.
    assert_eq!(nudged.len(), 1);
    assert_eq!(
        serde_json::Value::Object(nudged[0].payload.clone()),
        json!({"tokens": 667, "trigger_at": TRIGGER})
    );
    let requests = session.requests();
    let last = requests[1].conversation.last().unwrap();
    assert!(
        text_of(last).starts_with("Fiber: your context holds 667 tokens. At 1000 tokens"),
        "{last:?}"
    );
    // The nudge stays in the conversation the next request sends.
    assert_eq!(requests[2].conversation[..5], requests[1].conversation[..]);
}

#[test]
fn a_context_a_token_below_two_thirds_is_not_nudged() {
    // 665 and a one-token result: 666, and 666 x 3 is short of 2,000.
    let mut session = session(vec![called(665), said("Done.", 100)], nudging());

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(&lines, &[OPENING, STEP, CALL_BODY, STEP, REPLY, ENDED]);

    assert!(of_kind(&lines, "context_nudged").is_empty());
}

#[test]
fn the_nudge_can_be_turned_off() {
    let mut session = session(vec![called(900), said("Done.", 100)], settings());

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(&lines, &[OPENING, STEP, CALL_BODY, STEP, REPLY, ENDED]);

    assert!(of_kind(&lines, "context_nudged").is_empty());
}

#[test]
fn each_handoff_starts_a_context_that_can_be_nudged_again() {
    let mut session = session(
        vec![
            called(700),
            called(TRIGGER - 1),
            Scripted::text("The note."),
            called(700),
            said("Done.", 100),
        ],
        nudging(),
    );

    let (outcome, lines) = run(&mut session, "hi");

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    // One nudge before the handoff, and none at it: the handoff took its
    // place. One in the new context.
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &["context_nudged"],
            CALL_BODY,
            STEP,
            HANDED_OFF,
            CALL_BODY,
            STEP,
            &["context_nudged"],
            REPLY,
            ENDED,
        ],
    );
}

#[test]
fn the_nudge_fires_when_three_times_the_size_equals_twice_the_trigger() {
    // 665 prompt tokens and a one-token result: 666, and 666 x 3 is 1,998,
    // exactly 999 x 2. One token less is 665 x 3 = 1,995: short.
    let at = |prompt: u64, expected: &[&[&str]]| {
        let mut session = session(
            vec![called(prompt), said("Done.", 100)],
            HandoffSettings {
                tokens: 999,
                ..nudging()
            },
        );
        let (_, lines) = run(&mut session, "hi");
        assert_kinds(&lines, expected);
        of_kind(&lines, "context_nudged").len()
    };
    let nudged: &[&[&str]] = &[
        OPENING,
        STEP,
        CALL_BODY,
        STEP,
        &["context_nudged"],
        REPLY,
        ENDED,
    ];
    let quiet: &[&[&str]] = &[OPENING, STEP, CALL_BODY, STEP, REPLY, ENDED];
    assert_eq!(at(665, nudged), 1);
    assert_eq!(at(664, quiet), 0);
}

#[test]
fn a_new_context_is_unmeasured_in_the_next_turn_too() {
    // The request after the handoff fails, so no reply measures the new
    // context. The next turn must not read the old context's size.
    let mut session = session(
        vec![
            called(TRIGGER + 400),
            Scripted::text("The note."),
            failed(ErrorCode::InvalidRequest),
            said("Fine.", 20),
        ],
        settings(),
    );
    let (first, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            HANDED_OFF,
            &["assistant_message_started", "assistant_message_completed"],
            ENDED,
        ],
    );
    assert_eq!(first, Some(TurnOutcome::Failed));
    assert_eq!(
        of_kind(&lines, "handoff_completed")[0].payload["outcome"],
        "completed"
    );

    let (second, lines) = run(&mut session, "again");
    assert_kinds(&lines, &[&["turn_started"], STEP, REPLY, ENDED]);

    assert_eq!(second, Some(TurnOutcome::Completed));
    assert!(of_kind(&lines, "handoff_started").is_empty());
}

#[test]
fn a_notes_reasoning_is_not_part_of_the_note() {
    let mut session = session(
        vec![
            called(TRIGGER - 1),
            reasoning_reply("Thinking it over.", "The note."),
            said("Done.", 50),
        ],
        settings(),
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &[
                "handoff_started",
                "assistant_message_started",
                "reasoning_started",
                "reasoning_delta",
                "assistant_message_delta",
                "reasoning_completed",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "handoff_completed",
                "opening_message",
            ],
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(session.requests()[2].conversation[2], user("The note."));
}
