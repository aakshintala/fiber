//! A call's `control.name` writes `session_named` before its completion
//! (`docs/tools.md`, "What a result carries"), through the loop's public
//! API with test tools registered through the tool seam. The loop acts on
//! the field, never on the tool's name: none of these tools is
//! `name_session`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::sync::{Arc, mpsc};

use contract::events::{Control, TurnOutcome};
use contract::tool::Tool;
use contract::{Envelope, ErrorCode};
use fakes::{Deadline, Scripted};
use serde_json::{Value, json};

use support::{
    DEADLINE, Script, Session, Tap, TestTool, calls_reply, delivery, kinds, name_reported, paris,
};

/// A tool whose calls return `control.name` holding `name`.
fn naming(name: &'static str, text: &'static str) -> TestTool {
    let mut tool = TestTool::reads(name, text);
    tool.output.control = Some(Control {
        handoff: None,
        questions: None,
        skill: None,
        name: Some("Fix the parser".into()),
    });
    tool
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

fn head() -> Vec<&'static str> {
    vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
    ]
}

/// The step after the calls' step: the model says "Done.".
fn tail() -> Vec<&'static str> {
    vec![
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]
}

#[test]
fn a_call_naming_the_session_writes_its_name_before_its_completion() {
    let mut session = session(vec![Arc::new(naming("name", "Named."))], &["name"]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = head();
    expected.extend(step_of(1));
    expected.extend(["tool_call_started", "session_named", "tool_call_completed"]);
    expected.extend(tail());
    assert_eq!(kinds(&lines), expected);
    let at = |kind: &str| lines.iter().position(|l| l.kind == kind).unwrap();
    let named = &lines[at("session_named")];
    let completed = lines
        .iter()
        .find(|l| l.kind == "tool_call_completed")
        .unwrap();
    assert_eq!(named.payload["name"], "Fix the parser");
    assert_eq!(named.payload["by"], "model");
    assert_eq!(named.action_id, completed.action_id);
    assert_eq!(completed.payload["status"], "completed");
    assert_eq!(completed.payload["control"]["name"], "Fix the parser");
}

#[test]
fn a_naming_call_with_job_lines_writes_its_name_last() {
    let mut tool = naming("name", "Named.");
    tool.output.jobs = vec![
        contract::jobs::JobRecord::Started(contract::events::JobStarted {
            job_id: contract::JobId("j_1".into()),
            tool: None,
            extension: None,
            description: "a job".into(),
            output_path: "artifacts/j_1.out".into(),
        }),
        contract::jobs::JobRecord::Completed(contract::events::JobCompleted {
            job_id: contract::JobId("j_1".into()),
            status: contract::events::Outcome::Completed,
            error: None,
            process: None,
            output_tail: None,

            notice: None,
            changed_by: None,
            protected_paths: None,
        }),
    ];
    let mut session = session(vec![Arc::new(tool)], &["name"]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = head();
    expected.extend(step_of(1));
    expected.extend([
        "tool_call_started",
        "job_started",
        "job_completed",
        "session_named",
        "tool_call_completed",
    ]);
    expected.extend(tail());
    assert_eq!(kinds(&lines), expected);
}

#[test]
fn two_naming_calls_write_in_request_order() {
    let trace = Arc::default();
    let mut first = naming("first", "First.");
    first.output.control = Some(Control {
        handoff: None,
        questions: None,
        skill: None,
        name: Some("A".into()),
    });
    first.after = Some("second");
    first.trace = Arc::clone(&trace);
    let mut second = naming("second", "Second.");
    second.output.control = Some(Control {
        handoff: None,
        questions: None,
        skill: None,
        name: Some("B".into()),
    });
    second.trace = Arc::clone(&trace);
    let mut session = session(
        vec![Arc::new(first), Arc::new(second)],
        &["first", "second"],
    );
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = head();
    expected.extend(step_of(2));
    expected.extend([
        "tool_call_started",
        "tool_call_started",
        "session_named",
        "tool_call_completed",
        "session_named",
        "tool_call_completed",
    ]);
    expected.extend(tail());
    assert_eq!(kinds(&lines), expected);
    let trace = trace.lock().unwrap().clone();
    assert!(
        trace.iter().position(|t| t == "done second")
            < trace.iter().position(|t| t == "done first"),
        "{trace:?}"
    );
    let named: Vec<&Envelope> = lines.iter().filter(|l| l.kind == "session_named").collect();
    assert_eq!(named.len(), 2);
    assert_eq!(named[0].payload["name"], "A");
    assert_eq!(named[1].payload["name"], "B");
    let completed: Vec<&Envelope> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .collect();
    assert_eq!(named[0].action_id, completed[0].action_id);
    assert_eq!(named[1].action_id, completed[1].action_id);
}

#[test]
fn two_steps_name_twice_in_order() {
    let tool = Arc::new(naming("name", "Named."));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("name", paris())]),
            calls_reply("", &[("name", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool],
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = head();
    expected.extend(step_of(1));
    expected.extend(["tool_call_started", "session_named", "tool_call_completed"]);
    expected.extend(step_of(1));
    expected.extend(["tool_call_started", "session_named", "tool_call_completed"]);
    expected.extend(tail());
    assert_eq!(kinds(&lines), expected);
    let named: Vec<&Envelope> = lines.iter().filter(|l| l.kind == "session_named").collect();
    assert_eq!(named.len(), 2);
}

#[test]
fn a_failed_call_names_nothing() {
    let mut tool = TestTool::failing("name", ErrorCode::ToolError);
    tool.output.control = Some(Control {
        handoff: None,
        questions: None,
        skill: None,
        name: Some("Fix the parser".into()),
    });
    let mut session = session(vec![Arc::new(tool)], &["name"]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = head();
    expected.extend(step_of(1));
    expected.extend(["tool_call_started", "tool_call_completed"]);
    expected.extend(tail());
    assert_eq!(kinds(&lines), expected);
    let completed = lines
        .iter()
        .find(|l| l.kind == "tool_call_completed")
        .unwrap();
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "tool_error");
}

#[test]
fn a_cancelled_call_names_nothing() {
    let mut tool = naming("slow", "Slow done.");
    tool.output.control = Some(Control {
        handoff: None,
        questions: None,
        skill: None,
        name: Some("Fix the parser".into()),
    });
    tool.script = vec![Script::WaitCancel];
    let tool = Arc::new(tool);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("slow", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    session.inbox.send(delivery("hi")).unwrap();
    let tap = Tap::new(&session.log);
    let cancel = Arc::clone(&session.cancel);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| session.turn());
        tap.wait_for("tool_call_started");
        assert!(cancel.cancel());
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Interrupted));
    });
    let lines = session.lines();
    let mut expected = head();
    expected.extend(step_of(1));
    expected.extend(["tool_call_started", "tool_call_completed", "turn_completed"]);
    assert_eq!(kinds(&lines), expected);
    let completed = lines
        .iter()
        .find(|l| l.kind == "tool_call_completed")
        .unwrap();
    assert_eq!(completed.payload["status"], "cancelled");
}

