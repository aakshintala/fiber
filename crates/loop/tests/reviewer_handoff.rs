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
use contract::shapes::Failure;
use contract::tool::Tool;
use contract::{Envelope, ErrorCode, ThinkingLevel};
use fakes::Deadline;
use fakes::{Scripted, ScriptedProvider};
use r#loop::{BlockLimits, Loop, Model, Permissions, PromptInputs, Reviewer};
use serde_json::json;

use support::{
    DEADLINE, MODEL, REVIEWER_MODEL, Session, TestTool, calls_reply, delivery, handoff, kinds,
    of_kind, paris, shell,
};

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
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => {
                panic!("not a user item: {input:?}")
            }
        })
        .collect()
}

/// The first agent request whose conversation holds the handoff's note
/// `note`: the first request after the handoff.
fn noted_request(session: &Session, note: &str) -> ModelRequest {
    session
        .requests()
        .into_iter()
        .find(|request| user_texts(request).contains(&note))
        .expect("a request after the handoff")
}

/// A request's user item texts, in order.
fn user_texts(request: &ModelRequest) -> Vec<&str> {
    request
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } => Some(text.as_str()),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
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
    let tool = shell(None, None);
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
    assert!(after_texts[3].starts_with("Call under review: the last tool call above, shell."));
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

    // The agent's first request after the handoff holds the same kept
    // message word for word, between the opening and the note, and not the
    // dropped one.
    let first = noted_request(&session, "The note.");
    let agent = user_texts(&first);
    assert!(agent[0].starts_with("This message is from Fiber"));
    assert_eq!(
        agent[1..],
        ["never push to main", "The note.", "now run it"]
    );
}

#[test]
fn selection_prose_never_reaches_a_later_prompt() {
    let tool = shell(None, None);
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
    let tool = shell(None, None);
    let mut session_a = Session::with_tools(script(), None, vec![tool as Arc<dyn Tool>]);
    let reviewer_a = session_a.reviewer(vec![
        Scripted::text("allow"),
        Scripted::text("1"),
        Scripted::text("allow"),
    ]);
    run_flow(&mut session_a);

    // Run B stops after the handoff turn, drops the loop, resumes from the
    // log with a reviewer answering what is left, then runs the last turn.
    let tool = shell(None, None);
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
    let tool = shell(None, None);
    let reviewer_b = Arc::new(ScriptedProvider::new(vec![Scripted::text("allow")]));
    let mut looped = Loop::resume(
        r#loop::Session {
            log: Arc::clone(&session_b.log),
            provider: Arc::clone(&session_b.provider) as Arc<dyn Provider>,
            model: Model {
                reference: MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: vec![("builtin".into(), tool as Arc<dyn Tool>)],
            permissions: Permissions {
                workspace: session_b.workspace.display().to_string(),
                credentials: session_b.credentials.clone(),
                credential_files: Vec::new(),
                rules,
            },
        },
        resumed,
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
        Deadline::after(DEADLINE)
            .recv(&finished)
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
                | Input::ToolResult { .. }
                | Input::ToolSearchResult { .. } => {
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
    let tool = shell(None, None);
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
    assert_eq!(after_texts.len(), 7);
    assert_eq!(after_texts[0], "The person: never push to main");
    assert_eq!(after_texts[1], "The agent: Done.");
    assert_eq!(after_texts[2], "The person: say hi");
    assert_eq!(after_texts[3], "The person: now run it");
    assert!(after_texts[4].starts_with("Tool call: "));
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
    let tool = shell(None, None);
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
    let tool = shell(None, None);
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
    let tool = shell(None, None);
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
    // Nothing earlier is carried without a selection: the first request
    // after the handoff holds the opening, the note and this turn's input.
    let first = noted_request(&session, "The note.");
    let agent = user_texts(&first);
    assert!(agent[0].starts_with("This message is from Fiber"));
    assert_eq!(agent[1..], ["The note.", "after"]);
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
    // "The person: never push to main" is 31 bytes, 8 tokens, and "The
    // person: say hi" is 19 bytes, 5 tokens, answered by "The agent: Noted
    // one." at 21 bytes, 6 tokens: the reply counts against the message
    // it answers. A window of 11 fits only the newest with its reply.
    let window = "The person: say hi".len().div_ceil(4) as u64
        + "The agent: Noted one.".len().div_ceil(4) as u64;
    assert_eq!(window, 11);

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
fn a_reply_counts_against_the_message_it_answers_window_share() {
    // "The person: never push to main" is 8 tokens and "The person: say
    // hi" is 5; the reply "The agent: Noted one." is 6. A window of 13
    // fits both messages without the reply, but with it the oldest drops.
    let window = 13;
    let replies = vec![
        Scripted::text("Noted one."),
        Scripted::text("Noted two."),
        Scripted::text("The note."),
    ];
    let mut session = Session::with_tools(replies, None, Vec::new());
    let reviewer = session.reviewer_windowed(vec![Scripted::text("1, 2")], window);
    let mut lines = prompt_turn(&mut session, "never push to main");
    lines.extend(prompt_turn(&mut session, "say hi"));
    lines.extend(handoff_turn(&mut session, "c_h"));
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0].payload["kept"],
        json!([{"seq": turn_started_seq(&lines, "say hi"), "item": 0}])
    );
    assert!(kept[0].payload.get("failed").is_none());
    assert_eq!(reviewer.requests().len(), 1);
    // The selection saw the reply above the listing: it is what made the
    // oldest message exceed the window.
    let selected = reviewer.requests();
    let seen = texts(&selected[0]);
    assert!(seen.contains(&"The agent: Noted one."));
    assert_eq!(
        seen.iter()
            .position(|text| text == &"The agent: Noted one.")
            .unwrap()
            + 1,
        seen.iter()
            .position(|text| text == &"The person: say hi")
            .unwrap(),
        "the reply sits directly before its message: {seen:?}"
    );

    // The same flow with empty agent text keeps both: no reply, no share.
    let mut session = Session::with_tools(
        vec![
            Scripted::text(""),
            Scripted::text(""),
            Scripted::text("The note."),
        ],
        None,
        Vec::new(),
    );
    let reviewer = session.reviewer_windowed(vec![Scripted::text("1, 2")], window);
    let mut lines = prompt_turn(&mut session, "never push to main");
    lines.extend(prompt_turn(&mut session, "say hi"));
    lines.extend(handoff_turn(&mut session, "c_h"));
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0].payload["kept"],
        json!([
            {"seq": turn_started_seq(&lines, "never push to main"), "item": 0},
            {"seq": turn_started_seq(&lines, "say hi"), "item": 0}
        ])
    );
    assert!(kept[0].payload.get("failed").is_none());
    assert_eq!(reviewer.requests().len(), 1);
    assert!(
        !texts(&reviewer.requests()[0])
            .iter()
            .any(|text| text.starts_with("The agent:")),
        "no reply reaches the selection"
    );
}

