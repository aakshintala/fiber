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
use std::sync::mpsc;
use std::time::Duration;

use contract::ThinkingLevel;
use contract::events::{CacheLifetime, Control, TextCompleted, TurnOutcome};
use contract::inbox::{Ack, Answer, Delivery, Rejection};
use contract::provider::{Finish, Input, Provider, ReplyAction};
use contract::shapes::Failure;
use contract::{Envelope, ErrorCode};
use fakes::{Scripted, ScriptedProvider};
use r#loop::{HandoffSettings, Hosted, Prepare, Prepared, Retry, Switchable, rebuild};
use serde_json::json;

use support::{
    DEADLINE, ENDED, MODEL, OPENING, REPLY, STEP, Session, TestTool, assert_kinds,
    assert_no_stored_attempt, attempt_numbers, calls_reply, delivery, handoff, ignore, kinds,
    model, reasoning_reply, tool_call_reply, with_tokens,
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
        retry_after_ms: None,
        provider: None,
    })
}

fn run(session: &mut Session, prompt: &str) -> (Option<TurnOutcome>, Vec<Envelope>) {
    session.inbox.send(delivery(prompt)).unwrap();
    let outcome = session.turn();
    (outcome, session.lines())
}

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
fn of_kind<'a>(lines: &'a [Envelope], kind: &str) -> Vec<&'a Envelope> {
    lines.iter().filter(|line| line.kind == kind).collect()
}

fn user(text: &str) -> Input {
    Input::User {
        text: text.into(),
        images: Vec::new(),
    }
}

fn text_of(input: &Input) -> &str {
    match input {
        Input::User { text, .. } | Input::Assistant { text, .. } => text,
        other @ (Input::Reasoning { .. } | Input::ToolCall { .. } | Input::ToolResult { .. }) => {
            panic!("not a message: {other:?}")
        }
    }
}

fn is_opening(input: &Input) -> bool {
    matches!(input, Input::User { text , ..} if text.starts_with("This message is from Fiber"))
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
                "usage_recorded",
                "assistant_message_completed",
                "retry_scheduled",
                "assistant_message_started",
                "usage_recorded",
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
                "usage_recorded",
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
                "usage_recorded",
                "handoff_completed",
            ],
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    let tail = kinds(&lines);
    assert_eq!(
        tail[tail.len() - 5..],
        [
            "handoff_started",
            "assistant_message_started",
            "usage_recorded",
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
fn a_budget_refused_note_stops_nothing_and_the_turn_end_stops_once() {
    // As `an_exhausted_budget_fails_the_handoff_and_then_the_turn`, with
    // the session's jobs attached: the refused note must not stop
    // anything, and the turn's next `send` stops the delegates exactly
    // once through the one budget-end function.
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
    let mut session = Session::open(
        vec![called(TRIGGER - 1), Scripted::text("never sent")],
        Vec::new(),
        vec![weather()],
        model,
    )
    .handoff(settings())
    .budget(Some(0.0005));
    let jobs = fakes::jobs::FakeJobs::new(&session.workspace);
    let looped = session.looped.take().unwrap();
    session.looped = Some(looped.jobs(Arc::clone(&jobs) as Arc<dyn contract::jobs::Jobs>));
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Failed));
    let ended = of_kind(&lines, "turn_completed");
    assert_eq!(ended[0].payload["error"]["code"], "budget_exceeded");
    assert_eq!(jobs.stop_delegates_calls(), 1);
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
        &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, REJECTED, ENDED],
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

// A person's handoff (`docs/handoff.md`, "A person").

/// The `handoff-note` text, to its last line.
const NOTE_REQUEST_END: &str = "Reply with the note only, and make no tool calls.";

/// The text of the note request, the last input of `request`.
fn note_request(request: &contract::provider::ModelRequest) -> &str {
    text_of(request.conversation.last().unwrap())
}

/// An acknowledgement that sends its answer on a channel.
fn answered() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (Ack(Box::new(move |answer| tx.send(answer).unwrap())), rx)
}

/// A session whose first model call sends `during`, with the weather tool.
fn injected(script: Vec<Scripted>, during: Vec<Delivery>, settings: HandoffSettings) -> Session {
    Session::with_tools_injecting(script, during, vec![weather()])
        .handoff(settings)
        .retry(no_wait())
}

/// The script of one handoff after a tool step: the call, the note, the
/// answer.
fn handed_after_a_call() -> Vec<Scripted> {
    vec![called(100), Scripted::text("The note."), said("Done.", 50)]
}

#[test]
fn a_person_handoff_during_a_turn_runs_at_the_next_step_boundary() {
    let (ack, answer) = answered();
    let mut session = injected(
        handed_after_a_call(),
        vec![Delivery::Handoff(
            contract::CommandId("c_h".into()),
            contract::commands::Handoff {
                instructions: Some("focus on tests".into()),
            },
            ack,
        )],
        settings(),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, REPLY, ENDED],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    // Accepted when taken.
    assert!(answer.recv_timeout(DEADLINE).unwrap().unwrap().is_none());
    assert_eq!(
        of_kind(&lines, "handoff_started")[0].payload["trigger"],
        "person"
    );
    let note_message = of_kind(&lines, "assistant_message_started")[1];
    assert_eq!(
        serde_json::Value::Object(of_kind(&lines, "handoff_completed")[0].payload.clone()),
        json!({
            "outcome": "completed",
            "note": [note_message.action_id.as_ref().unwrap().0],
            "tokens_before": 101,
            "instructions": "focus on tests",
        })
    );
    let requests = session.requests();
    assert_eq!(requests.len(), 3);
    let asked = note_request(&requests[1]);
    assert!(asked.starts_with("Fiber is about to restart your context."));
    assert!(
        asked.ends_with(&format!(
            "{NOTE_REQUEST_END}\n\nThe person asked that the next stretch of work focus on:\n\nfocus on tests"
        )),
        "{asked}"
    );
    // The request after the handoff starts from the note.
    let next = &requests[2].conversation;
    assert_eq!(next.len(), 3);
    assert_eq!(next[1], user("hi"));
    assert_eq!(next[2], user("The note."));
    assert_eq!(requests[2].previous_end, None);
    // No line is written for the instruction, or for the command.
    assert!(of_kind(&lines, "steering_queue").is_empty());
}

#[test]
fn a_person_handoff_runs_with_automatic_handoff_off() {
    let mut session = injected(
        handed_after_a_call(),
        vec![handoff("c_h", None)],
        HandoffSettings {
            enabled: false,
            ..settings()
        },
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, REPLY, ENDED],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(
        of_kind(&lines, "handoff_started")[0].payload["trigger"],
        "person"
    );
}

