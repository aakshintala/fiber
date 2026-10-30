//! A `before_handoff` hook at the loop (`docs/extensions.md`, "The hook
//! points"; `docs/handoff.md`, "The handoff note"): a hook's note replaces
//! the note request, and `handoff_completed` carries it with the
//! extension's name.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::events::{HandoffTrigger, Notice, TurnOutcome};
use contract::hook::{
    BeforeHandoff, BeforeHandoffAnswer, BeforeHandoffOutcome, ContextAnswer, ContextOutcome,
    HookFailure, Hooks,
};
use contract::inbox::Delivery;
use contract::provider::Input;
use contract::shapes::{ContentPart, DeclaredEffects, Effect};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{Envelope, ErrorCode};
use fakes::Scripted;
use r#loop::{HandoffSettings, Retry};
use serde_json::{Map, Value, json};

use support::{
    ENDED, Gate, OPENING, REPLY, STEP, Script, Session, Tap, TestTool, assert_kinds,
    assert_no_stored_attempt, attempt_numbers, calls_reply, delivery, handoff, kinds, of_kind,
    start_added, start_empty, start_failed, tool_call_reply, with_tokens,
};

/// What one `before_handoff` call was shown.
struct Seen {
    trigger: HandoffTrigger,
    instructions: Option<String>,
    conversation: Vec<Input>,
}

/// A scripted `Hooks` fake: a queue of `before_handoff` answers, what each
/// call was shown, and an optional cancel inside the call. Every other
/// point passes empty.
struct HandoffHooks {
    answers: Mutex<VecDeque<BeforeHandoffAnswer>>,
    calls: AtomicUsize,
    seen: Mutex<Vec<Seen>>,
    cancel_in_call: Mutex<Option<Arc<r#loop::TurnCancel>>>,
    start_answers: Mutex<VecDeque<ContextAnswer>>,
}