#[test]
fn the_reviewer_sees_the_reply_a_person_message_answers() {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            Scripted::text("Delete the 3 stale branches?"),
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer(vec![Scripted::text("allow")]);
    prompt_turn(&mut session, "go");
    prompt_turn(&mut session, "yes");
    let requests = reviewer.requests();
    assert_eq!(requests.len(), 1);
    let seen = texts(&requests[0]);
    // The reply the person's "yes" answers reaches the reviewer marked
    // as the agent's, directly before the message.
    assert_eq!(seen.len(), 6);
    assert_eq!(seen[0], "The person: go");
    assert_eq!(seen[1], "The agent: Delete the 3 stale branches?");
    assert_eq!(seen[2], "The person: yes");
    assert!(seen[3].starts_with("Tool call: "));
    assert!(seen[4].starts_with("Call under review: the last tool call above, shell."));
    assert!(seen[5].starts_with("## first-pass\n"));
    let at = seen
        .iter()
        .position(|text| text == &"The person: yes")
        .unwrap();
    assert_eq!(seen[at - 1], "The agent: Delete the 3 stale branches?");
}

#[test]
fn a_tool_call_between_the_text_and_the_message_hides_the_reply() {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("The plan is ready.", &[("shell", paris())]),
            Scripted::text(""),
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer(vec![Scripted::text("allow"), Scripted::text("allow")]);
    prompt_turn(&mut session, "go");
    prompt_turn(&mut session, "yes");
    let requests = reviewer.requests();
    assert_eq!(requests.len(), 2);
    // The call between the agent's text and "yes" breaks the link: the
    // second review holds the message with no agent reply before it.
    let seen = texts(&requests[1]);
    assert!(seen.contains(&"The person: yes"));
    assert!(
        !seen.iter().any(|text| text.starts_with("The agent:")),
        "no reply survives the tool call: {seen:?}"
    );
}

