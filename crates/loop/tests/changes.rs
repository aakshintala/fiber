//! Instruction files changing mid-session (`docs/system-prompt.md`, "When
//! something changes", and "The date"): the turn-start check, the own-edit
//! tracking, the subdirectory files, the date line, and the resume fold,
//! through whole turns.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::emit::Emit;
use contract::events::TurnOutcome;
use contract::inbox::Delivery;
use contract::provider::{Input, Provider};
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use contract::{Envelope, SessionId};
use fakes::{Scripted, ScriptedProvider};
use log::Log;
use r#loop::{Loop, Model};
use serde_json::{Map, Value, json};

use support::{Session, calls_reply, delivery, kinds};

/// A tool whose calls rewrite the workspace's `AGENTS.md`. The workspace
/// only exists once the session does, so it is set after `with_tools`.
struct WriteAgents {
    workspace: Mutex<PathBuf>,
}

impl Tool for WriteAgents {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "write_agents".into(),
            description: "The test write tool.".into(),
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
                effects: vec![Effect::Writes],
                reversible: true,
                paths: Some(vec!["AGENTS.md".into()]),
            },
            subject: Some(String::new()),
            prefix: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        std::fs::write(
            self.workspace.lock().unwrap().join("AGENTS.md"),
            "Revised by the call.\n",
        )
        .unwrap();
        Output {
            content: vec![ContentPart::Text {
                text: "Wrote it.".into(),
            }],
            ..Output::default()
        }
    }
}

fn city() -> Value {
    json!({"city": "Paris"})
}

/// A tool whose calls rewrite `<home>/AGENTS.md`, outside the workspace.
/// The declared path is the absolute path the opening message records:
/// the canonical home joined with `AGENTS.md`.
struct WriteHome {
    home: Mutex<PathBuf>,
}

impl WriteHome {
    fn target(&self) -> PathBuf {
        self.home
            .lock()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join("AGENTS.md")
    }
}

impl Tool for WriteHome {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "write_home".into(),
            description: "The test home write tool.".into(),
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
                effects: vec![Effect::Writes],
                reversible: true,
                paths: Some(vec![self.target().display().to_string()]),
            },
            subject: Some(String::new()),
            prefix: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        std::fs::write(self.target(), "Global, revised.\n").unwrap();
        Output {
            content: vec![ContentPart::Text {
                text: "Wrote it.".into(),
            }],
            ..Output::default()
        }
    }
}

fn users(conversation: &[Input]) -> usize {
    conversation
        .iter()
        .filter(|input| matches!(input, Input::User { .. }))
        .count()
}

/// A first turn answering with text: session setup, the opening message,
/// one step with a two-delta reply, and the turn's end.
fn first_text_turn() -> Vec<&'static str> {
    vec![
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
}

/// A later turn answering with text and no changes.
fn later_text_turn() -> Vec<&'static str> {
    vec![
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
}

#[test]
fn a_first_turn_has_no_change() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    // In the opening message: sent, not a change.
    std::fs::write(session.workspace.join("AGENTS.md"), "Leaf.\n").unwrap();
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    // The complete turn, in order: nothing changed, so no
    // `instruction_file` and no `date_changed`.
    assert_eq!(kinds(&session.lines()), first_text_turn());
}

#[test]
fn a_new_instruction_file_is_created_at_the_next_turn_start() {
    let mut session = Session::new(vec![Scripted::text("One."), Scripted::text("Two.")], None);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    assert_eq!(kinds(&session.lines()), first_text_turn());
    std::fs::write(session.workspace.join("AGENTS.md"), "Leaf.\n").unwrap();
    session.inbox.send(delivery("again")).unwrap();
    session.turn();
    let lines = session.lines();
    // After the first turn's opening message, before the turn starts.
    let mut created = vec!["instruction_file"];
    created.extend(later_text_turn());
    assert_eq!(kinds(&lines), created);
    let created = lines
        .iter()
        .find(|line| line.kind == "instruction_file")
        .unwrap();
    assert_eq!(created.payload["reason"], "created");
    assert_eq!(created.payload["sent"], "full");
    assert_eq!(created.payload["content"], "Leaf.\n");
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].conversation.iter().any(
            |input| matches!(input, Input::User { text } if text.contains("a new instruction file appeared"))
        ),
        "{:?}",
        requests[1].conversation
    );
}