impl HandoffHooks {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(VecDeque::new()),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
            cancel_in_call: Mutex::new(None),
            start_answers: Mutex::new(VecDeque::new()),
        })
    }

    /// Answers the next `before_handoff` call with `answer`.
    fn answer(&self, answer: BeforeHandoffAnswer) {
        self.answers.lock().unwrap().push_back(answer);
    }

    /// Answers the next `before_handoff` call with a hook's note.
    fn note(&self, text: &str, extension: &str) {
        self.answer(BeforeHandoffAnswer {
            outcome: BeforeHandoffOutcome::Note {
                text: text.into(),
                extension: extension.into(),
            },
            notices: Vec::new(),
        });
    }

    /// Answers the next `before_handoff` call with a `blocking` failure.
    fn failed(&self, extension: &str, message: &str) {
        self.answer(BeforeHandoffAnswer {
            outcome: BeforeHandoffOutcome::Failed(HookFailure {
                extension: extension.into(),
                message: message.into(),
            }),
            notices: Vec::new(),
        });
    }

    /// Cancels the turn inside the next `before_handoff` call.
    fn cancel_inside(&self, cancel: Arc<r#loop::TurnCancel>) {
        *self.cancel_in_call.lock().unwrap() = Some(cancel);
    }

    /// Answers the next `session_start` call with `answer`.
    fn start_answer(&self, answer: ContextAnswer) {
        self.start_answers.lock().unwrap().push_back(answer);
    }

    /// How many `before_handoff` calls ran.
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// What each call was shown, in call order.
    fn seen(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

fn no_note(notices: Vec<Notice>) -> BeforeHandoffAnswer {
    BeforeHandoffAnswer {
        outcome: BeforeHandoffOutcome::NoNote,
        notices,
    }
}

fn hook_failed(extension: &str, message: &str) -> Notice {
    Notice {
        code: ErrorCode::HookFailed,
        message: message.into(),
        extension: Some(extension.into()),
    }
}

impl Hooks for HandoffHooks {
    fn after_tool(
        &self,
        _call: &contract::hook::AfterToolCall<'_>,
    ) -> contract::hook::AfterToolAnswer {
        contract::hook::AfterToolAnswer {
            outcome: contract::hook::AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn before_tool(
        &self,
        _call: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::BeforeToolAnswer {
        contract::hook::BeforeToolAnswer {
            outcome: contract::hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(
        &self,
        _call: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(
        &self,
        _message: &contract::hook::BeforeMessage<'_>,
    ) -> contract::hook::BeforeMessageAnswer {
        contract::hook::BeforeMessageAnswer {
            outcome: contract::hook::BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(
        &self,
        _start: &contract::hook::SessionStart<'_>,
    ) -> contract::hook::ContextAnswer {
        self.start_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(ContextAnswer {
                outcome: ContextOutcome::Added(Vec::new()),
                notices: Vec::new(),
            })
    }

    fn turn_start(&self, _turn: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(
        &self,
        _call: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _end: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_handoff(&self, handoff: &BeforeHandoff<'_>) -> BeforeHandoffAnswer {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(Seen {
            trigger: handoff.trigger,
            instructions: handoff.instructions.map(str::to_owned),
            conversation: handoff.conversation.to_vec(),
        });
        if let Some(cancel) = self.cancel_in_call.lock().unwrap().take() {
            cancel.cancel();
        }
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(no_note(Vec::new()))
    }

    fn deliver_to(&self, _inbox: mpsc::Sender<Delivery>) {}
}

/// A handoff whose note a hook wrote: no note request, then the opening.
const HOOK_HANDED_OFF: &[&str] = &["handoff_started", "handoff_completed", "opening_message"];
/// A handoff whose note the model wrote, to the new opening message.
const MODEL_HANDED_OFF: &[&str] = &[
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
/// A reply calling the weather tool, with its streamed fragments, and the
/// call's run.
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

/// The trigger in these tests.
const TRIGGER: u64 = 1_000;

fn settings() -> HandoffSettings {
    HandoffSettings {
        tokens: TRIGGER,
        nudge: false,
        ..HandoffSettings::default()
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

fn hooked(script: Vec<Scripted>, hooks: Arc<HandoffHooks>) -> Session {
    Session::with_tools(script, None, vec![weather()])
        .handoff(settings())
        .retry(no_wait())
        .hooks(hooks as Arc<dyn Hooks>)
}

/// A retryable failure retries at once and never parks, one retry.
fn no_wait() -> Retry {
    Retry {
        attempts: 1,
        initial: Duration::ZERO,
        max: Duration::ZERO,
    }
}

fn run(session: &mut Session, prompt: &str) -> (Option<TurnOutcome>, Vec<Envelope>) {
    session.inbox.send(delivery(prompt)).unwrap();
    let outcome = session.turn();
    (outcome, session.lines())
}

fn user(text: &str) -> Input {
    Input::User {
        text: text.into(),
        images: Vec::new(),
    }
}

fn is_opening(input: &Input) -> bool {
    matches!(input, Input::User { text, .. } if text.starts_with("This message is from Fiber"))
}

#[test]
fn a_hook_note_completes_the_handoff_with_no_note_request() {
    let hooks = HandoffHooks::new();
    hooks.note("Hook note.", "fiber.test/notes");
    let mut session = hooked(
        vec![called(TRIGGER - 1), said("Done.", 50)],
        Arc::clone(&hooks),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            HOOK_HANDED_OFF,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done.len(), 1);
    assert_eq!(
        serde_json::Value::Object(done[0].payload.clone()),
        json!({
            "outcome": "completed",
            "note_text": "Hook note.",
            "extension": "fiber.test/notes",
            "tokens_before": TRIGGER,
        })
    );
    // One request less than with the model's note: the note request never
    // ran, and no usage was recorded for it.
    assert_eq!(of_kind(&lines, "usage_recorded").len(), 2);
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    // The next request is the opening message, the turn's input, then the
    // note.
    let next = &requests[1].conversation;
    assert_eq!(next.len(), 3);
    assert!(is_opening(&next[0]));
    assert_eq!(next[1], user("hi"));
    assert_eq!(next[2], user("Hook note."));
    assert_eq!(requests[1].previous_end, None);
    // The hook saw the trigger, no instructions, and the loop's
    // conversation at that point: the first request's history, then the
    // call and its result.
    assert_eq!(hooks.calls(), 1);
    let seen = hooks.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].trigger, HandoffTrigger::Auto);
    assert_eq!(seen[0].instructions, None);
    let first = &requests[0].conversation;
    assert_eq!(seen[0].conversation.len(), first.len() + 2);
    assert_eq!(seen[0].conversation[..first.len()], first[..]);
    assert!(
        matches!(&seen[0].conversation[first.len()], Input::ToolCall { call, .. } if call.name == "get_weather")
    );
    assert!(matches!(
        &seen[0].conversation[first.len() + 1],
        Input::ToolResult { .. }
    ));
}

#[test]
fn no_hook_note_leaves_the_note_to_the_model() {
    let hooks = HandoffHooks::new();
    let mut session = hooked(
        vec![
            called(TRIGGER - 1),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        Arc::clone(&hooks),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            MODEL_HANDED_OFF,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(hooks.calls(), 1);
    let done = of_kind(&lines, "handoff_completed");
    assert!(done[0].payload.contains_key("note"));
    assert!(!done[0].payload.contains_key("note_text"));
}

#[test]
fn a_blocking_hook_failure_fails_the_handoff_and_blocks_the_rest_of_the_turn() {
    let hooks = HandoffHooks::new();
    hooks.failed("fiber.test/notes", "boom");
    let mut session = hooked(
        vec![called(TRIGGER - 1), said("Done.", 50)],
        Arc::clone(&hooks),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &["handoff_started", "handoff_completed"],
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(
        serde_json::Value::Object(done[0].payload.clone()),
        json!({
            "outcome": "failed",
            "error": {
                "code": "hook_failed",
                "message": "The `before_handoff` hook of extension fiber.test/notes failed, so the handoff did not complete: boom",
            },
            "tokens_before": TRIGGER,
        })
    );
    // No note request ran, and the next request is the old conversation.
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    assert!(matches!(
        requests[1].conversation[2],
        Input::ToolCall { .. }
    ));
    // The failed handoff blocks the automatic trigger for the rest of the
    // turn: the hook ran once.
    assert_eq!(hooks.calls(), 1);
}

#[test]
fn hook_notices_ride_with_the_handoff_and_the_note_request() {
    // A hook note: the notice lands after `handoff_started` and before
    // `handoff_completed`.
    let hooks = HandoffHooks::new();
    hooks.answer(BeforeHandoffAnswer {
        outcome: BeforeHandoffOutcome::Note {
            text: "Hook note.".into(),
            extension: "fiber.test/notes".into(),
        },
        notices: vec![hook_failed("fiber.test/notes", "a soft failure")],
    });
    let mut session = hooked(
        vec![called(TRIGGER - 1), said("Done.", 50)],
        Arc::clone(&hooks),
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
                "notice",
                "handoff_completed",
                "opening_message",
            ],
            REPLY,
            ENDED,
        ],
    );
    // No note: the notice lands before the note request.
    let hooks = HandoffHooks::new();
    hooks.answer(no_note(vec![hook_failed(
        "fiber.test/notes",
        "a soft failure",
    )]));
    let mut session = hooked(
        vec![
            called(TRIGGER - 1),
            Scripted::text("The note."),
            said("Done.", 50),
        ],
        Arc::clone(&hooks),
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
                "notice",
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
}

#[test]
fn a_person_handoff_hands_the_hook_its_instructions() {
    let hooks = HandoffHooks::new();
    hooks.note("Hook note.", "fiber.test/notes");
    let mut session = Session::with_tools_injecting(
        vec![called(100), said("Done.", 50)],
        vec![handoff("c_h", Some("focus on tests"))],
        vec![weather()],
    )
    .handoff(settings())
    .retry(no_wait())
    .hooks(Arc::clone(&hooks) as Arc<dyn Hooks>);

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            HOOK_HANDED_OFF,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let seen = hooks.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].trigger, HandoffTrigger::Person);
    assert_eq!(seen[0].instructions.as_deref(), Some("focus on tests"));
    assert_eq!(
        of_kind(&lines, "handoff_completed")[0].payload["instructions"],
        "focus on tests"
    );
}

#[test]
fn a_person_handoff_turn_with_no_input_ends_completed_after_the_hook_note() {
    let hooks = HandoffHooks::new();
    hooks.note("Hook note.", "fiber.test/notes");
    let mut session = Session::with_tools(Vec::new(), None, vec![weather()])
        .handoff(settings())
        .retry(no_wait())
        .hooks(Arc::clone(&hooks) as Arc<dyn Hooks>);
    session
        .inbox
        .send(handoff("c_h", Some("focus on tests")))
        .unwrap();

    let outcome = session.turn();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            &["handoff_started", "handoff_completed", "opening_message"],
            ENDED,
        ],
    );
    let done = of_kind(&lines, "handoff_completed");
    // No reply measured this context, so its size is estimated from the
    // conversation the hook saw. That starts with the opening message,
    // which embeds the temp workspace and session-log paths (plus
    // os/arch), so its length differs between macOS and Linux: derive
    // the expectation with the same `ceil(bytes / 4)` estimate instead
    // of hardcoding it (as `handoff.rs`'s person-handoff test does).
    let seen = hooks.seen();
    assert_eq!(seen.len(), 1);
    let expected_tokens: u64 = seen[0]
        .conversation
        .iter()
        .map(|input| {
            let bytes = match input {
                Input::User { text, .. } | Input::Assistant { text, .. } => text.len(),
                Input::Reasoning { text, .. } | Input::ToolResult { text, .. } => text.len(),
                Input::ToolCall { call, .. } => call.name.len() + call.arguments.to_string().len(),
                Input::ToolSearchResult { loaded, .. } => {
                    contract::provider::loaded_text(loaded).len()
                }
            };
            bytes.div_ceil(4) as u64
        })
        .sum();
    assert_eq!(
        serde_json::Value::Object(done[0].payload.clone()),
        json!({
            "outcome": "completed",
            "note_text": "Hook note.",
            "extension": "fiber.test/notes",
            "tokens_before": expected_tokens,
            "instructions": "focus on tests",
        })
    );
    // No note request and no answer: the provider was never called.
    assert!(session.requests().is_empty());
    assert_eq!(hooks.calls(), 1);
}

/// A reply calling the weather tool and the long tool, with `tokens` in its
/// prompt.
fn called_both(tokens: u64) -> Scripted {
    with_tokens(tool_call_reply("", &["get_weather", "long"]), tokens, 0)
}

fn long() -> Arc<TestTool> {
    let mut tool = TestTool::reads("long", "0123456789");
    tool.bound = Bound { start: 4, end: 0 };
    Arc::new(tool)
}

fn overflowing() -> Scripted {
    failed(ErrorCode::ContextOverflow)
}

fn failed(code: ErrorCode) -> Scripted {
    Scripted::failed(contract::shapes::Failure {
        code,
        message: "The call failed.".into(),
        retry_after_ms: None,
        provider: None,
    })
}

#[test]
fn an_overflow_handoff_hands_the_hook_the_moved_results() {
    let hooks = HandoffHooks::new();
    hooks.note("Hook note.", "fiber.test/notes");
    let mut session = Session::with_tools(
        vec![called_both(100), overflowing(), said("Done.", 50)],
        None,
        vec![weather(), long()],
    )
    .handoff(settings())
    .retry(no_wait())
    .hooks(Arc::clone(&hooks) as Arc<dyn Hooks>);

    let (outcome, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            TWO_CALLS,
            STEP,
            REJECTED,
            HOOK_HANDED_OFF,
            REPLY,
            ENDED,
        ],
    );

    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(
        of_kind(&lines, "handoff_started")[0].payload["trigger"],
        "overflow"
    );
    // The hook saw the conversation with the last step's results moved out.
    let seen = hooks.seen();
    assert_eq!(seen.len(), 1);
    let results: Vec<&str> = seen[0]
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolResult { text, .. } => Some(text.as_str()),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    for result in results {
        assert!(result.contains("moved out of your context"), "{result}");
    }
    // A hook note sends the step's request from the new context.
    let requests = session.requests();
    assert_eq!(requests.len(), 3);
    let next = &requests[2].conversation;
    assert_eq!(next.len(), 3);
    assert!(is_opening(&next[0]));
    assert_eq!(next[1], user("hi"));
    assert_eq!(next[2], user("Hook note."));
    assert_eq!(attempt_numbers(&lines), [1, 1, 1]);
    assert_no_stored_attempt(&lines);
}

/// A reply calling both tools, with its streamed fragments, and the calls'
/// runs.
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
/// A model call the provider rejected for size.
const REJECTED: &[&str] = &[
    "assistant_message_started",
    "usage_recorded",
    "assistant_message_completed",
];

#[test]
fn a_failed_overflow_hook_fails_the_turn_context_overflow() {
    let hooks = HandoffHooks::new();
    hooks.failed("fiber.test/notes", "boom");
    let mut session = Session::with_tools(
        vec![called_both(100), overflowing()],
        None,
        vec![weather(), long()],
    )
    .handoff(settings())
    .retry(no_wait())
    .hooks(Arc::clone(&hooks) as Arc<dyn Hooks>);

    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Failed));
    assert_eq!(
        lines.last().unwrap().payload["error"]["code"],
        "context_overflow"
    );
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done[0].payload["error"]["code"], "hook_failed");
}

/// A tool that returns no content and `control.handoff` of `note`.
fn wrapping(name: &'static str, note: &str) -> Arc<TestTool> {
    let mut tool = TestTool::reads(name, "");
    tool.output.content = Vec::new();
    tool.output.control = Some(contract::events::Control {
        handoff: Some(note.into()),
        ..Default::default()
    });
    Arc::new(tool)
}

/// A reply of `names` called, each with its own arguments, with `tokens` in
/// its prompt.
fn calling(names: &[&str], tokens: u64) -> Scripted {
    let calls: Vec<(&str, Value)> = names
        .iter()
        .enumerate()
        .map(|(index, name)| (*name, json!({"city": format!("city {index}")})))
        .collect();
    with_tokens(calls_reply("", &calls), tokens, 0)
}

#[test]
fn a_tool_started_handoff_never_calls_the_hook() {
    let hooks = HandoffHooks::new();
    hooks.note("Hook note.", "fiber.test/notes");
    let mut session = Session::with_tools(
        vec![calling(&["wrapup"], 500), said("Done.", 50)],
        None,
        vec![wrapping("wrapup", "Tool note.")],
    )
    .handoff(settings())
    .retry(no_wait())
    .hooks(Arc::clone(&hooks) as Arc<dyn Hooks>);

    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(hooks.calls(), 0);
    // The tool's argument is the note, as today: no hook text anywhere.
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done.len(), 1);
    assert!(done[0].payload.contains_key("note"));
    assert!(!done[0].payload.contains_key("note_text"));
    assert!(!done[0].payload.contains_key("extension"));
}

#[test]
fn a_cancel_during_the_hook_ends_the_turn_interrupted() {
    let hooks = HandoffHooks::new();
    hooks.note("Hook note.", "fiber.test/notes");
    let mut session = hooked(
        vec![called(TRIGGER - 1), said("Done.", 50)],
        Arc::clone(&hooks),
    );
    hooks.cancel_inside(Arc::clone(&session.cancel));
    hooks.note("Second note.", "fiber.test/notes");

    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(
        serde_json::Value::Object(done[0].payload.clone()),
        json!({"outcome": "cancelled", "tokens_before": TRIGGER})
    );
    // No note request ran.
    assert_eq!(session.requests().len(), 1);
    // The context is unchanged: the next turn's hook sees the old
    // conversation, then the new input.
    let (outcome, _) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(hooks.calls(), 2);
    let seen = hooks.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].conversation.len(), seen[0].conversation.len() + 1);
    assert_eq!(
        seen[1].conversation[..seen[0].conversation.len()],
        seen[0].conversation[..]
    );
    assert_eq!(
        seen[1].conversation[seen[0].conversation.len()],
        user("again")
    );
}

/// A tool that cancels the turn when it runs.
struct TripTool {
    cancel: Mutex<Option<Arc<r#loop::TurnCancel>>>,
}

impl Tool for TripTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "trip".into(),
            description: "The test tool that cancels.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            review_when_confined: false,
            hosts: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn contract::emit::Emit) -> Output {
        if let Some(cancel) = self.cancel.lock().unwrap().clone() {
            cancel.cancel();
        }
        Output {
            content: vec![ContentPart::Text {
                text: "tripped".into(),
            }],
            ..Default::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

#[test]
fn a_cancel_before_the_handoff_check_runs_no_hook() {
    let trip = Arc::new(TripTool {
        cancel: Mutex::new(None),
    });
    let hooks = HandoffHooks::new();
    hooks.note("Hook note.", "fiber.test/notes");
    let mut session = Session::with_tools(
        vec![tripping(TRIGGER - 1), said("Done.", 50)],
        None,
        vec![Arc::clone(&trip) as Arc<dyn Tool>, weather()],
    )
    .handoff(settings())
    .retry(no_wait())
    .hooks(Arc::clone(&hooks) as Arc<dyn Hooks>);
    trip.cancel
        .lock()
        .unwrap()
        .replace(Arc::clone(&session.cancel));

    let (outcome, lines) = run(&mut session, "hi");
    // The step's tool cancelled the turn, so it ends before the next
    // check: the hook is never called.
    assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    assert_eq!(hooks.calls(), 0);
    assert!(!kinds(&lines).contains(&"handoff_started"));
}

/// A reply calling the trip tool, with `tokens` in its prompt.
fn tripping(tokens: u64) -> Scripted {
    with_tokens(tool_call_reply("", &["trip"]), tokens, 0)
}

#[test]
fn a_shutdown_while_the_step_tool_runs_runs_no_hook() {
    // With hooks and without: the turn ends the same way, and with hooks
    // the hook never ran.
    let mut outcomes = Vec::new();
    let mut all_kinds = Vec::new();
    for with_hooks in [true, false] {
        let gate = Arc::new(Gate::default());
        let mut tool = TestTool::reads("parker", "parked");
        tool.script = vec![Script::Wait(Arc::clone(&gate))];
        let tool = Arc::new(tool);
        let hooks = HandoffHooks::new();
        hooks.note("Hook note.", "fiber.test/notes");
        let mut session = Session::with_tools(
            vec![parking(TRIGGER - 1), said("Done.", 50)],
            None,
            vec![tool],
        )
        .handoff(settings())
        .retry(no_wait());
        if with_hooks {
            session.looped = session
                .looped
                .take()
                .map(|looped| looped.hooks(Arc::clone(&hooks) as Arc<dyn Hooks>));
        }
        session.inbox.send(delivery("hi")).unwrap();
        let tap = Tap::new(&session.log);
        let cancel = Arc::clone(&session.cancel);
        std::thread::scope(|scope| {
            let turn = scope.spawn(|| session.turn());
            // The tool started its call and waits at its gate: the shutdown
            // lands while the step runs, before the handoff check.
            tap.wait_for("tool_call_started");
            cancel.shutdown(143);
            gate.open();
            outcomes.push(turn.join().unwrap());
        });
        gate.check("parker");
        if with_hooks {
            assert_eq!(hooks.calls(), 0);
        }
        let lines = session.lines();
        let kinds_now: Vec<String> = kinds(&lines).into_iter().map(str::to_owned).collect();
        all_kinds.push(kinds_now);
    }
    assert_eq!(outcomes[0], outcomes[1], "the outcomes match");
    assert_eq!(outcomes[0], Some(TurnOutcome::Interrupted));
    assert_eq!(all_kinds[0], all_kinds[1], "the turn ends as it does today");
}

/// A reply calling the parking tool, with `tokens` in its prompt.
fn parking(tokens: u64) -> Scripted {
    with_tokens(tool_call_reply("", &["parker"]), tokens, 0)
}

fn text_of(input: &Input) -> &str {
    match input {
        Input::User { text, .. } | Input::Assistant { text, .. } => text,
        other @ (Input::Reasoning { .. }
        | Input::ToolCall { .. }
        | Input::ToolResult { .. }
        | Input::ToolSearchResult { .. }) => {
            panic!("not a message: {other:?}")
        }
    }
}

#[test]
fn the_live_conversation_after_a_hook_note_is_what_a_rebuild_renders() {
    let hooks = HandoffHooks::new();
    hooks.note("Hook note.", "fiber.test/notes");
    let mut session = hooked(
        vec![
            called(TRIGGER - 1),
            said("Done.", 50),
            Scripted::text("Again."),
        ],
        Arc::clone(&hooks),
    );

    let (_, lines) = run(&mut session, "hi");
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            HOOK_HANDED_OFF,
            REPLY,
            ENDED,
        ],
    );

    let requests = session.requests();
    let rebuilt = session.rebuilt();
    // The request after the handoff, then the reply it got.
    assert_eq!(rebuilt.len(), 4);
    assert_eq!(rebuilt[..3], requests[1].conversation[..]);
    assert_eq!(text_of(&rebuilt[3]), "Done.");
}

