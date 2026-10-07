//! A call's `control.questions` ends the turn at the step boundary
//! (`docs/tools.md`, "What a result carries", "When a program drives the
//! session"), through the loop's public API with test tools registered
//! through the tool seam. The loop acts on the field, never on the tool's
//! name: none of these tools is `ask_user`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::sync::Arc;

use contract::events::{Control, TurnOutcome};
use contract::shapes::{Effect, Question};
use contract::tool::Tool;
use contract::{Envelope, ErrorCode};
use fakes::Scripted;
use serde_json::{Value, json};

use support::{Gate, Script, Session, Tap, TestTool, calls_reply, delivery, kinds};

fn paris() -> Value {
    json!({"city": "Paris"})
}

fn question(text: &str) -> Question {
    Question {
        header: "h".into(),
        question: text.into(),
        options: Vec::new(),
        multi_select: None,
    }
}

/// A tool whose calls return `control.questions` holding `texts`.
fn asking(name: &'static str, texts: &[&str]) -> TestTool {
    let mut tool = TestTool::reads(name, "Asked.");
    tool.output.control = Some(Control {
        handoff: None,
        questions: Some(texts.iter().map(|text| question(text)).collect()),
    });
    tool
}

/// A tool whose calls return `control.handoff`.
fn handing_off(name: &'static str) -> TestTool {
    let mut tool = TestTool::reads(name, "Handed off.");
    tool.output.control = Some(Control {
        handoff: Some("the note".into()),
        questions: None,
    });
    tool
}

/// A tool that reads a file in `sub/`, whose `AGENTS.md` it queues.
fn scanning() -> TestTool {
    TestTool::declaring(
        "scan",
        "Scanned.",
        vec![Effect::Reads],
        Some(vec!["sub/notes.txt".into()]),
    )
}

/// A session whose first reply calls `names` and whose second says "Done.".
fn session(tools: Vec<Arc<dyn Tool>>, names: &[&str]) -> Session {
    let calls: Vec<(&str, Value)> = names.iter().map(|name| (*name, paris())).collect();
    let session = Session::with_tools(
        vec![calls_reply("", &calls), Scripted::text("Done.")],
        None,
        tools,
    );
    session.inbox.send(delivery("go")).unwrap();
    session
}

/// Writes `sub/AGENTS.md` below the workspace, which the opening message
/// never reads.
fn subdirectory(session: &Session) {
    std::fs::create_dir_all(session.workspace.join("sub")).unwrap();
    std::fs::write(session.workspace.join("sub/AGENTS.md"), "Sub rules.\n").unwrap();
}

/// The payload of the last line, which is `turn_completed`.
fn turn_completed(lines: &[Envelope]) -> Value {
    let last = lines.last().unwrap();
    assert_eq!(last.kind, "turn_completed");
    Value::Object(last.payload.clone())
}

fn asked(texts: &[&str]) -> Value {
    serde_json::to_value(texts.iter().map(|text| question(text)).collect::<Vec<_>>()).unwrap()
}

/// The kinds of one step whose reply makes `calls` calls, through its
/// `assistant_message_completed`.
fn step_of(calls: usize) -> Vec<&'static str> {
    let mut kinds = vec![
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
    ];
    kinds.extend(std::iter::repeat_n("tool_call_arguments_delta", calls));
    kinds.extend(std::iter::repeat_n("tool_call_requested", calls));
    kinds.extend(["usage_recorded", "assistant_message_completed"]);
    kinds
}

