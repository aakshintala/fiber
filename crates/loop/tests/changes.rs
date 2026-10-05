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

fn users(conversation: &[Input]) -> usize {
    conversation
        .iter()
        .filter(|input| matches!(input, Input::User { .. }))
        .count()
}

#[test]
fn a_first_turn_has_no_change() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    // In the opening message: sent, not a change.
    std::fs::write(session.workspace.join("AGENTS.md"), "Leaf.\n").unwrap();
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert!(!kinds(&lines).contains(&"instruction_file"));
    assert!(!kinds(&lines).contains(&"date_changed"));
}

#[test]
fn a_new_instruction_file_is_created_at_the_next_turn_start() {
    let mut session = Session::new(vec![Scripted::text("One."), Scripted::text("Two.")], None);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    assert!(
        session
            .lines()
            .iter()
            .all(|line| line.kind != "instruction_file")
    );
    std::fs::write(session.workspace.join("AGENTS.md"), "Leaf.\n").unwrap();
    session.inbox.send(delivery("again")).unwrap();
    session.turn();
    let lines = session.lines();
    let kinds = kinds(&lines);
    // After the first turn's opening message, before the turn starts.
    assert_eq!(kinds[0], "instruction_file");
    assert_eq!(kinds[1], "turn_started");
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
    session.lines();
    let new = old.replace("line 50\n", "line fifty\n");
    std::fs::write(session.workspace.join("AGENTS.md"), &new).unwrap();
    session.inbox.send(delivery("again")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(kinds(&lines)[0], "instruction_file");
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
    assert_eq!(kinds(&lines)[0], "instruction_file");
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
    let kinds = kinds(&lines);
    let completed = kinds
        .iter()
        .position(|kind| *kind == "tool_call_completed")
        .unwrap();
    let edited = kinds
        .iter()
        .position(|kind| *kind == "instruction_file")
        .unwrap();
    assert!(edited > completed, "{kinds:?}");
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
    let kinds = kinds(&lines);
    let opening = lines
        .iter()
        .find(|line| line.kind == "opening_message")
        .unwrap();
    assert_eq!(
        opening.payload["instruction_files"],
        serde_json::Value::Array(Vec::new())
    );
    let completed = kinds
        .iter()
        .position(|kind| *kind == "tool_call_completed")
        .unwrap();
    let found = kinds
        .iter()
        .position(|kind| *kind == "instruction_file")
        .unwrap();
    // Queued at completion, written at the next step start: after the
    // result, under a later `step_started`.
    assert!(found > completed, "{kinds:?}");
    let stepped = kinds[..found]
        .iter()
        .rposition(|kind| *kind == "step_started")
        .unwrap();
    assert!(stepped > completed, "{kinds:?}");
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
    session.lines();
    // The fake clock reads 2023-11-14T22:13:20Z: two hours on is the 15th.
    session.clock.advance(Duration::from_secs(2 * 3_600));
    session.inbox.send(delivery("again")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(kinds(&lines)[0], "date_changed");
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
        let lines = self.lines();
        self.history_len = lines.len();
        Loop::resume(
            Arc::clone(&self.log),
            &lines,
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
    // The resumed loop rebuilds its preamble, then checks: the change
    // lands between `preamble_built` and `turn_started`.
    assert_eq!(lines[0].kind, "preamble_built");
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