#[test]
fn a_hook_note_runs_session_start_with_reason_handoff() {
    let hooks = HandoffHooks::new();
    hooks.note("Hook note.", "fiber.test/notes");
    hooks.start_answer(start_empty());
    hooks.start_answer(start_added("fiber.test/inbox", "at handoff"));
    let mut session = hooked(
        vec![called(TRIGGER - 1), said("Done.", 50)],
        Arc::clone(&hooks),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &[
                "handoff_started",
                "handoff_completed",
                "opening_message",
                "context_added",
            ],
            REPLY,
            ENDED,
        ],
    );
    // The next request is the opening message, the context, the input,
    // then the note.
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    let next = &requests[1].conversation;
    assert_eq!(next.len(), 4);
    assert!(is_opening(&next[0]));
    assert!(text_of(&next[1]).contains("at handoff"), "{next:?}");
    assert_eq!(next[2], user("hi"));
    assert_eq!(next[3], user("Hook note."));
}

#[test]
fn a_session_start_failure_after_a_hook_note_fails_the_handoff_with_no_note() {
    let hooks = HandoffHooks::new();
    hooks.note("Hook note.", "fiber.test/notes");
    hooks.start_answer(start_empty());
    hooks.start_answer(start_failed("fiber.test/inbox", "no context"));
    let mut session = hooked(
        vec![called(TRIGGER - 1), said("Done.", 50)],
        Arc::clone(&hooks),
    );

    let (outcome, lines) = run(&mut session, "hi");
    // The handoff failed, but the turn goes on from the old context.
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            CALL_BODY,
            STEP,
            &["handoff_started", "handoff_completed"],
            REPLY,
            ENDED,
        ],
    );
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["outcome"], "failed");
    assert_eq!(done[0].payload["error"]["code"], "hook_failed");
    assert!(
        done[0].payload["error"]["message"]
            .as_str()
            .unwrap()
            .contains("fiber.test/inbox"),
        "{}",
        done[0].payload["error"]["message"]
    );
    assert!(!done[0].payload.contains_key("note_text"));
    assert!(!done[0].payload.contains_key("note"));
}

#[test]
fn a_failed_hook_note_handoff_blocks_the_rest_of_the_turn() {
    // A hook note whose `session_start` fails ends the handoff `failed`:
    // the automatic trigger does not fire again in that turn, so a later
    // step that crosses the trigger runs no second hook. Under `!=` the
    // block is never set on failure and the hook runs twice.
    let hooks = HandoffHooks::new();
    hooks.note("Hook note.", "fiber.test/notes");
    hooks.note("Second note.", "fiber.test/notes");
    hooks.start_answer(start_empty());
    hooks.start_answer(start_failed("fiber.test/inbox", "no context"));
    // The second step reports a full trigger of tokens, so the check
    // before the third step crosses the trigger again: with the block
    // the hook still ran once, without it twice.
    let mut session = hooked(
        vec![called(TRIGGER - 1), called(TRIGGER), said("Done.", 50)],
        Arc::clone(&hooks),
    );

    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    // The failed handoff blocks the automatic trigger for the rest of
    // the turn: the hook ran once, even though the later step stays over
    // the trigger.
    assert_eq!(hooks.calls(), 1);
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["outcome"], "failed");
    assert_eq!(done[0].payload["error"]["code"], "hook_failed");
}