#[test]
fn a_call_that_asks_ends_the_turn_with_its_questions() {
    let mut session = session(vec![Arc::new(asking("ask", &["Which?"]))], &["ask"]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(1));
    expected.extend(["tool_call_started", "tool_call_completed", "turn_completed"]);
    assert_eq!(kinds(&lines), expected);
    assert_eq!(
        turn_completed(&lines),
        json!({"outcome": "completed", "questions": asked(&["Which?"])})
    );
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn questions_follow_call_order_not_completion_order() {
    let trace = Arc::default();
    let mut first = asking("first", &["One?", "Two?"]);
    first.after = Some("second");
    first.trace = Arc::clone(&trace);
    let mut second = asking("second", &["Three?"]);
    second.trace = Arc::clone(&trace);
    let mut session = session(
        vec![Arc::new(first), Arc::new(second)],
        &["first", "second"],
    );
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(2));
    expected.extend([
        "tool_call_started",
        "tool_call_started",
        "tool_call_completed",
        "tool_call_completed",
        "turn_completed",
    ]);
    assert_eq!(kinds(&lines), expected);
    let trace = trace.lock().unwrap().clone();
    assert!(
        trace.iter().position(|t| t == "done second")
            < trace.iter().position(|t| t == "done first"),
        "{trace:?}"
    );
    assert_eq!(
        turn_completed(&lines)["questions"],
        asked(&["One?", "Two?", "Three?"])
    );
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn a_call_beside_the_asking_one_still_completes() {
    let mut session = session(
        vec![
            Arc::new(TestTool::reads("weather", "Sunny.")),
            Arc::new(asking("ask", &["Which?"])),
        ],
        &["weather", "ask"],
    );
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(2));
    expected.extend([
        "tool_call_started",
        "tool_call_started",
        "tool_call_completed",
        "tool_call_completed",
        "turn_completed",
    ]);
    assert_eq!(kinds(&lines), expected);
    let results: Vec<&Value> = lines
        .iter()
        .filter(|line| line.kind == "tool_call_completed")
        .map(|line| &line.payload["content"][0]["text"])
        .collect();
    assert_eq!(results, ["Sunny.", "Asked."]);
    assert_eq!(turn_completed(&lines)["questions"], asked(&["Which?"]));
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn a_failed_call_asks_nothing() {
    let mut tool = asking("ask", &["Which?"]);
    tool.output.error = TestTool::failing("x", ErrorCode::ToolError).output.error;
    let mut session = session(vec![Arc::new(tool)], &["ask"]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(1));
    expected.extend([
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
    ]);
    assert_eq!(kinds(&lines), expected);
    let done: Vec<&Envelope> = lines
        .iter()
        .filter(|line| line.kind == "tool_call_completed")
        .collect();
    assert_eq!(done[0].payload["status"], "failed");
    assert_eq!(turn_completed(&lines), json!({"outcome": "completed"}));
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn an_empty_list_of_questions_asks_nothing() {
    let mut session = session(vec![Arc::new(asking("ask", &[]))], &["ask"]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(1));
    expected.extend([
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
    ]);
    assert_eq!(kinds(&lines), expected);
    assert_eq!(turn_completed(&lines), json!({"outcome": "completed"}));
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn a_handoff_in_the_same_step_applies_before_the_questions_end_the_turn() {
    let mut session = session(
        vec![
            Arc::new(handing_off("hand")),
            Arc::new(asking("ask", &["Which?"])),
        ],
        &["hand", "ask"],
    );
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(2));
    expected.extend([
        "tool_call_started",
        "tool_call_started",
        "tool_call_completed",
        "tool_call_completed",
        "handoff_completed",
        "opening_message",
        "turn_completed",
    ]);
    assert_eq!(kinds(&lines), expected);
    let at = |kind: &str| lines.iter().position(|line| line.kind == kind).unwrap();
    assert!(at("handoff_completed") < at("turn_completed"));
    assert_eq!(turn_completed(&lines)["questions"], asked(&["Which?"]));
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn the_queued_subdirectory_lines_are_written_before_the_turn_ends() {
    let mut session = session(
        vec![Arc::new(scanning()), Arc::new(asking("ask", &["Which?"]))],
        &["scan", "ask"],
    );
    subdirectory(&session);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(2));
    expected.extend([
        "tool_call_started",
        "tool_call_started",
        "tool_call_completed",
        "tool_call_completed",
        "instruction_file",
        "turn_completed",
    ]);
    assert_eq!(kinds(&lines), expected);
    let file = lines
        .iter()
        .find(|line| line.kind == "instruction_file")
        .unwrap();
    assert!(
        file.payload["path"]
            .as_str()
            .unwrap()
            .ends_with("AGENTS.md"),
        "{:?}",
        file.payload
    );
    assert_eq!(turn_completed(&lines)["questions"], asked(&["Which?"]));
}

#[test]
fn a_cancel_that_ends_the_step_drops_the_questions() {
    let gate = Arc::new(Gate::default());
    let mut tool = asking("ask", &["Which?"]);
    tool.script = vec![Script::Wait(Arc::clone(&gate))];
    let mut session = session(vec![Arc::new(tool)], &["ask"]);
    let tap = Tap::new(&session.log);
    let cancel = Arc::clone(&session.cancel);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        tap.wait_for("tool_call_started");
        assert!(cancel.cancel());
        gate.open();
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Interrupted));
    });
    gate.check("ask");
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(1));
    expected.extend(["tool_call_started", "tool_call_completed", "turn_completed"]);
    assert_eq!(kinds(&lines), expected);
    assert_eq!(turn_completed(&lines), json!({"outcome": "interrupted"}));
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn a_handoff_the_questions_and_a_queued_line_end_one_step_in_order() {
    let mut session = session(
        vec![
            Arc::new(handing_off("hand")),
            Arc::new(asking("ask", &["Which?"])),
            Arc::new(scanning()),
        ],
        &["hand", "ask", "scan"],
    );
    subdirectory(&session);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(3));
    expected.extend([
        "tool_call_started",
        "tool_call_started",
        "tool_call_started",
        "tool_call_completed",
        "tool_call_completed",
        "tool_call_completed",
        "handoff_completed",
        "opening_message",
        "instruction_file",
        "turn_completed",
    ]);
    assert_eq!(kinds(&lines), expected);
    assert_eq!(turn_completed(&lines)["questions"], asked(&["Which?"]));
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn a_cancel_after_the_questions_are_collected_ends_the_turn_interrupted() {
    let session = session(
        vec![
            Arc::new(handing_off("hand")),
            Arc::new(asking("ask", &["Which?"])),
        ],
        &["hand", "ask"],
    );
    // The handoff's restart runs after the questions are collected and
    // before the turn ends: a cancel there is the latest one can land.
    let cancel = Arc::clone(&session.cancel);
    let mut session = session.on_handoff(Arc::new(move || {
        cancel.cancel();
    }));
    assert_eq!(session.turn(), Some(TurnOutcome::Interrupted));
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(2));
    expected.extend([
        "tool_call_started",
        "tool_call_started",
        "tool_call_completed",
        "tool_call_completed",
        "handoff_completed",
        "opening_message",
        "turn_completed",
    ]);
    assert_eq!(kinds(&lines), expected);
    assert!(lines.iter().any(|line| line.kind == "handoff_completed"));
    assert_eq!(turn_completed(&lines), json!({"outcome": "interrupted"}));
    assert_eq!(session.requests().len(), 1);
}