#[test]
fn a_changed_file_sends_a_diff_then_a_deletion() {
    let old = (0..100).map(|n| format!("line {n}\n")).collect::<String>();
    let mut session = Session::new(
        vec![
            Scripted::text("One."),
            Scripted::text("Two."),
            Scripted::text("Three."),
        ],
        None,
    );
    std::fs::write(session.workspace.join("AGENTS.md"), &old).unwrap();
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    assert_eq!(kinds(&session.lines()), first_text_turn());
    let new = old.replace("line 50\n", "line fifty\n");
    std::fs::write(session.workspace.join("AGENTS.md"), &new).unwrap();
    session.inbox.send(delivery("again")).unwrap();
    session.turn();
    let lines = session.lines();
    let mut changed = vec!["instruction_file"];
    changed.extend(later_text_turn());
    assert_eq!(kinds(&lines), changed);
    assert_eq!(lines[0].payload["reason"], "changed");
    assert_eq!(lines[0].payload["sent"], "diff");
    assert_eq!(lines[0].payload["content"], new);
    let requests = session.requests();
    assert!(
        requests[1]
            .conversation
            .iter()
            .any(|input| matches!(input, Input::User { text } if text.contains("Apply this diff"))),
        "{:?}",
        requests[1].conversation
    );
    std::fs::remove_file(session.workspace.join("AGENTS.md")).unwrap();
    session.inbox.send(delivery("once more")).unwrap();
    session.turn();
    let lines = session.lines();
    let mut deleted = vec!["instruction_file"];
    deleted.extend(later_text_turn());
    assert_eq!(kinds(&lines), deleted);
    assert_eq!(lines[0].payload["reason"], "deleted");
    assert_eq!(lines[0].payload["sent"], "deleted");
    assert!(lines[0].payload.get("content").is_none());
}

#[test]
fn an_own_edit_is_recorded_after_the_call_and_sends_nothing() {
    let writer = Arc::new(WriteAgents {
        workspace: Mutex::new(PathBuf::new()),
    });
    let mut session = Session::with_tools(
        vec![
            calls_reply("Working.", &[("write_agents", city())]),
            Scripted::text("Done."),
        ],
        None,
        vec![Arc::clone(&writer) as Arc<dyn Tool>],
    );
    std::fs::write(session.workspace.join("AGENTS.md"), "Leaf.\n").unwrap();
    *writer.workspace.lock().unwrap() = session.workspace.clone();
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    // The complete turn, in order: the own edit lands with its call's
    // completion, before the next step starts.
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
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "instruction_file",
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
    let own = lines
        .iter()
        .find(|line| line.kind == "instruction_file")
        .unwrap();
    assert_eq!(own.payload["reason"], "own_edit");
    assert_eq!(own.payload["sent"], "none");
    assert_eq!(own.payload["content"], "Revised by the call.\n");
    // Nothing was sent: both requests hold the opening and the prompt only.
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(users(&requests[0].conversation), 2);
    assert_eq!(users(&requests[1].conversation), 2);
}

#[test]
fn an_own_edit_outside_the_workspace_is_recorded_and_sends_nothing() {
    let writer = Arc::new(WriteHome {
        home: Mutex::new(PathBuf::new()),
    });
    let mut session = Session::with_tools(
        vec![
            calls_reply("Working.", &[("write_home", city())]),
            Scripted::text("Done."),
        ],
        None,
        vec![Arc::clone(&writer) as Arc<dyn Tool>],
    );
    let home = session.dir.parent().unwrap().to_path_buf();
    std::fs::write(home.join("AGENTS.md"), "Global.\n").unwrap();
    *writer.home.lock().unwrap() = home;
    // Outside the workspace the call takes no fast path: a standing
    // allow lets it run without asking a person.
    session.rules.set(allow("write_home"));
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    // The complete turn, in order: the own edit lands with its call's
    // completion, before the next step starts.
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
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "instruction_file",
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
    let own = lines
        .iter()
        .find(|line| line.kind == "instruction_file")
        .unwrap();
    assert_eq!(own.payload["reason"], "own_edit");
    assert_eq!(own.payload["sent"], "none");
    assert_eq!(own.payload["content"], "Global, revised.\n");
    // Nothing was sent: both requests hold the opening and the prompt only.
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(users(&requests[0].conversation), 2);
    assert_eq!(users(&requests[1].conversation), 2);
}