#[test]
fn commands_before_one_boundary_make_one_handoff_with_their_instructions_joined() {
    let mut session = injected(
        handed_after_a_call(),
        vec![
            handoff("c_1", Some("first")),
            handoff("c_2", None),
            handoff("c_3", Some("")),
            handoff("c_4", Some("second")),
        ],
        settings(),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, REPLY, ENDED],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(of_kind(&lines, "handoff_started").len(), 1);
    assert_eq!(
        of_kind(&lines, "handoff_completed")[0].payload["instructions"],
        "first\n\nsecond"
    );
    let requests = session.requests();
    assert!(
        note_request(&requests[1]).ends_with("focus on:\n\nfirst\n\nsecond"),
        "{}",
        note_request(&requests[1])
    );
}

#[test]
fn a_handoff_with_no_instructions_asks_for_the_note_only() {
    let mut session = injected(
        handed_after_a_call(),
        vec![handoff("c_1", None), handoff("c_2", Some(""))],
        settings(),
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, REPLY, ENDED],
    );

    assert!(
        !of_kind(&lines, "handoff_completed")[0]
            .payload
            .contains_key("instructions")
    );
    let requests = session.requests();
    let asked = note_request(&requests[1]);
    assert!(asked.starts_with("Fiber is about to restart your context."));
    assert!(asked.ends_with(NOTE_REQUEST_END), "{asked}");
    assert!(!asked.contains("The person asked"));
}

#[test]
fn a_handoff_held_at_the_end_of_the_turn_continues_the_turn() {
    // The first reply calls no tool, and the command arrived while it was
    // being written: the end-of-turn check finds it.
    let mut session = injected(
        vec![
            said("Hello.", 100),
            Scripted::text("The note."),
            said("More.", 50),
        ],
        vec![handoff("c_h", Some("go on"))],
        settings(),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, REPLY, STEP, HANDED_OFF, REPLY, ENDED],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let requests = session.requests();
    assert_eq!(requests.len(), 3);
    // The note request holds the reply the turn had just ended on.
    assert_eq!(text_of(&requests[1].conversation[2]), "Hello.");
    assert_eq!(requests[2].conversation[1], user("hi"));
    assert_eq!(requests[2].conversation[2], user("The note."));
}

#[test]
fn a_person_handoff_on_the_automatic_boundary_runs_once_as_the_persons() {
    let mut session = injected(
        vec![
            called(TRIGGER),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        vec![handoff("c_h", None)],
        settings(),
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, REPLY, ENDED],
    );

    assert_eq!(
        of_kind(&lines, "handoff_started")[0].payload["trigger"],
        "person"
    );
}

#[test]
fn a_steer_drop_cannot_drop_a_handoff() {
    let (ack, answer) = answered();
    let mut session = injected(
        handed_after_a_call(),
        vec![
            handoff("c_h", None),
            Delivery::SteerDrop(contract::CommandId("c_h".into()), ack),
        ],
        settings(),
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, REPLY, ENDED],
    );

    let rejected = answer.recv_timeout(DEADLINE).unwrap().unwrap_err();
    assert_eq!(rejected.code, ErrorCode::StaleRequest);
}

#[test]
fn a_steer_drop_between_turns_cannot_drop_a_handoff_either() {
    let (ack, answer) = answered();
    let mut session = session(vec![Scripted::text("The note.")], settings());
    session.inbox.send(handoff("c_h", None)).unwrap();
    session
        .inbox
        .send(Delivery::SteerDrop(contract::CommandId("c_h".into()), ack))
        .unwrap();

    let outcome = session.turn();
    let lines = session.lines();
    assert_kinds(&lines, &[OPENING, STEP, HANDED_OFF, ENDED]);

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let rejected = answer.recv_timeout(DEADLINE).unwrap().unwrap_err();
    assert_eq!(rejected.code, ErrorCode::StaleRequest);
}

#[test]
fn a_person_handoff_between_turns_is_a_turn_of_its_own() {
    let (ack, answer) = answered();
    let mut session = session(
        vec![Scripted::text("The note."), said("Next.", 50)],
        settings(),
    );
    session
        .inbox
        .send(Delivery::Handoff(
            contract::CommandId("c_h".into()),
            contract::commands::Handoff {
                instructions: Some("focus".into()),
            },
            ack,
        ))
        .unwrap();

    let outcome = session.turn();
    let lines = session.lines();
    assert_kinds(&lines, &[OPENING, STEP, HANDED_OFF, ENDED]);

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert!(answer.recv_timeout(DEADLINE).unwrap().unwrap().is_none());
    assert_eq!(
        serde_json::Value::Object(of_kind(&lines, "turn_started")[0].payload.clone()),
        json!({"input": [{"type": "handoff", "command_id": "c_h"}]})
    );
    assert_eq!(
        of_kind(&lines, "handoff_started")[0].payload["trigger"],
        "person"
    );
    // No request after the note.
    let requests = session.requests();
    assert_eq!(requests.len(), 1);
    // The opening message was all there was to hand off, and no reply
    // measured it: its size is estimated at a token to four bytes.
    let opening = text_of(&requests[0].conversation[0]);
    assert_eq!(
        of_kind(&lines, "handoff_completed")[0].payload["tokens_before"],
        opening.len().div_ceil(4)
    );
    assert_eq!(requests[0].conversation.len(), 2);
    assert!(is_opening(&requests[0].conversation[0]));
    assert!(note_request(&requests[0]).ends_with("focus on:\n\nfocus"));

    // The next turn starts from the note.
    let (outcome, second) = run(&mut session, "next");
    assert_kinds(&second, &[&["turn_started"], STEP, REPLY, ENDED]);
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let requests = session.requests();
    let next = &requests[1].conversation;
    assert_eq!(next.len(), 3);
    assert!(is_opening(&next[0]));
    assert_eq!(next[1], user("The note."));
    assert_eq!(next[2], user("next"));
    // The live conversation is what a rebuild renders.
    let rebuilt = rebuild(&[lines, second].concat(), MODEL).unwrap();
    assert_eq!(rebuilt[..3], next[..]);
}

