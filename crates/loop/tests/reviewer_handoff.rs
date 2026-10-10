//! The handoff selection (`docs/permissions.md`, "At a handoff"): at each
//! completed handoff the reviewer is asked which of the person's messages
//! still bind, and its input restarts from those messages, word for word.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::sync::{Arc, mpsc};
use std::thread;

use contract::events::{CacheLifetime, Control, TurnOutcome};
use contract::provider::{Cost, Input, ModelRequest, Provider};
use contract::shapes::{Effect, Failure};
use contract::tool::Tool;
use contract::{Envelope, ErrorCode, ThinkingLevel};
use fakes::{Scripted, ScriptedProvider};
use r#loop::{BlockLimits, Loop, Model, Permissions, PromptInputs, Reviewer};
use serde_json::{Value, json};

use support::{
    DEADLINE, MODEL, REVIEWER_MODEL, Session, TestTool, calls_reply, delivery, handoff, kinds,
};

fn paris() -> Value {
    json!({"city": "Paris"})
}

/// A tool whose calls declare `executes`, so the reviewer judges them.
fn shell() -> Arc<TestTool> {
    let mut tool = TestTool::declaring("shell", "Ran it.", vec![Effect::Executes], None);
    tool.subject = None;
    tool.prefix = None;
    Arc::new(tool)
}

/// The session script every selection flow shares: a reviewed call, a
/// said message, the handoff's note request, then a reviewed call.
fn script() -> Vec<Scripted> {
    vec![
        calls_reply("", &[("shell", paris())]),
        Scripted::text("Done."),
        Scripted::text("Hi there."),
        Scripted::text("The note."),
        calls_reply("", &[("shell", paris())]),
        Scripted::text("Done."),
    ]
}

/// One prompt turn, asserting it completes.
fn prompt_turn(session: &mut Session, text: &str) -> Vec<Envelope> {
    session.inbox.send(delivery(text)).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.lines()
}

/// One person handoff turn, asserting it completes.
fn handoff_turn(session: &mut Session, id: &str) -> Vec<Envelope> {
    session.inbox.send(handoff(id, None)).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.lines()
}

/// The shared flow: a reviewed call, "say hi", a person handoff, then a
/// reviewed call.
fn run_flow(session: &mut Session) -> Vec<Envelope> {
    let mut lines = Vec::new();
    lines.extend(prompt_turn(session, "never push to main"));
    lines.extend(prompt_turn(session, "say hi"));
    lines.extend(handoff_turn(session, "c_h"));
    lines.extend(prompt_turn(session, "now run it"));
    lines
}

/// The `seq` of the `turn_started` line holding the person's `text`.
fn turn_started_seq(lines: &[Envelope], text: &str) -> u64 {
    lines
        .iter()
        .find(|line| {
            line.kind == "turn_started"
                && line.payload["input"].as_array().is_some_and(|input| {
                    input.iter().any(|item| {
                        item["content"]
                            .as_array()
                            .is_some_and(|content| content.iter().any(|part| part["text"] == text))
                    })
                })
        })
        .unwrap()
        .seq
        .unwrap()
        .0
}

/// Every `reviewer_kept` line, in order.
fn kept_lines(lines: &[Envelope]) -> Vec<&Envelope> {
    lines
        .iter()
        .filter(|line| line.kind == "reviewer_kept")
        .collect()
}

/// The notice with `code`, if any.
fn notice<'a>(lines: &'a [Envelope], code: &str) -> Option<&'a Envelope> {
    lines
        .iter()
        .find(|line| line.kind == "notice" && line.payload["code"] == code)
}

/// A reviewer request's conversation texts. Every reviewer item is a user
/// item.
fn texts(request: &ModelRequest) -> Vec<&str> {
    request
        .conversation
        .iter()
        .map(|input| match input {
            Input::User { text, .. } => text.as_str(),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. } => {
                panic!("not a user item: {input:?}")
            }
        })
        .collect()
}

fn failed(message: &str) -> Scripted {
    Scripted::failed(Failure {
        code: ErrorCode::RateLimited,
        message: message.into(),
        retry_after_ms: None,
        provider: None,
    })
}