#[test]
fn a_handoff_keeps_the_pair_and_drops_the_calls() {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            Scripted::text("Delete the 3 stale branches?"),
            Scripted::text("On it."),
            Scripted::text("The note."),
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer(vec![Scripted::text("1, 2"), Scripted::text("allow")]);
    let mut lines = prompt_turn(&mut session, "go");
    lines.extend(prompt_turn(&mut session, "yes"));
    lines.extend(handoff_turn(&mut session, "c_h"));
    lines.extend(prompt_turn(&mut session, "now run it"));
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0].payload["kept"],
        json!([
            {"seq": turn_started_seq(&lines, "go"), "item": 0},
            {"seq": turn_started_seq(&lines, "yes"), "item": 0}
        ])
    );
    let requests = reviewer.requests();
    assert_eq!(requests.len(), 2);
    // The selection holds the kept pair with the reply directly before
    // its message, and no pre-handoff call.
    let selecting = texts(&requests[0]);
    let at = selecting
        .iter()
        .position(|text| text == &"The person: yes")
        .unwrap();
    assert_eq!(selecting[at - 1], "The agent: Delete the 3 stale branches?");
    assert!(selecting.contains(&"The person: go"));
    assert!(
        !selecting.iter().any(|text| text.starts_with("Tool call: ")),
        "no pre-handoff call reaches the selection: {selecting:?}"
    );
    // The post-handoff review holds the kept pair too, then the new call
    // as the only tool call.
    let after = texts(&requests[1]);
    let at = after
        .iter()
        .position(|text| text == &"The person: yes")
        .unwrap();
    assert_eq!(after[at - 1], "The agent: Delete the 3 stale branches?");
    assert!(after.contains(&"The person: go"));
    assert!(after.contains(&"The person: now run it"));
    assert_eq!(
        after
            .iter()
            .filter(|text| text.starts_with("Tool call: "))
            .count(),
        1
    );
}

#[test]
fn the_selection_is_not_asked_past_the_spending_budget() {
    let tool = shell(None, None);
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
    let tool = shell(None, None);
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

#[test]
fn a_handoff_with_session_start_context_and_a_reviewer_completes() {
    use support::{StartHooks, start_added, start_empty};

    fn hook(hooks: Arc<StartHooks>, session: &mut Session) {
        session.looped = session
            .looped
            .take()
            .map(|looped| looped.hooks(hooks as Arc<dyn contract::hook::Hooks>));
    }

    // A person's handoff during a turn with input, judged by a
    // reviewer: the selection runs after the handoff's guard is
    // released, so the handoff completes instead of deadlocking.
    let mut session = support::Session::new(
        vec![
            Scripted::text("Done."),
            Scripted::text("The note."),
            Scripted::text("After."),
        ],
        None,
    );
    let reviewer = session.reviewer(vec![Scripted::text("1")]);
    let hooks = StartHooks::new();
    hooks.answer(start_empty());
    hooks.answer(start_added("fiber.test/inbox", "at handoff"));
    hook(Arc::clone(&hooks), &mut session);
    session.inbox.send(delivery("hi")).unwrap();
    session.inbox.send(handoff("c_h", None)).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    // The hook's context follows the new opening message, and the
    // reviewer selection runs after it.
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
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
            "context_added",
            "usage_recorded",
            "reviewer_kept",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(
        of_kind(&lines, "context_added")[0].payload["text"],
        "at handoff"
    );
    assert_eq!(reviewer.requests().len(), 1);
    drop(session.looped.take());
    drop(reviewer);
}

#[test]
fn kept_messages_over_the_trigger_are_carried_whole_and_hand_off_once() {
    // 8,000 bytes are 2,000 tokens: the kept message alone is over a
    // trigger of 1,000.
    let long = format!("never push to main. {}", "x".repeat(8_000));
    let trigger = 1_000;
    assert!(long.len() as u64 / 4 > trigger);
    let mut session = Session::with_tools(
        vec![
            support::with_tokens(Scripted::text("Hi."), trigger, 0),
            Scripted::text("The note."),
            support::with_tokens(Scripted::text("Done."), 50, 0),
        ],
        None,
        Vec::new(),
    )
    .handoff(r#loop::HandoffSettings {
        tokens: trigger,
        nudge: false,
        ..r#loop::HandoffSettings::default()
    });
    session.reviewer(vec![Scripted::text("1, 2")]);
    prompt_turn(&mut session, &long);
    let lines = prompt_turn(&mut session, "second");
    // The second turn starts over the trigger: one handoff, and the
    // context it starts is over the trigger too, which does not hand off again
    // in that step.
    assert_eq!(of_kind(&lines, "handoff_started").len(), 1);
    assert_eq!(of_kind(&lines, "handoff_completed").len(), 1);
    let kept = kept_lines(&lines);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].payload["kept"].as_array().unwrap().len(), 2);
    let first = noted_request(&session, "The note.");
    let agent = user_texts(&first);
    assert!(agent[0].starts_with("This message is from Fiber"));
    // The kept message is carried whole, before the turn's input and the
    // note.
    assert_eq!(agent[2..], ["second", "The note."]);
    assert!(
        agent[1] == long,
        "the kept message is carried whole: {} bytes of {}",
        agent[1].len(),
        long.len()
    );
    assert_eq!(session.requests().len(), 3);
}