#[test]
fn a_prompt_in_the_drain_of_a_handoff_is_carried_and_answered_after_it() {
    for prompt_first in [false, true] {
        let (prompt_ack, prompt_answer) = answered();
        let mut session = session(
            vec![Scripted::text("The note."), said("Answer.", 50)],
            settings(),
        );
        let prompt = Delivery::Prompt(support::message("hi"), prompt_ack);
        let command = handoff("c_h", None);
        for delivery in if prompt_first {
            [prompt, command]
        } else {
            [command, prompt]
        } {
            session.inbox.send(delivery).unwrap();
        }

        let outcome = session.turn();
        let lines = session.lines();
        assert_kinds(&lines, &[OPENING, STEP, HANDED_OFF, REPLY, ENDED]);

        assert_eq!(outcome, Some(TurnOutcome::Completed));
        assert!(prompt_answer.recv_timeout(DEADLINE).unwrap().is_ok());
        let input = &of_kind(&lines, "turn_started")[0].payload["input"];
        let types: Vec<&str> = input
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            if prompt_first {
                ["message", "handoff"]
            } else {
                ["handoff", "message"]
            }
        );
        let requests = session.requests();
        assert_eq!(requests.len(), 2);
        // The note request holds the prompt, and the answer is asked from
        // the note after it.
        assert_eq!(requests[0].conversation[1], user("hi"));
        assert_eq!(requests[1].conversation[1], user("hi"));
        assert_eq!(requests[1].conversation[2], user("The note."));
    }
}

#[test]
fn a_failed_handoff_between_turns_still_ends_the_turn_without_a_request() {
    let mut session = session(
        vec![failed(ErrorCode::InvalidRequest), said("Never.", 1)],
        settings(),
    );
    session.inbox.send(handoff("c_h", None)).unwrap();

    let outcome = session.turn();
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            &[
                "handoff_started",
                "assistant_message_started",
                "usage_recorded",
                "assistant_message_completed",
                "handoff_completed",
            ],
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(
        of_kind(&lines, "handoff_completed")[0].payload["outcome"],
        "failed"
    );
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn a_turn_a_job_started_goes_on_after_a_handoff() {
    // The turn's input holds no message, but it did not begin as a handoff.
    let notice = Delivery::Job(contract::inbox::JobNotice {
        completed: contract::events::JobCompleted {
            job_id: contract::JobId("j_1".into()),
            status: contract::events::Outcome::Completed,
            error: None,
            process: None,
            output_tail: None,
        },
        claim: contract::inbox::Claim(Box::new(|| true)),
        delegate: None,
    });
    let mut session = injected(
        handed_after_a_call(),
        vec![handoff("c_h", None)],
        settings(),
    );
    session.inbox.send(notice).unwrap();

    let outcome = session.turn();
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            &[
                "session_started",
                "preamble_built",
                "opening_message",
                "turn_started",
            ],
            STEP,
            &["job_completed"],
            CALL_BODY,
            STEP,
            HANDED_OFF,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 3);
}

#[test]
fn a_handoff_is_refused_closing_once_close_was_taken_with_nothing_to_start() {
    let (ack, answer) = answered();
    let mut session = session(vec![said("Never.", 1)], settings());
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    session
        .inbox
        .send(Delivery::Handoff(
            contract::CommandId("c_h".into()),
            contract::commands::Handoff { instructions: None },
            ack,
        ))
        .unwrap();

    assert_eq!(session.turn(), None);

    let rejected = answer.recv_timeout(DEADLINE).unwrap().unwrap_err();
    assert_eq!(rejected.code, ErrorCode::Closing);
    assert!(session.requests().is_empty());
}

#[test]
fn a_handoff_after_close_joins_a_turn_that_already_has_input() {
    let (ack, answer) = answered();
    let mut session = session(
        vec![Scripted::text("The note."), said("Done.", 50)],
        settings(),
    );
    session.inbox.send(delivery("hi")).unwrap();
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    session
        .inbox
        .send(Delivery::Handoff(
            contract::CommandId("c_h".into()),
            contract::commands::Handoff { instructions: None },
            ack,
        ))
        .unwrap();

    let outcome = session.turn();
    let lines = session.lines();
    assert_kinds(&lines, &[OPENING, STEP, HANDED_OFF, REPLY, ENDED]);

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert!(answer.recv_timeout(DEADLINE).unwrap().is_ok());
}

// A tool's handoff (`docs/handoff.md`, "A tool").

/// A tool that returns no content and `control.handoff` of `note`.
fn wrapping(name: &'static str, note: &str) -> Arc<TestTool> {
    let mut tool = TestTool::reads(name, "");
    tool.output.content = Vec::new();
    tool.output.control = Some(Control {
        handoff: Some(note.into()),
        ..Default::default()
    });
    Arc::new(tool)
}

/// A reply of `names` called, each with its own arguments, with `tokens` in
/// its prompt.
fn calling(names: &[&str], tokens: u64) -> Scripted {
    let calls: Vec<(&str, serde_json::Value)> = names
        .iter()
        .enumerate()
        .map(|(index, name)| (*name, json!({"city": format!("city {index}")})))
        .collect();
    with_tokens(calls_reply("", &calls), tokens, 0)
}

fn tool_session(script: Vec<Scripted>, tools: Vec<Arc<TestTool>>) -> Session {
    Session::with_tools(
        script,
        None,
        tools
            .into_iter()
            .map(|tool| tool as Arc<dyn contract::tool::Tool>)
            .collect(),
    )
    .handoff(settings())
    .retry(no_wait())
}

/// A reply of calls with no sibling: the calls' lines, their starts and
/// completions.
fn call_lines(count: usize) -> Vec<&'static str> {
    let mut kinds = vec!["assistant_message_started", "assistant_message_delta"];
    kinds.extend(std::iter::repeat_n("tool_call_arguments_delta", count));
    kinds.extend(std::iter::repeat_n("tool_call_requested", count));
    kinds.extend(["usage_recorded", "assistant_message_completed"]);
    kinds.extend(std::iter::repeat_n("tool_call_started", count));
    kinds.extend(std::iter::repeat_n("tool_call_completed", count));
    kinds
}