#[test]
fn the_reviewer_keeps_the_selected_messages_then_what_follows() {
    let tool = shell();
    let mut session = Session::with_tools(script(), None, vec![tool as Arc<dyn Tool>]);
    let reviewer = session.reviewer_thinking(
        vec![
            Scripted::text("allow"),
            Scripted::text("1"),
            Scripted::text("allow"),
        ],
        vec![ThinkingLevel::Minimal],
    );
    let lines = run_flow(&mut session);
    // The whole flow in order: reviewed call, text, handoff with its selection, reviewed call.
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
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "usage_recorded",
            "reviewer_kept",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
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

    let seq = turn_started_seq(&lines, "never push to main");
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].payload["kept"], json!([{"seq": seq, "item": 0}]));
    assert!(kept[0].payload.get("failed").is_none());

    let requests = reviewer.requests();
    assert_eq!(requests.len(), 3);
    for (n, request) in requests.iter().enumerate() {
        assert_eq!(
            request.thinking,
            Some(ThinkingLevel::Minimal),
            "request {n}"
        );
    }
    let after = &requests[2];
    let after_texts = texts(after);
    assert_eq!(after_texts.len(), 5);
    assert_eq!(after_texts[0], "The person: never push to main");
    assert_eq!(after_texts[1], "The person: now run it");
    assert!(after_texts[2].starts_with("Tool call: "));
    assert!(after_texts[3].starts_with("Declared effects: "));
    assert!(after_texts[4].starts_with("## first-pass\n"));
    // The input's prefix changed, so the first review after the handoff
    // sends `previous_end: None`.
    assert_eq!(after.previous_end, None);
    // Neither the dropped message nor the dropped call is sent: the one
    // tool call is the one under review.
    assert!(!after_texts.iter().any(|text| text.contains("say hi")));
    assert_eq!(
        after_texts
            .iter()
            .filter(|text| text.starts_with("Tool call: "))
            .count(),
        1
    );
}

#[test]
fn selection_prose_never_reaches_a_later_prompt() {
    let tool = shell();
    let mut session = Session::with_tools(script(), None, vec![tool as Arc<dyn Tool>]);
    let reviewer = session.reviewer(vec![
        Scripted::text("allow"),
        Scripted::text("1\nThe person said ZQX-PROSE, which binds."),
        Scripted::text("allow"),
    ]);
    let lines = run_flow(&mut session);
    // The whole flow in order, as above: the prose reply still reads as a selection.
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
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "usage_recorded",
            "reviewer_kept",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
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
    assert_eq!(kept_lines(&lines).len(), 1);
    for request in reviewer.requests() {
        assert!(!request.system_prompt.contains("ZQX-PROSE"));
        for input in &request.conversation {
            assert!(!format!("{input:?}").contains("ZQX-PROSE"));
        }
    }
}