/// A standing allow for `tool`, so its calls run without asking a person.
fn allow(tool: &str) -> StandingRules {
    StandingRules {
        global: vec![Rule {
            decision: RuleDecision::Allow,
            tool: tool.into(),
            prefix: String::new(),
            added: None,
            session_id: None,
        }],
        project: Vec::new(),
    }
}

/// The prompt's extension sections: each extension's name, its files'
/// paths, and its budget.
type Sections = Vec<(String, Vec<PathBuf>, Option<u64>)>;

/// A section file outside every temporary home, and the manifest entry
/// naming it: the tests own the directory, so its path is known before
/// the session is built.
fn section(content: &str) -> (fakes::TempDir, PathBuf, Sections) {
    let held = fakes::TempDir::new("fiber-section-file");
    let path = held.path().join("a.md");
    std::fs::write(&path, content).unwrap();
    let sections = vec![("fiber.test/notes".to_owned(), vec![path.clone()], None)];
    (held, path, sections)
}

#[test]
fn an_own_write_to_a_section_file_sends_nothing_at_the_next_turn() {
    let (_held, path, sections) = section("Notes.\n");
    let writer = Arc::new(support::WriteFile {
        name: "write",
        target: path,
        content: "Revised by the call.\n".into(),
        effect: Effect::Writes,
    });
    let mut session = Session::with_tools_sectioned(
        vec![
            calls_reply("Working.", &[("write", city())]),
            Scripted::text("Done."),
            Scripted::text("After."),
        ],
        vec![writer as Arc<dyn Tool>],
        sections,
    );
    session.rules.set(allow("write"));
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    // The complete turn, in order: the own edit lands with its call's
    // completion, before the next step starts.
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
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "instruction_file",
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
    let own = lines
        .iter()
        .find(|line| line.kind == "instruction_file")
        .unwrap();
    assert_eq!(own.payload["reason"], "own_edit");
    assert_eq!(own.payload["extension"], "fiber.test/notes");
    assert_eq!(own.payload["sent"], "none");
    assert_eq!(own.payload["content"], "Revised by the call.\n");
    // Nothing was sent: both requests hold the opening and the prompt only.
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(users(&requests[0].conversation), 2);
    assert_eq!(users(&requests[1].conversation), 2);
    // The next turn starts with no change.
    session.inbox.send(delivery("again")).unwrap();
    session.turn();
    assert_eq!(kinds(&session.lines()), later_text_turn());
}

#[test]
fn an_outside_edit_to_a_section_file_sends_a_diff_at_the_next_turn() {
    let old = (0..100).map(|n| format!("line {n}\n")).collect::<String>();
    let (_held, path, sections) = section(&old);
    let mut session = Session::sectioned(
        vec![Scripted::text("One."), Scripted::text("Two.")],
        sections,
    );
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    assert_eq!(kinds(&session.lines()), first_text_turn());
    let new = old.replace("line 50\n", "line fifty\n");
    std::fs::write(&path, &new).unwrap();
    session.inbox.send(delivery("again")).unwrap();
    session.turn();
    let lines = session.lines();
    let mut changed = vec!["instruction_file"];
    changed.extend(later_text_turn());
    assert_eq!(kinds(&lines), changed);
    assert_eq!(lines[0].payload["reason"], "changed");
    assert_eq!(lines[0].payload["extension"], "fiber.test/notes");
    assert_eq!(lines[0].payload["sent"], "diff");
    assert_eq!(lines[0].payload["content"], new);
    let requests = session.requests();
    assert!(
        requests[1]
            .conversation
            .iter()
            .any(|input| matches!(input, Input::User { text } if text.contains("Apply this diff"))),
        "{:?}",
        requests[1].conversation
    );
}