#[test]
fn a_handoff_call_names_nothing() {
    let mut tool = TestTool::reads("hand", "Handed off.");
    tool.output.control = Some(Control {
        handoff: Some("the note".into()),
        questions: None,
        skill: None,
        name: None,
    });
    let mut session = session(vec![Arc::new(tool)], &["hand"]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = head();
    expected.extend(step_of(1));
    expected.extend(["tool_call_started", "tool_call_completed"]);
    expected.extend(["handoff_completed", "opening_message"]);
    expected.extend(tail());
    assert_eq!(kinds(&lines), expected);
    assert_eq!(
        lines
            .iter()
            .find(|l| l.kind == "tool_call_completed")
            .unwrap()
            .payload["control"],
        json!({"handoff": "the note"})
    );
}

/// A session naming `names` whose inbox first takes the person's `name`:
/// the name is written at the idle drain, before the turn starts.
fn session_with_person_name(person: Vec<(&str, mpsc::Sender<contract::inbox::Answer>)>) -> Session {
    let calls: Vec<(&str, Value)> = ["name"].iter().map(|name| (*name, paris())).collect();
    let session = Session::with_tools(
        vec![calls_reply("", &calls), Scripted::text("Done.")],
        None,
        vec![Arc::new(naming("name", "Named."))],
    );
    for (text, tx) in person {
        session.inbox.send(name_reported(text, tx)).unwrap();
    }
    session.inbox.send(delivery("go")).unwrap();
    session
}

#[test]
fn a_person_name_pins_the_session_against_a_naming_call() {
    let (tx, rx) = mpsc::channel();
    let mut session = session_with_person_name(vec![("Mine", tx)]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let answer = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the name is answered");
    assert!(
        matches!(answer, Ok(None)),
        "the person's name accepts its ack with no answer"
    );
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "session_named",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(1));
    expected.extend(["tool_call_started", "tool_call_completed"]);
    expected.extend(tail());
    assert_eq!(kinds(&lines), expected);
    let named = lines
        .iter()
        .find(|line| line.kind == "session_named")
        .unwrap();
    assert_eq!(named.payload["name"], "Mine");
    assert_eq!(named.payload["by"], "person");
    assert!(
        named.turn_id.is_none(),
        "the person's session_named carries no turn"
    );
    assert!(
        named.action_id.is_none(),
        "the person's session_named carries no action"
    );
    let completed = lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap();
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "name_pinned");
    assert_eq!(
        completed.payload["error"]["message"],
        "the person named this session"
    );
}

#[test]
fn a_cleared_person_name_lets_a_naming_call_name_the_session() {
    let (mine_tx, mine_rx) = mpsc::channel();
    let (clear_tx, clear_rx) = mpsc::channel();
    let mut session = session_with_person_name(vec![("Mine", mine_tx), ("", clear_tx)]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let mine = Deadline::after(DEADLINE)
        .recv(&mine_rx)
        .expect("the name is answered");
    assert!(
        matches!(mine, Ok(None)),
        "the person's name accepts its ack with no answer"
    );
    let cleared = Deadline::after(DEADLINE)
        .recv(&clear_rx)
        .expect("the clear is answered");
    assert!(
        matches!(cleared, Ok(None)),
        "the clear accepts its ack with no answer"
    );
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "session_named",
        "session_named",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step_of(1));
    expected.extend(["tool_call_started", "session_named", "tool_call_completed"]);
    expected.extend(tail());
    assert_eq!(kinds(&lines), expected);
    let named: Vec<&Envelope> = lines
        .iter()
        .filter(|line| line.kind == "session_named")
        .collect();
    assert_eq!(named.len(), 3);
    assert_eq!(named[0].payload["name"], "Mine");
    assert_eq!(named[0].payload["by"], "person");
    assert!(
        named[1].payload["name"].is_null(),
        "a clear writes name null"
    );
    assert_eq!(named[1].payload["by"], "person");
    assert_eq!(named[2].payload["name"], "Fix the parser");
    assert_eq!(named[2].payload["by"], "model");
    let completed = lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap();
    assert_eq!(completed.payload["status"], "completed");
}