#[test]
fn a_resume_rebuilds_the_reviewers_input_from_the_selection() {
    // Run A goes straight through the shared flow.
    let tool = shell();
    let mut session_a = Session::with_tools(script(), None, vec![tool as Arc<dyn Tool>]);
    let reviewer_a = session_a.reviewer(vec![
        Scripted::text("allow"),
        Scripted::text("1"),
        Scripted::text("allow"),
    ]);
    run_flow(&mut session_a);

    // Run B stops after the handoff turn, drops the loop, resumes from the
    // log with a reviewer answering what is left, then runs the last turn.
    let tool = shell();
    let mut session_b = Session::with_tools(script(), None, vec![tool as Arc<dyn Tool>]);
    session_b.reviewer(vec![Scripted::text("allow"), Scripted::text("1")]);
    prompt_turn(&mut session_b, "never push to main");
    prompt_turn(&mut session_b, "say hi");
    handoff_turn(&mut session_b, "c_h");
    drop(session_b.looped.take());
    let resumed = r#loop::resumed(&session_b.dir).unwrap();
    let (tx, rx) = mpsc::channel();
    let home = session_b.dir.parent().unwrap().to_path_buf();
    let session_log = session_b.dir.join("events.jsonl").display().to_string();
    let uncoerced = Arc::clone(&session_b.clock);
    let clock: Arc<dyn contract::clock::Clock> = uncoerced;
    let mut prompt = PromptInputs::new(
        home,
        "/bin/sh".into(),
        session_log,
        clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let rules: Arc<dyn contract::rules::Rules> = session_b.rules.clone();
    let tool = shell();
    let reviewer_b = Arc::new(ScriptedProvider::new(vec![Scripted::text("allow")]));
    let mut looped = Loop::resume(
        Arc::clone(&session_b.log),
        resumed,
        Arc::clone(&session_b.provider) as Arc<dyn Provider>,
        Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
        prompt,
        rx,
        vec![("builtin".into(), tool as Arc<dyn Tool>)],
        Permissions {
            workspace: session_b.workspace.display().to_string(),
            credentials: session_b.credentials.clone(),
            credential_files: Vec::new(),
            rules,
        },
    )
    .unwrap()
    .reviewer(
        Ok(Reviewer {
            provider: reviewer_b.clone(),
            model: Model {
                reference: REVIEWER_MODEL.into(),
                cost: None,
                subscription: false,
            },
            cache_lifetime: CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            thinking_levels: Vec::new(),
        }),
        BlockLimits::default(),
    );
    tx.send(delivery("now run it")).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    assert_eq!(
        finished
            .recv_timeout(DEADLINE)
            .expect("the turn ended in time"),
        Some(TurnOutcome::Completed)
    );

    let request_a = &reviewer_a.requests()[2];
    let request_b = &reviewer_b.requests()[0];
    // The two sessions live in different temporary workspaces, so the
    // effects item's workspace root is normalized before comparing.
    let normalized = |conversation: &[Input], workspace: &str| {
        conversation
            .iter()
            .map(|input| match input {
                Input::User { text, .. } => text.replace(workspace, "<workspace>"),
                Input::Assistant { .. }
                | Input::Reasoning { .. }
                | Input::ToolCall { .. }
                | Input::ToolResult { .. } => {
                    panic!("not a user item: {input:?}")
                }
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        normalized(
            &request_a.conversation,
            &session_a.workspace.display().to_string()
        ),
        normalized(
            &request_b.conversation,
            &session_b.workspace.display().to_string()
        )
    );
    assert_eq!(request_a.system_prompt, request_b.system_prompt);
    assert_eq!(request_a.previous_end, request_b.previous_end);
}

#[test]
fn a_failed_selection_keeps_every_earlier_person_message() {
    let tool = shell();
    let mut session = Session::with_tools(script(), None, vec![tool as Arc<dyn Tool>]);
    let reviewer = session.reviewer(vec![
        Scripted::text("allow"),
        failed("rate limited"),
        Scripted::text("allow"),
    ]);
    let lines = run_flow(&mut session);
    // The whole flow in order: the failed selection's usage, then the fallback line and notice.
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
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "usage_recorded",
            "reviewer_kept",
            "notice",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
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

    let first = turn_started_seq(&lines, "never push to main");
    let second = turn_started_seq(&lines, "say hi");
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0].payload["kept"],
        json!([{"seq": first, "item": 0}, {"seq": second, "item": 0}])
    );
    assert_eq!(kept[0].payload["failed"], true);
    let notice = notice(&lines, "reviewer_selection_failed").unwrap();
    assert!(
        notice.payload["message"]
            .as_str()
            .unwrap()
            .contains("rate limited")
    );

    // The failed selection sends no re-ask: one review, one selection,
    // then the next turn's review.
    assert_eq!(reviewer.requests().len(), 3);
    let requests = reviewer.requests();
    let after_texts = texts(&requests[2]);
    assert_eq!(after_texts.len(), 6);
    assert_eq!(after_texts[0], "The person: never push to main");
    assert_eq!(after_texts[1], "The person: say hi");
    assert_eq!(after_texts[2], "The person: now run it");
    assert!(after_texts[3].starts_with("Tool call: "));
    assert_eq!(
        after_texts
            .iter()
            .filter(|text| text.starts_with("Tool call: "))
            .count(),
        1
    );
}

#[test]
fn an_unreadable_selection_is_asked_once_more() {
    // "keep all" then "1": two selection requests, and the line keeps
    // message 1.
    let tool = shell();
    let mut session = Session::with_tools(script(), None, vec![tool as Arc<dyn Tool>]);
    let reviewer = session.reviewer(vec![
        Scripted::text("allow"),
        Scripted::text("keep all"),
        Scripted::text("1"),
        Scripted::text("allow"),
    ]);
    let lines = run_flow(&mut session);
    // The whole flow in order: the re-ask costs a second selection request before the kept line.
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
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "usage_recorded",
            "usage_recorded",
            "reviewer_kept",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
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
    let requests = reviewer.requests();
    assert_eq!(requests.len(), 4);
    let second = texts(&requests[2]);
    let note = second[second.len() - 1];
    // The re-ask note is the prompt file's fixed section, not Rust text.
    assert!(note.starts_with("## handoff-reask\n"));
    assert!(note.contains("Your reply could not be read: "));
    assert!(!format!("{:?}", requests[2]).contains("keep all"));
    let seq = turn_started_seq(&lines, "never push to main");
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].payload["kept"], json!([{"seq": seq, "item": 0}]));
    assert!(kept[0].payload.get("failed").is_none());

    // "keep all" twice: two requests, then the fallback.
    let tool = shell();
    let mut session = Session::with_tools(script(), None, vec![tool as Arc<dyn Tool>]);
    let reviewer = session.reviewer(vec![
        Scripted::text("allow"),
        Scripted::text("keep all"),
        Scripted::text("keep all"),
        Scripted::text("allow"),
    ]);
    let lines = run_flow(&mut session);
    // The whole flow in order: two unreadable replies, then the fallback line and notice.
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
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "usage_recorded",
            "usage_recorded",
            "reviewer_kept",
            "notice",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
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
    assert_eq!(reviewer.requests().len(), 4);
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].payload["failed"], true);
    assert!(notice(&lines, "reviewer_selection_failed").is_some());
}