#[test]
fn an_outside_deletion_of_a_section_file_sends_one_line() {
    let (_held, path, sections) = section("Notes.\n");
    let mut session = Session::sectioned(
        vec![Scripted::text("One."), Scripted::text("Two.")],
        sections,
    );
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    assert_eq!(kinds(&session.lines()), first_text_turn());
    std::fs::remove_file(&path).unwrap();
    session.inbox.send(delivery("again")).unwrap();
    session.turn();
    let lines = session.lines();
    let mut deleted = vec!["instruction_file"];
    deleted.extend(later_text_turn());
    assert_eq!(kinds(&lines), deleted);
    assert_eq!(lines[0].payload["reason"], "deleted");
    assert_eq!(lines[0].payload["extension"], "fiber.test/notes");
    assert_eq!(lines[0].payload["sent"], "deleted");
    assert!(lines[0].payload.get("content").is_none());
}

#[test]
fn an_outside_creation_of_a_section_file_sends_its_full_text() {
    let held = fakes::TempDir::new("fiber-section-file");
    let path = held.path().join("a.md");
    // The manifest names a path that does not exist yet.
    let sections = vec![("fiber.test/notes".to_owned(), vec![path.clone()], None)];
    let mut session = Session::sectioned(
        vec![Scripted::text("One."), Scripted::text("Two.")],
        sections,
    );
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    assert_eq!(kinds(&session.lines()), first_text_turn());
    std::fs::write(&path, "New notes.\n").unwrap();
    session.inbox.send(delivery("again")).unwrap();
    session.turn();
    let lines = session.lines();
    let mut created = vec!["instruction_file"];
    created.extend(later_text_turn());
    assert_eq!(kinds(&lines), created);
    assert_eq!(lines[0].payload["reason"], "created");
    assert_eq!(lines[0].payload["extension"], "fiber.test/notes");
    assert_eq!(lines[0].payload["sent"], "full");
    assert_eq!(lines[0].payload["content"], "New notes.\n");
    let requests = session.requests();
    assert!(
        requests[1].conversation.iter().any(
            |input| matches!(input, Input::User { text } if text.contains("a new file appeared in the fiber.test/notes extension's section"))
        ),
        "{:?}",
        requests[1].conversation
    );
    drop(held);
}

/// A section file budgeted at `budget`, holding `content`: the manifest
/// entry naming it.
fn budgeted(path: &std::path::Path, budget: u64, content: &str) -> Sections {
    std::fs::write(path, content).unwrap();
    vec![(
        "fiber.test/notes".to_owned(),
        vec![path.to_path_buf()],
        Some(budget),
    )]
}