#[test]
fn a_call_that_sets_control_handoff_restarts_the_context_from_its_note() {
    let mut session = tool_session(
        vec![calling(&["wrapup"], 500), said("Done.", 50)],
        vec![wrapping("wrapup", "Tool note.")],
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            &call_lines(1),
            &["handoff_completed", "opening_message"],
            STEP,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let call = of_kind(&lines, "tool_call_requested")[0];
    assert_eq!(
        serde_json::Value::Object(of_kind(&lines, "handoff_completed")[0].payload.clone()),
        json!({
            "outcome": "completed",
            "note": [call.action_id.as_ref().unwrap().0],
            "tokens_before": 500,
        })
    );
    // No note request: the tool's argument was the note.
    assert!(of_kind(&lines, "handoff_started").is_empty());
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    let next = &requests[1].conversation;
    assert_eq!(next.len(), 3);
    assert!(is_opening(&next[0]));
    assert_eq!(next[1], user("hi"));
    assert_eq!(next[2], user("Tool note."));
    assert_eq!(requests[1].previous_end, None);
}

#[test]
fn the_other_calls_of_the_step_follow_the_note_in_call_order() {
    let mut session = tool_session(
        vec![
            calling(&["get_weather", "wrapup", "get_weather"], 500),
            said("Done.", 50),
        ],
        vec![weather(), wrapping("wrapup", "Tool note.")],
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            &call_lines(3),
            &["handoff_completed", "opening_message"],
            STEP,
            REPLY,
            ENDED,
        ],
    );

    let ids: Vec<&str> = of_kind(&lines, "tool_call_requested")
        .iter()
        .map(|line| line.action_id.as_ref().unwrap().0.as_str())
        .collect();
    assert_eq!(
        of_kind(&lines, "handoff_completed")[0].payload["note"],
        json!([ids[1]])
    );
    // The prompt tokens, and the two one-token results written after the
    // reply that measured them.
    assert_eq!(
        of_kind(&lines, "handoff_completed")[0].payload["tokens_before"],
        502
    );
    let requests = session.requests();
    let next = &requests[1].conversation;
    assert_eq!(next.len(), 7);
    assert_eq!(next[2], user("Tool note."));
    // The calls in call order, then their results in the same order.
    let called: Vec<&str> = next[3..5]
        .iter()
        .map(|input| {
            let Input::ToolCall { action_id, .. } = input else {
                panic!("a call: {input:?}");
            };
            action_id.0.as_str()
        })
        .collect();
    assert_eq!(called, [ids[0], ids[2]]);
    let results: Vec<(&str, &str)> = next[5..]
        .iter()
        .map(|input| {
            let Input::ToolResult {
                action_id, text, ..
            } = input
            else {
                panic!("a result: {input:?}");
            };
            (action_id.0.as_str(), text.as_str())
        })
        .collect();
    assert_eq!(results, [(ids[0], "abcd"), (ids[2], "abcd")]);
    // The live conversation is what a rebuild renders.
    let rebuilt = rebuild(&lines, MODEL).unwrap();
    assert_eq!(rebuilt[..7], next[..]);
}

#[test]
fn two_calls_that_set_control_handoff_join_their_notes_in_call_order() {
    // Call order, not name order.
    let mut session = tool_session(
        vec![
            calling(&["wrap_b", "get_weather", "wrap_a"], 500),
            said("Done.", 50),
        ],
        vec![
            wrapping("wrap_a", "A note."),
            wrapping("wrap_b", "B note."),
            weather(),
        ],
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            &call_lines(3),
            &["handoff_completed", "opening_message"],
            STEP,
            REPLY,
            ENDED,
        ],
    );

    let ids: Vec<&str> = of_kind(&lines, "tool_call_requested")
        .iter()
        .map(|line| line.action_id.as_ref().unwrap().0.as_str())
        .collect();
    assert_eq!(
        of_kind(&lines, "handoff_completed")[0].payload["note"],
        json!([ids[0], ids[2]])
    );
    let next = &session.requests()[1].conversation;
    assert_eq!(next[2], user("B note.\n\nA note."));
    // Only the weather call is left to follow the note.
    assert_eq!(next.len(), 5);
    assert!(matches!(&next[3], Input::ToolCall { action_id, .. } if action_id.0 == ids[1]));
    assert!(matches!(&next[4], Input::ToolResult { action_id, .. } if action_id.0 == ids[1]));
    assert_eq!(rebuild(&lines, MODEL).unwrap()[..5], next[..]);
}