#[test]
fn none_keeps_nothing_and_a_second_handoff_with_no_person_message_asks_nothing() {
    let tool = shell();
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
            Scripted::text("Hi there."),
            Scripted::text("The first note."),
            Scripted::text("The second note."),
        ],
        None,
        vec![tool as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer(vec![Scripted::text("allow"), Scripted::text("none")]);
    prompt_turn(&mut session, "never push to main");
    prompt_turn(&mut session, "say hi");
    let mut lines = handoff_turn(&mut session, "c_first");
    lines.extend(handoff_turn(&mut session, "c_second"));
    // Both handoffs in order: the first selects and the second asks nothing.
    assert_eq!(
        kinds(&lines),
        [
            "turn_started",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "usage_recorded",
            "reviewer_kept",
            "turn_completed",
            "turn_started",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "reviewer_kept",
            "turn_completed",
        ]
    );

    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 2);
    for line in &kept {
        assert_eq!(line.payload["kept"], json!([]));
        assert!(line.payload.get("failed").is_none());
    }
    // The second handoff holds no person message, so it asks nothing.
    assert_eq!(reviewer.requests().len(), 2);
}

#[test]
fn without_a_reviewer_a_handoff_writes_no_selection() {
    let mut session = Session::with_tools(
        vec![
            Scripted::text("Hi."),
            Scripted::text("The note."),
            Scripted::text("Later."),
        ],
        None,
        Vec::new(),
    );
    prompt_turn(&mut session, "hello");
    let mut lines = handoff_turn(&mut session, "c_h");
    assert!(kept_lines(&lines).is_empty());
    assert!(notice(&lines, "reviewer_selection_failed").is_none());
    lines.extend(prompt_turn(&mut session, "after"));
    // The handoff writes no selection without a reviewer, then the prompt turn.
    assert_eq!(
        kinds(&lines),
        [
            "turn_started",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "turn_completed",
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
    assert!(kept_lines(&lines).is_empty());
}

#[test]
fn the_oldest_kept_messages_drop_past_the_reviewers_window() {
    // "The person: say hi" is 19 bytes, 5 tokens; "The person: never
    // push to main" is 31 bytes, 8 tokens. A window of 5 fits only the
    // newest.
    let window = "The person: say hi".len().div_ceil(4) as u64;
    assert_eq!(window, 5);

    // A selection of both keeps only the second, with no notice.
    let mut session = Session::with_tools(
        vec![
            Scripted::text("Noted one."),
            Scripted::text("Noted two."),
            Scripted::text("The note."),
        ],
        None,
        Vec::new(),
    );
    let reviewer = session.reviewer_windowed(vec![Scripted::text("1, 2")], window);
    let mut lines = prompt_turn(&mut session, "never push to main");
    lines.extend(prompt_turn(&mut session, "say hi"));
    lines.extend(handoff_turn(&mut session, "c_h"));
    // Two text turns, then the handoff keeping only what fits the window.
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
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "usage_recorded",
            "reviewer_kept",
            "turn_completed",
        ]
    );
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0].payload["kept"],
        json!([{"seq": turn_started_seq(&lines, "say hi"), "item": 0}])
    );
    assert!(kept[0].payload.get("failed").is_none());
    assert!(notice(&lines, "reviewer_selection_failed").is_none());
    assert_eq!(reviewer.requests().len(), 1);

    // A failed selection keeps only the second too, and the notice says
    // the oldest message was dropped to fit the window.
    let mut session = Session::with_tools(
        vec![
            Scripted::text("Noted one."),
            Scripted::text("Noted two."),
            Scripted::text("The note."),
        ],
        None,
        Vec::new(),
    );
    let reviewer = session.reviewer_windowed(vec![failed("rate limited")], window);
    let mut lines = prompt_turn(&mut session, "never push to main");
    lines.extend(prompt_turn(&mut session, "say hi"));
    lines.extend(handoff_turn(&mut session, "c_h"));
    // Two text turns, then the handoff fallback capped to the window, with its notice.
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
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "usage_recorded",
            "reviewer_kept",
            "notice",
            "turn_completed",
        ]
    );
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0].payload["kept"],
        json!([{"seq": turn_started_seq(&lines, "say hi"), "item": 0}])
    );
    assert_eq!(kept[0].payload["failed"], true);
    let message = notice(&lines, "reviewer_selection_failed").unwrap().payload["message"]
        .as_str()
        .unwrap();
    assert!(message.contains("oldest"), "{message}");
    assert!(message.contains("context window"), "{message}");
    assert!(!message.contains("every one"), "{message}");
    assert_eq!(reviewer.requests().len(), 1);
}