/// The text parts of a `tool_call_completed` line's content.
fn texts(line: &Envelope) -> Vec<&str> {
    line.payload["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
        .collect()
}

#[test]
fn an_over_budget_write_ends_its_result_with_the_prune_line() {
    let held = fakes::TempDir::new("fiber-section-file");
    let path = held.path().join("a.md");
    let sections = budgeted(&path, 5, "1234");
    let writer = Arc::new(support::WriteFile {
        name: "write",
        target: path,
        content: "123456".into(),
        effect: Effect::Writes,
    });
    let mut session = Session::with_tools_sectioned(
        vec![
            calls_reply("Working.", &[("write", city())]),
            Scripted::text("Done."),
        ],
        vec![writer as Arc<dyn Tool>],
        sections,
    );
    session.rules.set(allow("write"));
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    // The complete turn, in order: the own edit lands with its call's
    // completion, before the next step starts, and the prune line ends
    // the call's result.
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
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "instruction_file",
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
    let done = lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap();
    assert_eq!(done.payload["status"], "completed");
    let parts = texts(done);
    assert_eq!(
        *parts.last().unwrap(),
        "Fiber: these files are 6 bytes, over their budget of 5 bytes. Prune them."
    );
    // The own edit is recorded too, with nothing sent.
    let own = lines
        .iter()
        .find(|line| line.kind == "instruction_file")
        .unwrap();
    assert_eq!(own.payload["reason"], "own_edit");
    assert_eq!(own.payload["sent"], "none");
    drop(held);
}

#[test]
fn a_tool_declaring_a_write_gets_the_prune_line_whatever_its_name() {
    let held = fakes::TempDir::new("fiber-section-file");
    let path = held.path().join("a.md");
    let sections = budgeted(&path, 5, "1234");
    let writer = Arc::new(support::WriteFile {
        name: "notes_writer",
        target: path,
        content: "123456".into(),
        effect: Effect::Writes,
    });
    let mut session = Session::with_tools_sectioned(
        vec![
            calls_reply("Working.", &[("notes_writer", city())]),
            Scripted::text("Done."),
        ],
        vec![writer as Arc<dyn Tool>],
        sections,
    );
    session.rules.set(allow("notes_writer"));
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    let done = lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap();
    let parts = texts(done);
    assert_eq!(
        *parts.last().unwrap(),
        "Fiber: these files are 6 bytes, over their budget of 5 bytes. Prune them."
    );
    drop(held);
}

#[test]
fn a_tool_named_write_declaring_only_a_read_gets_no_prune_line() {
    let held = fakes::TempDir::new("fiber-section-file");
    let path = held.path().join("a.md");
    let sections = budgeted(&path, 5, "1234");
    let writer = Arc::new(support::WriteFile {
        name: "write",
        target: path,
        content: "123456".into(),
        effect: Effect::Reads,
    });
    let mut session = Session::with_tools_sectioned(
        vec![
            calls_reply("Working.", &[("write", city())]),
            Scripted::text("Done."),
        ],
        vec![writer as Arc<dyn Tool>],
        sections,
    );
    session.rules.set(allow("write"));
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    let done = lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap();
    assert!(
        texts(done)
            .iter()
            .all(|text| !text.starts_with("Fiber: these files are")),
        "{}",
        done.payload["content"]
    );
    drop(held);
}

#[test]
fn an_under_budget_write_has_no_prune_line() {
    let held = fakes::TempDir::new("fiber-section-file");
    let path = held.path().join("a.md");
    let sections = budgeted(&path, 5, "1234");
    let writer = Arc::new(support::WriteFile {
        name: "edit",
        target: path,
        content: "12".into(),
        effect: Effect::Writes,
    });
    let mut session = Session::with_tools_sectioned(
        vec![
            calls_reply("Working.", &[("edit", city())]),
            Scripted::text("Done."),
        ],
        vec![writer as Arc<dyn Tool>],
        sections,
    );
    session.rules.set(allow("edit"));
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    // The complete turn, in order: the own edit lands with its call's
    // completion, before the next step starts.
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
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "instruction_file",
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
    let done = lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap();
    assert!(
        texts(done).iter().all(|text| !text.contains("Prune them")),
        "{}",
        done.payload["content"]
    );
    drop(held);
}

#[test]
fn a_write_touching_no_section_file_has_no_prune_line() {
    let held = fakes::TempDir::new("fiber-section-file");
    let path = held.path().join("a.md");
    // Over budget, but the call touches another file.
    let sections = budgeted(&path, 5, "123456");
    let writer = Arc::new(support::WriteFile {
        name: "write",
        target: held.path().join("b.md"),
        content: "Elsewhere.\n".into(),
        effect: Effect::Writes,
    });
    let mut session = Session::with_tools_sectioned(
        vec![
            calls_reply("Working.", &[("write", city())]),
            Scripted::text("Done."),
        ],
        vec![writer as Arc<dyn Tool>],
        sections,
    );
    session.rules.set(allow("write"));
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    // The complete turn, in order: the write touches no section file,
    // so no own edit is recorded and no prune line ends the result.
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
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
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
    let done = lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap();
    assert!(
        texts(done).iter().all(|text| !text.contains("Prune them")),
        "{}",
        done.payload["content"]
    );
    drop(held);
}

#[test]
fn a_failed_write_over_budget_has_no_prune_line() {
    let held = fakes::TempDir::new("fiber-section-file");
    let path = held.path().join("a.md");
    // Over budget on disk, but the call fails.
    let sections = budgeted(&path, 5, "123456");
    let mut failing = support::TestTool::failing("write", contract::ErrorCode::ToolError);
    failing.effects = Ok(DeclaredEffects {
        effects: vec![Effect::Writes],
        reversible: true,
        paths: Some(vec![path.display().to_string()]),
    });
    let mut session = Session::with_tools_sectioned(
        vec![
            calls_reply("Working.", &[("write", city())]),
            Scripted::text("Done."),
        ],
        vec![Arc::new(failing) as Arc<dyn Tool>],
        sections,
    );
    session.rules.set(allow("write"));
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    // The complete turn, in order: the call fails, so no own edit is
    // recorded and no prune line ends the result.
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
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
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
    let done = lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap();
    assert_eq!(done.payload["status"], "failed");
    assert!(
        texts(done).iter().all(|text| !text.contains("Prune them")),
        "{}",
        done.payload["content"]
    );
    drop(held);
}

#[test]
fn a_subdirectory_file_is_queued_for_the_next_step() {
    let scan = Arc::new(support::TestTool::declaring(
        "scan",
        "Scanned.",
        vec![Effect::Reads],
        Some(vec!["sub/notes.txt".into()]),
    ));
    let mut session = Session::with_tools(
        vec![
            calls_reply("Working.", &[("scan", city())]),
            Scripted::text("Done."),
        ],
        None,
        vec![scan as Arc<dyn Tool>],
    );
    // Below the workspace: the opening message never reads it.
    std::fs::create_dir_all(session.workspace.join("sub")).unwrap();
    std::fs::write(session.workspace.join("sub/AGENTS.md"), "Sub rules.\n").unwrap();
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    // The complete turn, in order: queued at completion, written at the
    // next step start, after the result and under a later `step_started`.
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
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "instruction_file",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let opening = lines
        .iter()
        .find(|line| line.kind == "opening_message")
        .unwrap();
    assert_eq!(
        opening.payload["instruction_files"],
        serde_json::Value::Array(Vec::new())
    );
    let sub = lines
        .iter()
        .find(|line| line.kind == "instruction_file")
        .unwrap();
    assert_eq!(sub.payload["reason"], "subdirectory");
    assert_eq!(sub.payload["sent"], "full");
    assert_eq!(sub.payload["content"], "Sub rules.\n");
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].conversation.iter().any(
            |input| matches!(input, Input::User { text } if text.contains("has its own instruction file"))
        ),
        "{:?}",
        requests[1].conversation
    );
}