#[test]
fn the_loop_acts_on_the_field_and_never_on_the_tool_name() {
    // A tool named `handoff` that sets nothing does not hand off.
    let mut session = tool_session(
        vec![calling(&["handoff"], 500), said("Done.", 50)],
        vec![Arc::new(TestTool::reads("handoff", "Not a note."))],
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(&lines, &[OPENING, STEP, &call_lines(1), STEP, REPLY, ENDED]);

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert!(of_kind(&lines, "handoff_completed").is_empty());
}

#[test]
fn a_call_that_failed_does_not_hand_off_whatever_it_set() {
    let mut tool = TestTool::failing("wrapup", ErrorCode::ToolError);
    tool.output.control = Some(Control {
        handoff: Some("Never.".into()),
        ..Default::default()
    });
    let mut session = tool_session(
        vec![calling(&["wrapup"], 500), said("Done.", 50)],
        vec![Arc::new(tool)],
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(&lines, &[OPENING, STEP, &call_lines(1), STEP, REPLY, ENDED]);

    assert!(of_kind(&lines, "handoff_completed").is_empty());
}

#[test]
fn a_held_person_handoff_still_runs_at_the_boundary_after_a_tool_handoff() {
    let mut session = Session::with_tools_injecting(
        vec![
            calling(&["wrapup"], 500),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        vec![handoff("c_h", Some("focus"))],
        vec![wrapping("wrapup", "Tool note.")],
    )
    .handoff(settings())
    .retry(no_wait());

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            &call_lines(1),
            &["handoff_completed", "opening_message"],
            STEP,
            HANDED_OFF,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    // The person's handoff asked for its note from the context the tool's
    // handoff made.
    let requests = session.requests();
    assert_eq!(requests[1].conversation[2], user("Tool note."));
    assert_eq!(requests[2].conversation[2], user("The note."));
}

// Overflow (`docs/handoff.md`, "Overflow").

/// A request the provider rejected for size: one assistant message, failed.
const REJECTED: &[&str] = &[
    "assistant_message_started",
    "usage_recorded",
    "assistant_message_completed",
];
/// A handoff whose note request failed.
const FAILED_NOTE: &[&str] = &[
    "handoff_started",
    "assistant_message_started",
    "usage_recorded",
    "assistant_message_completed",
    "handoff_completed",
];
/// A reply that calls two tools, with their runs.
const TWO_CALLS: &[&str] = &[
    "assistant_message_started",
    "assistant_message_delta",
    "tool_call_arguments_delta",
    "tool_call_arguments_delta",
    "tool_call_requested",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_started",
    "tool_call_completed",
    "tool_call_completed",
];
const MOVED: &str = "Fiber: this result was moved out of your context. Its full text is at ";
/// What the `long` tool returns, and the first four bytes of it its bound
/// keeps.
const LONG: &str = "0123456789";

/// A tool whose result is cut to four bytes, so it has an artifact.
fn long() -> Arc<TestTool> {
    let mut tool = TestTool::reads("long", LONG);
    tool.bound = contract::tool::Bound { start: 4, end: 0 };
    Arc::new(tool)
}

fn overflowing() -> Scripted {
    failed(ErrorCode::ContextOverflow)
}

/// A reply calling the weather tool and the long tool, with `tokens` in its
/// prompt.
fn called_both(tokens: u64) -> Scripted {
    with_tokens(tool_call_reply("", &["get_weather", "long"]), tokens, 0)
}

fn both_tools() -> Vec<Arc<dyn contract::tool::Tool>> {
    vec![weather(), long()]
}

fn error_of(line: &Envelope) -> &str {
    line.payload["error"]["code"].as_str().unwrap()
}

#[test]
fn a_provider_overflow_moves_the_last_steps_results_and_hands_off() {
    let mut session = Session::with_tools(
        vec![
            called_both(100),
            overflowing(),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        None,
        both_tools(),
    )
    .handoff(settings())
    .retry(no_wait());

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING, STEP, TWO_CALLS, STEP, REJECTED, HANDED_OFF, REPLY, ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(
        of_kind(&lines, "handoff_started")[0].payload["trigger"],
        "overflow"
    );
    let completions = of_kind(&lines, "tool_call_completed");
    let weather_id = completions[0].action_id.as_ref().unwrap().0.clone();
    let long_id = completions[1].action_id.as_ref().unwrap().0.clone();
    let cut = completions[1].payload["artifact"].as_str().unwrap();
    assert!(completions[0].payload.get("artifact").is_none());

    let session_dir = std::fs::canonicalize(&session.dir).unwrap();
    let requests = session.requests();
    assert_eq!(requests.len(), 4);
    // The note request: the unchanged conversation, with each result moved.
    let asked = &requests[2].conversation;
    assert_eq!(asked.len(), requests[1].conversation.len() + 1);
    let results: Vec<&str> = asked
        .iter()
        .filter_map(|input| match input {
            Input::ToolResult { text, .. } => Some(text.as_str()),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. } => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    let written = format!("artifacts/{weather_id}.txt");
    assert_eq!(
        results[0],
        format!("{MOVED}{}.", session_dir.join(&written).display())
    );
    assert_eq!(
        results[1],
        format!("{MOVED}{}.", session_dir.join(cut).display())
    );
    assert_eq!(cut, format!("artifacts/{long_id}.txt"));
    // A result without an artifact now has one holding its text; one with an
    // artifact is not rewritten.
    assert_eq!(
        std::fs::read_to_string(session_dir.join(&written)).unwrap(),
        "abcd"
    );
    assert_eq!(
        std::fs::read_to_string(session_dir.join(cut)).unwrap(),
        LONG
    );
    // The replacement is only in the note request: the log keeps the result.
    assert!(!lines.iter().any(|line| {
        serde_json::to_string(&line.payload)
            .unwrap()
            .contains("moved out of your context")
    }));
    // The step's request is sent again from the new context.
    let next = &requests[3].conversation;
    assert_eq!(next.len(), 3);
    assert!(is_opening(&next[0]));
    assert_eq!(next[1], user("hi"));
    assert_eq!(next[2], user("The note."));
    assert_eq!(requests[3].previous_end, None);
    assert_eq!(attempt_numbers(&lines), [1, 1, 1, 1]);
    assert_no_stored_attempt(&lines);
}

#[test]
fn a_retried_note_request_counts_its_own_attempts() {
    let mut session = Session::with_tools(
        vec![
            called_both(100),
            overflowing(),
            failed(ErrorCode::RateLimited),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        None,
        both_tools(),
    )
    .handoff(settings())
    .retry(no_wait());

    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            TWO_CALLS,
            STEP,
            REJECTED,
            &[
                "handoff_started",
                "assistant_message_started",
                "usage_recorded",
                "assistant_message_completed",
                "retry_scheduled",
                "assistant_message_started",
                "assistant_message_delta",
                "assistant_message_delta",
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
    assert_eq!(attempt_numbers(&lines), [1, 1, 1, 2, 1]);
    assert_no_stored_attempt(&lines);
    let waits: Vec<&Envelope> = of_kind(&lines, "retry_scheduled");
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0].payload["attempt"], 2);
}

#[test]
fn a_note_request_rejected_for_size_fails_the_turn_context_overflow() {
    let mut session = Session::with_tools(
        vec![called_both(100), overflowing(), overflowing()],
        None,
        both_tools(),
    )
    .handoff(settings())
    .retry(no_wait());

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, TWO_CALLS, STEP, REJECTED, FAILED_NOTE, ENDED],
    );

    assert_eq!(outcome, Some(TurnOutcome::Failed));
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done[0].payload["outcome"], "failed");
    assert_eq!(error_of(done[0]), "context_overflow");
    assert_eq!(error_of(lines.last().unwrap()), "context_overflow");
    assert_eq!(session.requests().len(), 3);
}

#[test]
fn a_provider_overflow_with_automatic_handoff_off_fails_the_turn() {
    let mut session =
        Session::with_tools(vec![called_both(100), overflowing()], None, both_tools())
            .handoff(HandoffSettings {
                enabled: false,
                ..settings()
            })
            .retry(no_wait());

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(&lines, &[OPENING, STEP, TWO_CALLS, STEP, REJECTED, ENDED]);

    assert_eq!(outcome, Some(TurnOutcome::Failed));
    assert_eq!(error_of(lines.last().unwrap()), "context_overflow");
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn an_overflow_in_the_step_of_a_failed_automatic_handoff_fails_the_turn() {
    // The automatic handoff at the step's start fails, then the step's
    // request overflows: one handoff per step.
    let mut session = session(
        vec![
            called(TRIGGER - 1),
            failed(ErrorCode::QuotaExceeded),
            overflowing(),
        ],
        settings(),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, CALL_BODY, STEP, FAILED_NOTE, REJECTED, ENDED],
    );

    assert_eq!(outcome, Some(TurnOutcome::Failed));
    assert_eq!(of_kind(&lines, "handoff_started").len(), 1);
    assert_eq!(error_of(lines.last().unwrap()), "context_overflow");
}

#[test]
fn an_overflow_in_a_later_step_than_a_failed_automatic_handoff_hands_off() {
    // Step 2: the automatic handoff fails, and the request goes on. Step 3:
    // the automatic trigger stays blocked, but the overflow rule runs.
    let mut session = session(
        vec![
            called(TRIGGER - 1),
            failed(ErrorCode::QuotaExceeded),
            called(100),
            overflowing(),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
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
            FAILED_NOTE,
            CALL_BODY,
            STEP,
            REJECTED,
            HANDED_OFF,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let started = of_kind(&lines, "handoff_started");
    assert_eq!(started[0].payload["trigger"], "auto");
    assert_eq!(started[1].payload["trigger"], "overflow");
}

#[test]
fn a_second_overflow_in_one_step_fails_the_turn() {
    let mut session = session(
        vec![
            called(100),
            overflowing(),
            Scripted::text("The note."),
            overflowing(),
        ],
        settings(),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING, STEP, CALL_BODY, STEP, REJECTED, HANDED_OFF, REJECTED, ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Failed));
    assert_eq!(error_of(lines.last().unwrap()), "context_overflow");
    assert_eq!(of_kind(&lines, "handoff_started").len(), 1);
}

#[test]
fn a_last_step_with_no_tool_call_asks_for_the_note_on_the_unchanged_conversation() {
    // A text reply, then a steer that arrived during it: the next step's
    // request overflows with no tool result in the last step.
    let mut session = Session::with_tools_injecting(
        vec![
            said("Hello.", 100),
            overflowing(),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        vec![support::steer("and more")],
        vec![weather()],
    )
    .handoff(settings())
    .retry(no_wait());

    let (outcome, lines) = run(&mut session, "hi");

    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            REPLY,
            &["steering_queue"],
            STEP,
            &["steering_applied", "steering_queue"],
            REJECTED,
            HANDED_OFF,
            REPLY,
            ENDED,
        ],
    );
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(
        of_kind(&lines, "handoff_started")[0].payload["trigger"],
        "overflow"
    );
    let requests = session.requests();
    assert_eq!(requests.len(), 4);
    let asked = &requests[2].conversation;
    assert_eq!(asked[..asked.len() - 1], requests[1].conversation[..]);
    assert!(text_of(asked.last().unwrap()).starts_with("Fiber is about to restart"));
    // The steer is carried verbatim.
    let next = &requests[3].conversation;
    assert_eq!(next[1], user("hi"));
    assert_eq!(next[2], user("and more"));
    assert_eq!(next[3], user("The note."));
}

#[test]
fn a_result_before_the_last_reply_stays_in_the_note_request() {
    // The first turn's call and its text reply, then a second turn whose
    // first request overflows: its last step called no tool, and the
    // first turn's result is not the last step's.
    let mut session = session(
        vec![
            called(100),
            said("Hello.", 100),
            overflowing(),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        settings(),
    );
    let (_, first) = run(&mut session, "hi");
    assert_kinds(&first, &[OPENING, STEP, CALL_BODY, STEP, REPLY, ENDED]);
    let (outcome, lines) = run(&mut session, "more");

    assert_kinds(
        &lines,
        &[&["turn_started"], STEP, REJECTED, HANDED_OFF, REPLY, ENDED],
    );
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let requests = session.requests();
    let asked = &requests[3].conversation;
    assert_eq!(asked[..asked.len() - 1], requests[2].conversation[..]);
}

#[test]
fn a_result_whose_artifact_cannot_be_written_stays_in_the_note_request() {
    let mut session = session(
        vec![
            called(100),
            overflowing(),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        settings(),
    );
    // No file can be written under `artifacts/`.
    let artifacts = session.dir.join("artifacts");
    std::fs::remove_dir_all(&artifacts).unwrap();
    std::fs::write(&artifacts, "not a directory").unwrap();

    let (outcome, lines) = run(&mut session, "hi");

    assert_kinds(
        &lines,
        &[
            OPENING, STEP, CALL_BODY, STEP, REJECTED, HANDED_OFF, REPLY, ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(
        of_kind(&lines, "handoff_completed")[0].payload["outcome"],
        "completed"
    );
    let requests = session.requests();
    let asked = &requests[2].conversation;
    // The result is as it was: `abcd`.
    assert_eq!(asked[3], requests[1].conversation[3]);
    assert!(matches!(&asked[3], Input::ToolResult { text, .. } if text == "abcd"));
}

#[test]
fn a_cancel_during_the_overflow_note_request_ends_the_turn_interrupted() {
    let mut session = Session::cancelling_at_call(
        vec![called(100), overflowing(), Scripted::text("never read")],
        3,
        vec![weather()],
    )
    .handoff(settings())
    .retry(no_wait());

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            REJECTED,
            &[
                "handoff_started",
                "assistant_message_started",
                "usage_recorded",
                "handoff_completed",
            ],
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    assert_eq!(
        of_kind(&lines, "handoff_completed")[0].payload["outcome"],
        "cancelled"
    );
    assert_eq!(session.requests().len(), 3);
}

/// A settings whose automatic trigger is twice the window, so an estimate
/// over the window is not handed off at the step's start.
fn beyond_the_window() -> HandoffSettings {
    HandoffSettings {
        tokens: 1_000_000,
        window_fraction: 2.0,
        ..settings()
    }
}

const WINDOW: u64 = 1_000;

fn windowed(script: Vec<Scripted>, settings: HandoffSettings, window: u64) -> Session {
    Session::windowed(script, vec![weather()], window)
        .handoff(settings)
        .retry(no_wait())
}

#[test]
fn an_estimate_over_a_known_window_hands_off_before_sending() {
    // 1,000 prompt tokens and a one-token result: 1,001 over the window.
    let mut session = windowed(
        vec![
            called(WINDOW),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        beyond_the_window(),
        WINDOW,
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, REPLY, ENDED],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let started = of_kind(&lines, "handoff_started");
    assert_eq!(started[0].payload["trigger"], "overflow");
    // No request that would not fit was made: the note request, then the
    // step's request from the new context.
    let requests = session.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        matches!(&requests[1].conversation[3], Input::ToolResult { text, .. }
        if text.starts_with(MOVED))
    );
    assert_eq!(requests[2].conversation.len(), 3);
}

#[test]
fn an_estimate_exactly_at_the_window_is_sent() {
    let mut session = windowed(
        vec![called(WINDOW - 1), said("Done.", 50)],
        beyond_the_window(),
        WINDOW,
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(&lines, &[OPENING, STEP, CALL_BODY, STEP, REPLY, ENDED]);

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn automatic_handoff_off_skips_the_size_check() {
    let mut session = windowed(
        vec![called(WINDOW), said("Done.", 50)],
        HandoffSettings {
            enabled: false,
            ..beyond_the_window()
        },
        WINDOW,
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(&lines, &[OPENING, STEP, CALL_BODY, STEP, REPLY, ENDED]);
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn an_estimate_over_the_window_after_a_handoff_in_the_step_fails_the_turn() {
    // The window is 1,000, so the automatic trigger is 700: it hands off at
    // the step's start, and the next request is still estimated over.
    let mut session = windowed(
        vec![called(WINDOW), Scripted::text(&"x".repeat(8_000))],
        HandoffSettings {
            window_fraction: 0.7,
            ..beyond_the_window()
        },
        WINDOW,
    );

    let (outcome, lines) = run(&mut session, "hi");

    assert_kinds(&lines, &[OPENING, STEP, CALL_BODY, STEP, HANDED_OFF, ENDED]);
    assert_eq!(outcome, Some(TurnOutcome::Failed));
    assert_eq!(of_kind(&lines, "handoff_started").len(), 1);
    assert_eq!(error_of(lines.last().unwrap()), "context_overflow");
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn the_live_conversation_after_an_overflow_handoff_is_what_a_rebuild_renders() {
    let mut session = Session::with_tools(
        vec![
            called_both(100),
            overflowing(),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        None,
        both_tools(),
    )
    .handoff(settings())
    .retry(no_wait());

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING, STEP, TWO_CALLS, STEP, REJECTED, HANDED_OFF, REPLY, ENDED,
        ],
    );

    let requests = session.requests();
    let rebuilt = rebuild(&lines, MODEL).unwrap();
    // The request after the handoff, then the reply it got.
    assert_eq!(rebuilt.len(), 4);
    assert_eq!(rebuilt[..3], requests[3].conversation[..]);
    assert_eq!(text_of(&rebuilt[3]), "Done.");
}

#[test]
fn a_failed_overflow_handoff_leaves_what_a_rebuild_renders_as_before() {
    let mut session = Session::with_tools(
        vec![called_both(100), overflowing(), overflowing()],
        None,
        both_tools(),
    )
    .handoff(settings())
    .retry(no_wait());

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[OPENING, STEP, TWO_CALLS, STEP, REJECTED, FAILED_NOTE, ENDED],
    );

    // The conversation the overflowing request held, the results unmoved.
    let requests = session.requests();
    assert_eq!(rebuild(&lines, MODEL).unwrap(), requests[1].conversation);
}

#[test]
fn a_result_carried_past_a_tool_handoff_keeps_its_artifact_for_an_overflow() {
    let mut session = tool_session(
        vec![
            calling(&["wrapup", "long"], 500),
            overflowing(),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        vec![wrapping("wrapup", "Tool note."), long()],
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            TWO_CALLS,
            &["handoff_completed", "opening_message"],
            STEP,
            REJECTED,
            HANDED_OFF,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let cut = of_kind(&lines, "tool_call_completed")[1].payload["artifact"]
        .as_str()
        .unwrap();
    let session_dir = std::fs::canonicalize(&session.dir).unwrap();
    let asked = &session.requests()[2].conversation;
    // The sibling call and its result follow the note; the result is moved
    // to the artifact the tool wrote, which is not rewritten.
    assert!(matches!(&asked[asked.len() - 3], Input::ToolCall { .. }));
    let Input::ToolResult { text, .. } = &asked[asked.len() - 2] else {
        panic!("not a result");
    };
    assert_eq!(
        *text,
        format!("{MOVED}{}.", session_dir.join(cut).display())
    );
    assert_eq!(
        std::fs::read_to_string(session_dir.join(cut)).unwrap(),
        LONG
    );
}

#[test]
fn a_handoff_rebuilds_the_section_from_its_current_files() {
    let held = fakes::TempDir::new("fiber-handoff-sections");
    let file = held.path().join("index.md");
    std::fs::write(&file, "Section v1.\n").unwrap();
    let mut session = Session::open_sectioned(
        vec![
            called(100),
            said("Done.", 100),
            called(TRIGGER - 1),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        Vec::new(),
        vec![weather()],
        r#loop::Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
        vec![("fiber.test/notes".into(), vec![file.clone()], None)],
    )
    .handoff(settings())
    .retry(no_wait());

    // The first turn stays below the trigger: its opening carries v1.
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, CALL_BODY, STEP, REPLY, ENDED]);
    let first_openings = of_kind(&first, "opening_message");
    assert_eq!(first_openings.len(), 1);
    assert_eq!(
        first_openings[0].payload["extension_sections"],
        json!([{
            "extension": "fiber.test/notes",
            "files": [{"path": file.display().to_string(), "content": "Section v1.\n"}],
        }])
    );

    // The file changes before the handoff.
    std::fs::write(&file, "Section v2.\n").unwrap();

    let (outcome, second) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &second,
        &[
            &["instruction_file"],
            &["turn_started"],
            STEP,
            CALL_BODY,
            STEP,
            HANDED_OFF,
            REPLY,
            ENDED,
        ],
    );
    // The turn-start check reports the outside edit as a diff naming the
    // section, before the handoff rebuilds from the current files.
    let changed = of_kind(&second, "instruction_file");
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].payload["reason"], "changed");
    assert_eq!(changed[0].payload["extension"], "fiber.test/notes");
    // A one-line file: the diff headers outweigh it, so the full text.
    assert_eq!(changed[0].payload["sent"], "full");
    // The handoff's rebuild carries the current content, v2.
    let rebuilt = of_kind(&second, "opening_message");
    assert_eq!(rebuilt.len(), 1);
    assert_eq!(
        rebuilt[0].payload["extension_sections"],
        json!([{
            "extension": "fiber.test/notes",
            "files": [{"path": file.display().to_string(), "content": "Section v2.\n"}],
        }])
    );
    // The request after the handoff sends the rebuilt section.
    let requests = session.requests();
    let next = &requests.last().unwrap().conversation;
    assert!(is_opening(&next[0]));
    let text = text_of(&next[0]);
    assert!(
        text.contains("# From the fiber.test/notes extension"),
        "{text}"
    );
    assert!(text.contains("Section v2."), "{text}");
    assert!(!text.contains("Section v1."), "{text}");
}

#[test]
fn a_tool_handoff_in_a_step_that_already_handed_off_is_an_ordinary_result() {
    // The check hands off automatically, then the first reply in the new
    // context calls a tool that sets `control.handoff`: at most one handoff
    // runs per step, so the call's result stays as any result does.
    let mut session = tool_session(
        vec![
            called(TRIGGER - 1),
            Scripted::text("The note."),
            calling(&["wrapup"], 50),
            said("Done.", 50),
        ],
        vec![weather(), wrapping("wrapup", "Tool note.")],
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            HANDED_OFF,
            &call_lines(1),
            STEP,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(of_kind(&lines, "handoff_completed").len(), 1);
    let wrapup = of_kind(&lines, "tool_call_requested")[1]
        .action_id
        .as_ref()
        .unwrap()
        .0
        .clone();
    let requests = session.requests();
    assert_eq!(requests.len(), 4);
    // The context did not restart from the tool's note: the call and its
    // empty result follow the automatic handoff's note.
    let next = &requests[3].conversation;
    assert_eq!(next.len(), 5);
    assert!(is_opening(&next[0]));
    assert_eq!(next[1], user("hi"));
    assert_eq!(next[2], user("The note."));
    assert!(matches!(&next[3], Input::ToolCall { action_id, .. } if action_id.0 == wrapup));
    assert!(matches!(
        &next[4],
        Input::ToolResult { action_id, text, .. } if action_id.0 == wrapup && text.is_empty()
    ));
    assert_eq!(rebuild(&lines, MODEL).unwrap()[..5], next[..]);
}

#[test]
fn a_tool_may_hand_off_in_the_step_after_an_automatic_handoff() {
    let mut session = tool_session(
        vec![
            called(TRIGGER - 1),
            Scripted::text("The note."),
            calling(&["get_weather"], 50),
            calling(&["wrapup"], 60),
            said("Done.", 50),
        ],
        vec![weather(), wrapping("wrapup", "Tool note.")],
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            HANDED_OFF,
            &call_lines(1),
            STEP,
            &call_lines(1),
            &["handoff_completed", "opening_message"],
            STEP,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let next = &session.requests()[4].conversation;
    assert_eq!(next.len(), 3);
    assert_eq!(next[2], user("Tool note."));
}

#[test]
fn a_skill_written_before_a_handoff_expands_after_it() {
    let mut session = session(
        vec![
            said("Hi.", 100),
            said("Still here.", 100),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        settings(),
    );
    // Turn 1 writes the opening message.
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);

    // Written after the opening, the skill is unknown to the maintained
    // set when it is admitted: the prompt is sent as written, and the
    // turn-start check appends its added line.
    let dir = session.workspace.join(".agents/skills/late");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: late\ndescription: Runs late.\n---\nRuns late.\n",
    )
    .unwrap();
    let (outcome, second) = run(&mut session, "/late 1");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &second,
        &[&["skills_changed", "turn_started"], STEP, REPLY, ENDED],
    );
    assert_eq!(
        of_kind(&second, "turn_started")[0].payload["input"][0]["content"][0]["text"],
        "/late 1"
    );

    // A person's handoff rewrites the opening message, which refreshes
    // the set: only that refresh tells the two turns apart.
    session.inbox.send(handoff("c_h", None)).unwrap();
    let outcome = session.turn();
    let handed = session.lines();
    assert_kinds(&handed, &[&["turn_started"], STEP, HANDED_OFF, ENDED]);
    assert_eq!(outcome, Some(TurnOutcome::Completed));

    // Now the same prompt expands.
    let (outcome, third) = run(&mut session, "/late 1");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&third, &[&["turn_started"], STEP, REPLY, ENDED]);
    assert_eq!(
        of_kind(&third, "turn_started")[0].payload["input"][0]["content"][0]["text"],
        "Runs late.\n\n1"
    );
}

/// The model a switch moves to, and the window it declares: small enough
/// that one skill's listing line passes 10% of it.
const SWITCHED_MODEL: &str = "fake/model-2";
const SWITCHED_WINDOW: u64 = 100;

/// A `Prepare` switching to `reference` with `window` as its context
/// window, keeping the session's other choices.
fn prepare_with_window(provider: Arc<ScriptedProvider>, reference: &str, window: u64) -> Prepare {
    let reference = reference.to_owned();
    Arc::new(
        move |args: &contract::commands::ModelArgs,
              _label: Option<&str>,
              chosen: Option<ThinkingLevel>| {
            let thinking = match &args.thinking {
                Some(level) => Some(level.parse::<ThinkingLevel>().map_err(|_| Rejection {
                    code: ErrorCode::InvalidArguments,
                    message: format!("unknown thinking level `{level}`"),
                })?),
                None => chosen,
            };
            let kept = thinking.or(chosen);
            Ok(Prepared {
                provider: Arc::clone(&provider) as Arc<dyn Provider>,
                model: r#loop::Model {
                    reference: reference.clone(),
                    cost: None,
                    subscription: false,
                },
                thinking: kept,
                chosen: kept,
                credential: Some("work".into()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: window,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: Err(contract::shapes::Failure {
                    code: ErrorCode::NoModel,
                    message: r#loop::NO_MODEL_MESSAGE.into(),
                    retry_after_ms: None,
                    provider: None,
                }),
                web_search: Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
            })
        },
    )
}

#[test]
fn a_handoff_after_a_model_switch_sizes_notices_by_the_new_window() {
    // One skill with a long description: its listing line passes 10% of
    // the switched window, but not of the session's wide start window.
    let mut session = Session::windowed(
        vec![Scripted::text("Hi.")],
        Vec::new(),
        fakes::CONTEXT_WINDOW,
    )
    .handoff(settings());
    let dir = session.workspace.join(".agents/skills/big");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!(
            "---\nname: big\ndescription: {}\n---\nBody.\n",
            "x".repeat(200)
        ),
    )
    .unwrap();

    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    assert!(
        of_kind(&first, "notice").is_empty(),
        "the wide window fits the listing"
    );

    // A model switch before the handoff moves the window the size
    // notices use: the handoff's fresh opening message is sized by the
    // new window, not the set's startup snapshot.
    let next = Arc::new(ScriptedProvider::new(vec![
        Scripted::text("Again."),
        Scripted::text("The note."),
    ]));
    let looped = session.looped.take().unwrap().switcher(
        prepare_with_window(Arc::clone(&next), SWITCHED_MODEL, SWITCHED_WINDOW),
        Switchable { chosen: None },
    );
    session.looped = Some(looped);
    session.inbox.send(model(SWITCHED_MODEL, None)).unwrap();
    let (outcome, switched) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &switched,
        &[
            &["model_changed", "preamble_built", "turn_started"],
            STEP,
            REPLY,
            ENDED,
        ],
    );

    session.inbox.send(handoff("c_h", None)).unwrap();
    let outcome = session.turn();
    let handed = session.lines();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &handed,
        &[
            &["turn_started"],
            STEP,
            &[
                "handoff_started",
                "assistant_message_started",
                "assistant_message_delta",
                "assistant_message_delta",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "handoff_completed",
                "opening_message",
                "notice",
            ],
            ENDED,
        ],
    );
    let notices = of_kind(&handed, "notice");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].payload["code"], "skills_large");
}