#[test]
fn the_selection_is_not_asked_past_the_spending_budget() {
    let tool = shell();
    let mut handoffer = TestTool::reads("handoffer", "Noted.");
    handoffer.output.control = Some(Control {
        handoff: Some("note".into()),
        ..Default::default()
    });
    // One priced reviewer call costs (10 + 3) / 1e6: the review of the
    // shell call spends past the tiny budget, so the handoff it triggers
    // falls back without a selection request.
    let mut session = Session::with_tools(
        vec![calls_reply(
            "",
            &[("shell", paris()), ("handoffer", paris())],
        )],
        None,
        vec![tool as Arc<dyn Tool>, Arc::new(handoffer) as Arc<dyn Tool>],
    )
    .budget(Some(0.00001));
    let reviewer = session.reviewer_priced(
        vec![Scripted::text("allow")],
        BlockLimits::default(),
        Some(Cost {
            input: 1.0,
            output: 1.0,
            cache_read: None,
            cache_write: None,
            tiers: Vec::new(),
        }),
    );
    session.inbox.send(delivery("go")).unwrap();
    session.turn();
    let lines = session.lines();
    // The single turn in order: the tool-triggered handoff falls back past the budget.
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
            "usage_recorded",
            "permission_resolved",
            "tool_call_started",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "handoff_completed",
            "opening_message",
            "reviewer_kept",
            "notice",
            "step_started",
            "turn_completed",
        ]
    );
    assert_eq!(reviewer.requests().len(), 1);
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].payload["failed"], true);
    assert!(notice(&lines, "reviewer_selection_failed").is_some());
}

/// The handoff selection's usage names its purpose and no call: it
/// reviewed no tool call, so `action_id` is absent.
#[test]
fn the_handoff_selection_marks_its_usage_with_no_call() {
    let tool = shell();
    let mut session = Session::with_tools(script(), None, vec![tool as Arc<dyn Tool>]);
    let _reviewer = session.reviewer(vec![
        Scripted::text("allow"),
        Scripted::text("1"),
        Scripted::text("allow"),
    ]);
    let lines = run_flow(&mut session);
    // The whole flow in order: reviewed call, text, handoff with its
    // selection, reviewed call (`docs/testing.md`, "Event streams").
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
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "usage_recorded",
            "reviewer_kept",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
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
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    let turn = kept[0].turn_id.clone();
    // The selection runs in the handoff's turn, beside the note request:
    // only the reviewer's model picks it out.
    let selection: Vec<_> = lines
        .iter()
        .filter(|line| {
            line.kind == "usage_recorded"
                && line.turn_id == turn
                && line.payload["model"] == REVIEWER_MODEL
        })
        .collect();
    assert_eq!(selection.len(), 1);
    assert_eq!(selection[0].action_id, None);
    assert_eq!(
        selection[0].payload["reviewer"],
        json!({"purpose": "handoff"})
    );
    // Every reviewer stage names the call it reviewed, and no other line
    // carries a reviewer.
    let calls: Vec<String> = lines
        .iter()
        .filter(|line| line.kind == "permission_resolved")
        .filter_map(|line| line.action_id.as_ref().map(|id| id.0.clone()))
        .collect();
    assert_eq!(calls.len(), 2);
    let mut stages = 0;
    for line in lines.iter().filter(|line| line.kind == "usage_recorded") {
        if line.payload["model"] == REVIEWER_MODEL {
            if line.turn_id == turn {
                continue;
            }
            stages += 1;
            assert_eq!(line.action_id, None);
            assert_eq!(line.payload["reviewer"]["purpose"], "stage_1");
            let named = line.payload["reviewer"]["action_id"].as_str().unwrap();
            assert!(calls.contains(&named.to_owned()), "{named}");
        } else {
            assert!(line.payload.get("reviewer").is_none());
        }
    }
    assert_eq!(stages, 2);
}