#[test]
fn a_later_date_appends_one_line() {
    let mut session = Session::new(vec![Scripted::text("One."), Scripted::text("Two.")], None);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    assert_eq!(kinds(&session.lines()), first_text_turn());
    // The fake clock reads 2023-11-14T22:13:20Z: two hours on is the 15th.
    session.clock.advance(Duration::from_secs(2 * 3_600));
    session.inbox.send(delivery("again")).unwrap();
    session.turn();
    let lines = session.lines();
    let mut dated = vec!["date_changed"];
    dated.extend(later_text_turn());
    assert_eq!(kinds(&lines), dated);
    assert_eq!(lines[0].payload["date"], "2023-11-15");
    let requests = session.requests();
    assert!(
        requests[1].conversation.iter().any(
            |input| matches!(input, Input::User { text } if text.contains("the date is now 2023-11-15"))
        ),
        "{:?}",
        requests[1].conversation
    );
}

/// A session run for real through one loop, then resumed on another: the
/// fold between them.
struct Resumed {
    root: fakes::TempDir,
    dir: PathBuf,
    log: Arc<Log>,
    clock: Arc<fakes::clock::FakeClock>,
    workspace: PathBuf,
    credentials: PathBuf,
    rules: Arc<support::FakeRules>,
    provider: Arc<ScriptedProvider>,
    history_len: usize,
}

impl Resumed {
    fn new(script: Vec<Scripted>) -> Self {
        let root = fakes::TempDir::new("fiber-changes");
        let workspace = root.path().join("w");
        std::fs::create_dir_all(&workspace).unwrap();
        let credentials = root.path().join("credentials");
        std::fs::create_dir_all(&credentials).unwrap();
        let clock = fakes::clock::FakeClock::new();
        let log =
            Arc::new(Log::create(root.path(), SessionId("s_1".into()), clock.clone()).unwrap());
        Self {
            dir: root.path().join("s_1"),
            root,
            log,
            clock,
            workspace,
            credentials,
            rules: Arc::new(support::FakeRules::empty()),
            provider: Arc::new(ScriptedProvider::new(script)),
            history_len: 0,
        }
    }

    fn start(&self, inbox: mpsc::Receiver<Delivery>) -> Loop {
        Loop::start(
            Arc::clone(&self.log),
            Arc::clone(&self.provider) as Arc<dyn Provider>,
            Self::model(),
            self.prompt(),
            inbox,
            Vec::new(),
            self.permissions(),
        )
        .unwrap()
    }

    fn resume(&mut self, inbox: mpsc::Receiver<Delivery>) -> Loop {
        self.history_len = self.lines().len();
        Loop::resume(
            Arc::clone(&self.log),
            r#loop::resumed(&self.dir).unwrap(),
            Arc::clone(&self.provider) as Arc<dyn Provider>,
            Self::model(),
            self.prompt(),
            inbox,
            Vec::new(),
            self.permissions(),
        )
        .unwrap()
    }

    /// Sends `prompt`, then runs one turn to completion.
    fn drive(&self, looped: Loop, tx: &mpsc::Sender<Delivery>, prompt: &str) -> TurnOutcome {
        tx.send(Delivery::Prompt(
            support::message(prompt),
            support::ignore(),
        ))
        .unwrap();
        let mut looped = looped;
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = looped.turn().unwrap().unwrap();
            done.send(outcome).unwrap();
        });
        finished
            .recv_timeout(support::DEADLINE)
            .expect("turn ended")
    }

    /// The lines written since the resume.
    fn new_lines(&self) -> Vec<Envelope> {
        self.lines()[self.history_len..].to_vec()
    }

    fn prompt(&self) -> r#loop::PromptInputs {
        let owned: Arc<fakes::clock::FakeClock> = Arc::clone(&self.clock);
        let clock: Arc<dyn contract::clock::Clock> = owned;
        r#loop::PromptInputs::new(
            self.root.path().to_path_buf(),
            "/bin/sh".into(),
            self.dir.join("events.jsonl").display().to_string(),
            clock,
        )
    }

    fn model() -> Model {
        Model {
            reference: support::MODEL.into(),
            cost: None,
            subscription: false,
        }
    }

    fn permissions(&self) -> r#loop::Permissions {
        r#loop::Permissions {
            workspace: self.workspace.display().to_string(),
            credentials: self.credentials.clone(),
            credential_files: Vec::new(),
            rules: self.rules.clone(),
        }
    }

    fn lines(&self) -> Vec<Envelope> {
        log::read(&self.dir).unwrap()
    }
}

#[test]
fn a_resume_detects_an_outside_change_with_a_diff() {
    let old = (0..100).map(|n| format!("line {n}\n")).collect::<String>();
    let mut session = Resumed::new(vec![Scripted::text("One."), Scripted::text("Two.")]);
    std::fs::write(session.workspace.join("AGENTS.md"), &old).unwrap();
    let (tx1, rx1) = mpsc::channel();
    let looped = session.start(rx1);
    let outcome = session.drive(looped, &tx1, "hi");
    assert!(matches!(outcome, TurnOutcome::Completed));
    // Edited outside while the session was away.
    let new = old.replace("line 50\n", "line fifty\n");
    std::fs::write(session.workspace.join("AGENTS.md"), &new).unwrap();
    let (tx2, rx2) = mpsc::channel();
    let looped = session.resume(rx2);
    let outcome = session.drive(looped, &tx2, "again");
    assert!(matches!(outcome, TurnOutcome::Completed));
    let lines = session.new_lines();
    // The complete resumed turn, in order: the rebuilt preamble, then
    // the check, with the change between `preamble_built` and
    // `turn_started`.
    assert_eq!(
        kinds(&lines),
        [
            "preamble_built",
            "instruction_file",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(lines[0].payload["reason"], "resume");
    assert_eq!(lines[1].kind, "instruction_file");
    assert_eq!(lines[1].payload["reason"], "changed");
    assert_eq!(lines[1].payload["sent"], "diff");
    assert_eq!(lines[1].payload["content"], new);
    let requests = session.provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .conversation
            .iter()
            .any(|input| matches!(input, Input::User { text } if text.contains("Apply this diff"))),
        "{:?}",
        requests[1].conversation
    );
}
