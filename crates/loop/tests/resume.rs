//! Resuming a session (`docs/events.md`, "Resume"): the fixed results for
//! calls a crash left without one, and the state a resume folds back.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use contract::events::{
    CallStatus, Empty, Environment, Event, InputItem, OpeningMessage, ReasoningCompleted,
    SteeringApplied, TextCompleted, ToolCallCompleted, ToolCallRequested, ToolCallStarted,
    TurnStarted,
};
use contract::provider::Input;
use contract::shapes::{ContentPart, DeclaredEffects, Origin, Sender};
use contract::{ActionId, CommandId, Envelope, SessionId};
use log::Log;
use serde_json::json;

const MODEL: &str = "fake/model-1";

/// A log in a temporary directory, with helpers appending lines under fixed
/// turn and action ids.
struct LogLines {
    _root: fakes::TempDir,
    dir: std::path::PathBuf,
    log: Log,
}

impl LogLines {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-resume");
        let log = Log::create(
            root.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap();
        let dir = root.path().join("s_1");
        Self {
            dir,
            _root: root,
            log,
        }
    }

    fn append(&self, event: Event, action: Option<&str>) {
        self.log
            .append(
                &event,
                Some(contract::TurnId("t_1".into())),
                action.map(|a| ActionId(a.into())),
            )
            .unwrap();
    }

    fn lines(&self) -> Vec<Envelope> {
        log::read(&self.dir).unwrap()
    }
}

fn user_turn(text: &str) -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Text { text: text.into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_1".into())),
            },
            changed_by: None,
        }],
    })
}

fn requested(name: &str) -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: name.into(),
        arguments: json!({"city": "Paris"}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

fn started() -> Event {
    Event::ToolCallStarted(ToolCallStarted {
        declared: DeclaredEffects {
            effects: Vec::new(),
            reversible: true,
            paths: None,
        },
        arguments: None,
        changed_by: None,
    })
}

fn completed(text: &str) -> Event {
    Event::ToolCallCompleted(ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![ContentPart::Text { text: text.into() }],
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
    })
}

fn assistant(text: &str) -> Event {
    Event::TextCompleted(TextCompleted {
        text: text.into(),
        provider_item: None,
    })
}

fn reasoning(text: &str) -> Event {
    Event::ReasoningCompleted(ReasoningCompleted {
        text: text.into(),
        provider_item: None,
    })
}

fn message_started() -> Event {
    Event::AssistantMessageStarted(Empty {})
}

fn kinds_of(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|l| l.kind.as_str()).collect()
}

fn steering(text: &str) -> Event {
    Event::SteeringApplied(SteeringApplied {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: Sender {
            origin: Origin::Driver,
            command_id: Some(CommandId("c_2".into())),
        },
        changed_by: None,
    })
}

fn result_of(input: &Input) -> (&ActionId, &str, bool) {
    let Input::ToolResult {
        action_id,
        text,
        is_error,
        ..
    } = input
    else {
        panic!("expected a tool result, got {input:?}");
    };
    (action_id, text, *is_error)
}

#[test]
fn a_completed_call_is_unchanged() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(requested("read"), Some("a_1"));
    log.append(started(), Some("a_1"));
    log.append(completed("Paris."), Some("a_1"));

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        [
            "turn_started",
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed"
        ]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    assert_eq!(conversation.len(), 3);
    assert!(matches!(conversation[0], Input::User { .. }));
    assert!(matches!(conversation[1], Input::ToolCall { .. }));
    let (id, text, is_error) = result_of(&conversation[2]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "Paris.");
    assert!(!is_error);
}

#[test]
fn a_requested_only_call_is_sent_as_never_ran() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(requested("read"), Some("a_1"));

    let lines = log.lines();
    assert_eq!(kinds_of(&lines), ["turn_started", "tool_call_requested"]);
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    assert_eq!(conversation.len(), 3);
    let (id, text, is_error) = result_of(&conversation[2]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "It never ran.");
    assert!(is_error);
}

#[test]
fn a_started_call_without_a_result_is_sent_as_outcome_unknown() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(requested("read"), Some("a_1"));
    log.append(started(), Some("a_1"));

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        ["turn_started", "tool_call_requested", "tool_call_started"]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    assert_eq!(conversation.len(), 3);
    let (id, text, is_error) = result_of(&conversation[2]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "Its outcome is unknown: it may have run.");
    assert!(is_error);
}

#[test]
fn one_completed_and_one_cut_short_call_share_a_batch() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(requested("read"), Some("a_1"));
    log.append(requested("search"), Some("a_2"));
    log.append(started(), Some("a_1"));
    log.append(completed("Paris."), Some("a_1"));
    log.append(started(), Some("a_2"));

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        [
            "turn_started",
            "tool_call_requested",
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_started",
        ]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    // The real result stays where the log put it; the fixed one follows the
    // batch.
    assert_eq!(conversation.len(), 5);
    let (_, text, is_error) = result_of(&conversation[3]);
    assert_eq!(text, "Paris.");
    assert!(!is_error);
    let (id, text, is_error) = result_of(&conversation[4]);
    assert_eq!(id.0, "a_2");
    assert_eq!(text, "Its outcome is unknown: it may have run.");
    assert!(is_error);
}

#[test]
fn a_fixed_result_sits_before_the_next_user_message() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(requested("read"), Some("a_1"));
    log.append(user_turn("two"), None);

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        ["turn_started", "tool_call_requested", "turn_started"]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    assert_eq!(conversation.len(), 4);
    let (id, text, _) = result_of(&conversation[2]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "It never ran.");
    assert!(matches!(&conversation[3], Input::User { text } if text == "two"));
}

#[test]
fn a_text_between_request_and_start_does_not_flush_early() {
    // The reply's text sits between the request and the start in log
    // order. The batch is the whole reply plus its result lines, so the
    // start is still seen before the flush decides.
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(assistant("Looking."), Some("a_0"));
    log.append(requested("read"), Some("a_1"));
    log.append(assistant("Found it."), Some("a_0"));
    log.append(started(), Some("a_1"));

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        [
            "turn_started",
            "text_completed",
            "tool_call_requested",
            "text_completed",
            "tool_call_started",
        ]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    assert_eq!(conversation.len(), 5);
    assert!(matches!(&conversation[1], Input::Assistant { .. }));
    assert!(matches!(&conversation[2], Input::ToolCall { .. }));
    assert!(matches!(&conversation[3], Input::Assistant { .. }));
    let (id, text, is_error) = result_of(&conversation[4]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "Its outcome is unknown: it may have run.");
    assert!(is_error);
}

#[test]
fn a_later_text_does_not_flush_a_pending_call() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(reasoning("Hmm."), Some("a_0"));
    log.append(requested("read"), Some("a_1"));
    log.append(requested("search"), Some("a_2"));
    log.append(assistant("One down."), Some("a_0"));
    log.append(completed("Paris."), Some("a_1"));

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        [
            "turn_started",
            "reasoning_completed",
            "tool_call_requested",
            "tool_call_requested",
            "text_completed",
            "tool_call_completed",
        ]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    // The real result stays where the log put it; the text ends no batch.
    assert_eq!(conversation.len(), 7);
    assert!(matches!(&conversation[1], Input::Reasoning { .. }));
    assert!(matches!(&conversation[4], Input::Assistant { .. }));
    let (_, text, is_error) = result_of(&conversation[5]);
    assert_eq!(text, "Paris.");
    assert!(!is_error);
    let (id, text, is_error) = result_of(&conversation[6]);
    assert_eq!(id.0, "a_2");
    assert_eq!(text, "It never ran.");
    assert!(is_error);
}

#[test]
fn a_completion_after_a_flush_leaves_exactly_one_result() {
    // The completion sits after a `turn_started` that already flushed the
    // batch. The pre-scan sees it, so the call gets no fixed result: the
    // real one stays where the log put it, the only result the call has.
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(requested("read"), Some("a_1"));
    log.append(user_turn("two"), None);
    log.append(completed("Paris."), Some("a_1"));

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        [
            "turn_started",
            "tool_call_requested",
            "turn_started",
            "tool_call_completed",
        ]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    assert_eq!(conversation.len(), 4);
    assert!(matches!(&conversation[1], Input::ToolCall { .. }));
    assert!(matches!(&conversation[2], Input::User { .. }));
    let (id, text, is_error) = result_of(&conversation[3]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "Paris.");
    assert!(!is_error);
    for input in &conversation {
        if let Input::ToolResult { text, .. } = input {
            assert!(
                !text.contains("never ran") && !text.contains("unknown"),
                "{text}"
            );
        }
    }
}

#[test]
fn two_pending_calls_get_fixed_results_in_request_order() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(requested("read"), Some("a_1"));
    log.append(assistant("Two calls."), Some("a_0"));
    log.append(requested("search"), Some("a_2"));

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        [
            "turn_started",
            "tool_call_requested",
            "text_completed",
            "tool_call_requested",
        ]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    assert_eq!(conversation.len(), 6);
    let (first, _, _) = result_of(&conversation[4]);
    let (second, _, _) = result_of(&conversation[5]);
    assert_eq!(first.0, "a_1");
    assert_eq!(second.0, "a_2");
}

#[test]
fn steering_starts_a_new_batch() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(requested("read"), Some("a_1"));
    log.append(steering("wait"), Some("a_9"));

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        ["turn_started", "tool_call_requested", "steering_applied"]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    assert_eq!(conversation.len(), 4);
    let (id, text, _) = result_of(&conversation[2]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "It never ran.");
    assert!(matches!(&conversation[3], Input::User { .. }));
}

#[test]
fn an_assistant_message_start_starts_a_new_batch() {
    // The log continues after the flush point: without the
    // `assistant_message_started` flush, the fixed result would sit after
    // the text at the end of the log.
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(requested("read"), Some("a_1"));
    log.append(message_started(), Some("a_9"));
    log.append(assistant("On it."), Some("a_9"));

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        [
            "turn_started",
            "tool_call_requested",
            "assistant_message_started",
            "text_completed",
        ]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    assert_eq!(conversation.len(), 4);
    let (id, text, _) = result_of(&conversation[2]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "It never ran.");
    assert!(matches!(&conversation[3], Input::Assistant { .. }));
}

#[test]
fn a_fixed_result_at_the_end_of_the_log_ends_the_conversation() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(assistant("Looking."), Some("a_0"));
    log.append(requested("read"), Some("a_1"));

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        ["turn_started", "text_completed", "tool_call_requested"]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    assert_eq!(conversation.len(), 4);
    let (id, text, _) = result_of(&conversation[3]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "It never ran.");
}

#[test]
fn a_call_with_no_action_id_is_skipped() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(requested("read"), None);
    log.append(completed("Paris."), None);

    let lines = log.lines();
    assert_eq!(
        kinds_of(&lines),
        ["turn_started", "tool_call_requested", "tool_call_completed"]
    );
    let conversation = r#loop::rebuild(&lines, MODEL).unwrap();

    assert_eq!(conversation.len(), 1);
}

// `Loop::resume`: the first request, the folds, and the state.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc;

use contract::events::{
    DecidedBy, Decision, Grant, PermissionResolved, ReviewerRef, SessionStarted, UsageRecorded,
    Variables, VariablesSource,
};
use contract::inbox::Delivery;
use contract::provider::Provider;
use contract::shapes::Tokens;
use contract::tool::Tool;
use contract::{ErrorCode, GenerationId};
use fakes::{Scripted, ScriptedProvider};
use r#loop::{Loop, Model};

/// `PromptInputs` over temp `home` with a fixed shell: the opening message
/// reads no real files.
fn resume_prompt(home: &std::path::Path) -> r#loop::PromptInputs {
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    r#loop::PromptInputs::new(
        home.to_path_buf(),
        "/bin/sh".into(),
        home.join("events.jsonl").display().to_string(),
        clock,
    )
}

/// A history log with helpers, then a resumed loop on it.
struct History {
    _root: fakes::TempDir,
    dir: std::path::PathBuf,
    log: Arc<Log>,
    workspace: String,
    credentials: std::path::PathBuf,
    rules: Arc<support::FakeRules>,
    provider: Arc<ScriptedProvider>,
    inbox_tx: mpsc::Sender<Delivery>,
    inbox_rx: Option<mpsc::Receiver<Delivery>>,
    history_len: usize,
}

impl History {
    fn new(script: Vec<Scripted>) -> Self {
        let root = fakes::TempDir::new("fiber-resume");
        let workspace_dir = root.path().join("w");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let credentials = root.path().join("credentials");
        std::fs::create_dir_all(&credentials).unwrap();
        let workspace = workspace_dir.display().to_string();
        let log = Arc::new(
            Log::create(
                root.path(),
                SessionId("s_1".into()),
                fakes::clock::FakeClock::new(),
            )
            .unwrap(),
        );
        let dir = root.path().join("s_1");
        let history = Self {
            _root: root,
            dir,
            log,
            workspace,
            credentials,
            rules: Arc::new(support::FakeRules::empty()),
            provider: Arc::new(ScriptedProvider::new(script)),
            inbox_tx: mpsc::channel().0,
            inbox_rx: None,
            history_len: 0,
        };
        let (tx, rx) = mpsc::channel();
        let mut history = History {
            inbox_tx: tx,
            inbox_rx: Some(rx),
            ..history
        };
        history.session_started();
        history
    }

    fn tid(&self) -> contract::TurnId {
        contract::TurnId("t_1".into())
    }

    fn write(&self, event: Event, action: Option<&str>) {
        self.log
            .append(&event, Some(self.tid()), action.map(|a| ActionId(a.into())))
            .unwrap();
    }

    fn session_started(&mut self) {
        self.log
            .append(
                &Event::SessionStarted(SessionStarted {
                    workspace: self.workspace.clone(),
                    variables: Variables {
                        path: String::new(),
                        names: Vec::new(),
                        source: VariablesSource::Inherited,
                    },
                    parent: None,
                    forked_from: None,
                    rewind: None,
                }),
                None,
                None,
            )
            .unwrap();
    }

    fn lines(&self) -> Vec<Envelope> {
        log::read(&self.dir).unwrap()
    }

    /// Freezes the history: the lines a resume folds.
    fn freeze(&mut self) {
        self.history_len = self.lines().len();
    }

    fn model() -> Model {
        Model {
            reference: support::MODEL.into(),
            cost: None,
            subscription: false,
        }
    }

    /// `PromptInputs` over a temp home with a fixed shell: the opening
    /// message reads no real files.
    fn prompt(&self) -> r#loop::PromptInputs {
        resume_prompt(self._root.path())
    }

    fn resume(&mut self, tools: Vec<(String, Arc<dyn Tool>)>) -> Loop {
        Loop::resume(
            Arc::clone(&self.log),
            r#loop::resumed(&self.dir).unwrap(),
            Arc::clone(&self.provider) as Arc<dyn contract::provider::Provider>,
            Self::model(),
            self.prompt(),
            self.inbox_rx.take().unwrap(),
            tools,
            r#loop::Permissions {
                workspace: self.workspace.clone(),
                credentials: self.credentials.clone(),
                rules: self.rules.clone(),
            },
        )
        .unwrap()
    }

    /// Sends `prompt`, then runs one turn, failing at [`support::DEADLINE`].
    fn run(&mut self, looped: Loop, prompt: &str) -> contract::events::TurnOutcome {
        self.inbox_tx
            .send(Delivery::Prompt(
                support::message(prompt),
                support::ignore(),
            ))
            .unwrap();
        let mut looped = looped;
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = looped.turn().unwrap().unwrap();
            done.send((looped, outcome)).unwrap();
        });
        let (looped, outcome) = finished
            .recv_timeout(support::DEADLINE)
            .expect("turn ended");
        let _ = looped;
        outcome
    }

    /// The history's kinds, in order: the lines the resume folds.
    fn history_kinds(&self) -> Vec<String> {
        self.lines()[..self.history_len]
            .iter()
            .map(|l| l.kind.clone())
            .collect()
    }

    /// The lines written since [`History::freeze`], and their kinds.
    fn new_lines(&self) -> Vec<Envelope> {
        self.lines()[self.history_len..].to_vec()
    }

    fn new_kinds(&self) -> Vec<String> {
        self.new_lines().iter().map(|l| l.kind.clone()).collect()
    }

    fn usage(id: &str, model: &str, cost: Option<f64>) -> Event {
        Event::UsageRecorded(UsageRecorded {
            generation_id: GenerationId(id.into()),
            model: model.into(),
            tokens: Tokens {
                input: 10,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 3,
            },
            web_searches: None,
            cost,
            subscription: None,
            extension: None,
            origin_session_id: None,
        })
    }
}

#[test]
fn the_first_request_after_resume_carries_the_earlier_turn_the_fixed_result_and_the_prompt() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(Event::AssistantMessageStarted(Empty {}), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.freeze();
    assert_eq!(
        history.history_kinds(),
        [
            "session_started",
            "turn_started",
            "assistant_message_started",
            "tool_call_requested",
        ]
    );

    let looped = history.resume(Vec::new());
    let outcome = history.run(looped, "two");

    assert_eq!(outcome, contract::events::TurnOutcome::Completed);
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    assert_eq!(conversation.len(), 5);
    // The log holds no opening message, so the resume writes one at its
    // first turn, at the front of the context.
    assert!(
        matches!(&conversation[0], Input::User { text } if text.starts_with("This message is from Fiber"))
    );
    assert!(matches!(&conversation[1], Input::User { text } if text == "one"));
    let Input::ToolCall { action_id, .. } = &conversation[2] else {
        panic!("{conversation:?}");
    };
    assert_eq!(action_id.0, "a_1");
    let (id, text, is_error) = result_of(&conversation[3]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "It never ran.");
    assert!(is_error);
    assert!(matches!(&conversation[4], Input::User { text } if text == "two"));
    // `sent` is the conversation's length at the dead process's last
    // `assistant_message_started`: the previous request ended after "one".
    assert_eq!(requests[0].previous_end, Some(1));
    // The cache key is the session's own id.
    assert_eq!(requests[0].cache_key, "s_1");

    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn resume_writes_no_session_started_and_seq_continues() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(requested("read"), Some("a_1"));
    history.freeze();

    let looped = history.resume(Vec::new());
    history.run(looped, "two");

    let lines = history.lines();
    assert_eq!(
        lines.iter().filter(|l| l.kind == "session_started").count(),
        1
    );
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        [
            "session_started",
            "turn_started",
            "tool_call_requested",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let seqs: Vec<u64> = lines.iter().map(|l| l.seq.unwrap().0).collect();
    assert_eq!(seqs, (0..lines.len() as u64).collect::<Vec<_>>());
}

#[test]
fn a_session_grant_from_before_the_resume_is_honoured() {
    let mut tool = support::TestTool::declaring(
        "exec",
        "done",
        vec![contract::shapes::Effect::Executes],
        None,
    );
    tool.subject = Some("run tests".into());
    let tool = Arc::new(tool);
    let mut history = History::new(vec![
        support::tool_call_reply("Go.", &["exec"]),
        Scripted::text("Done."),
    ]);
    history.write(user_turn("one"), None);
    history.write(requested("exec"), Some("a_1"));
    history.write(
        Event::PermissionResolved(PermissionResolved {
            request_id: None,
            decision: Decision::Allow,
            decided_by: DecidedBy::Person,
            reason: None,
            feedback: None,
            grant: Some(Grant {
                tool: "exec".into(),
                prefix: "run tests".into(),
            }),
            rule: None,
            reviewer: None,
        }),
        Some("a_1"),
    );
    history.write(started(), Some("a_1"));
    history.write(completed("done"), Some("a_1"));
    history.freeze();
    assert_eq!(
        history.history_kinds(),
        [
            "session_started",
            "turn_started",
            "tool_call_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
        ]
    );

    let looped = history.resume(vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)]);
    let outcome = history.run(looped, "two");

    assert_eq!(outcome, contract::events::TurnOutcome::Completed);
    assert_eq!(tool.ran().len(), 1);
    // No new `permission_requested`: the grant decided the call.
    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested"),
        "{:?}",
        history.new_kinds()
    );
    // The decision rode the grant.
    let resolved = history
        .new_lines()
        .into_iter()
        .find(|l| l.kind == "permission_resolved")
        .unwrap();
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(resolved.payload["decided_by"], "session_grant");
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn budget_counts_spend_from_before_the_resume() {
    let mut history = History::new(vec![]);
    history.write(user_turn("one"), None);
    history.write(History::usage("g1", support::MODEL, Some(5.0)), Some("a_1"));
    history.freeze();
    assert_eq!(
        history.history_kinds(),
        ["session_started", "turn_started", "usage_recorded"]
    );

    let looped = history.resume(Vec::new()).budget(Some(1.0));
    let outcome = history.run(looped, "two");

    assert_eq!(outcome, contract::events::TurnOutcome::Failed);
    assert!(history.provider.requests().is_empty());
    let kinds = history.new_kinds();
    assert_eq!(
        kinds,
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "turn_completed"
        ]
    );
    let completed = history
        .new_lines()
        .into_iter()
        .find(|l| l.kind == "turn_completed")
        .unwrap();
    assert_eq!(completed.payload["error"]["code"], "budget_exceeded");
}

#[test]
fn reviewer_denies_from_before_the_resume_count_toward_the_session_limit() {
    let mut history = History::new(vec![
        support::tool_call_reply("Go.", &["exec"]),
        Scripted::text("After."),
    ]);
    let mut tool = support::TestTool::declaring(
        "exec",
        "done",
        vec![contract::shapes::Effect::Executes],
        None,
    );
    tool.subject = Some("run tests".into());
    let tool = Arc::new(tool);
    for _ in 0..20 {
        history.write(
            Event::PermissionResolved(PermissionResolved {
                request_id: None,
                decision: Decision::Deny,
                decided_by: DecidedBy::Reviewer,
                reason: Some("no".into()),
                feedback: None,
                grant: None,
                rule: None,
                reviewer: Some(ReviewerRef {
                    model: "fake/reviewer-1".into(),
                    stage: 2,
                }),
            }),
            Some("a_old"),
        );
    }
    history.freeze();
    let mut expected = vec!["session_started".to_owned()];
    expected.extend(vec!["permission_resolved".to_owned(); 20]);
    assert_eq!(history.history_kinds(), expected);

    let reviewer = Arc::new(ScriptedProvider::new(vec![
        Scripted::text("check"),
        Scripted::text("block: it writes"),
    ]));
    let (tx, rx) = mpsc::channel();
    let looped = Loop::resume(
        Arc::clone(&history.log),
        r#loop::resumed(&history.dir).unwrap(),
        Arc::clone(&history.provider) as Arc<dyn Provider>,
        History::model(),
        history.prompt(),
        rx,
        vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)],
        r#loop::Permissions {
            workspace: history.workspace.clone(),
            credentials: history.credentials.clone(),
            rules: history.rules.clone(),
        },
    )
    .unwrap()
    .answerable(false)
    .reviewer(
        Ok(r#loop::Reviewer {
            provider: reviewer,
            model: Model {
                reference: "fake/reviewer-1".into(),
                cost: None,
                subscription: false,
            },
        }),
        r#loop::BlockLimits::default(),
    );
    history.inbox_tx = tx;
    let outcome = history.run(looped, "two");

    // The 21st session block escalates with no person to answer: the turn
    // fails `blocked`.
    assert_eq!(outcome, contract::events::TurnOutcome::Failed);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    let completed = history
        .new_lines()
        .into_iter()
        .find(|l| l.kind == "turn_completed")
        .unwrap();
    assert_eq!(completed.payload["error"]["code"], "blocked");
}

#[test]
fn non_reviewer_denies_from_before_the_resume_do_not_count() {
    let mut history = History::new(vec![
        support::tool_call_reply("Go.", &["exec"]),
        Scripted::text("After."),
    ]);
    let mut tool = support::TestTool::declaring(
        "exec",
        "done",
        vec![contract::shapes::Effect::Executes],
        None,
    );
    tool.subject = Some("run tests".into());
    let tool = Arc::new(tool);
    // Twenty denials with no `reviewer` object: a reviewer failure, not a
    // model's block.
    for _ in 0..20 {
        history.write(
            Event::PermissionResolved(PermissionResolved {
                request_id: None,
                decision: Decision::Deny,
                decided_by: DecidedBy::Reviewer,
                reason: Some("no".into()),
                feedback: None,
                grant: None,
                rule: None,
                reviewer: None,
            }),
            Some("a_old"),
        );
    }
    history.freeze();
    let mut expected = vec!["session_started".to_owned()];
    expected.extend(vec!["permission_resolved".to_owned(); 20]);
    assert_eq!(history.history_kinds(), expected);

    let reviewer = Arc::new(ScriptedProvider::new(vec![
        Scripted::text("check"),
        Scripted::text("block: it writes"),
    ]));
    let (tx, rx) = mpsc::channel();
    let looped = Loop::resume(
        Arc::clone(&history.log),
        r#loop::resumed(&history.dir).unwrap(),
        Arc::clone(&history.provider) as Arc<dyn Provider>,
        History::model(),
        history.prompt(),
        rx,
        vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)],
        r#loop::Permissions {
            workspace: history.workspace.clone(),
            credentials: history.credentials.clone(),
            rules: history.rules.clone(),
        },
    )
    .unwrap()
    .answerable(false)
    .reviewer(
        Ok(r#loop::Reviewer {
            provider: reviewer,
            model: Model {
                reference: "fake/reviewer-1".into(),
                cost: None,
                subscription: false,
            },
        }),
        r#loop::BlockLimits::default(),
    );
    history.inbox_tx = tx;
    let outcome = history.run(looped, "two");

    // The first counted block denies the call; the turn completes.
    assert_eq!(outcome, contract::events::TurnOutcome::Completed);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn the_reviewers_first_request_contains_the_earlier_tool_calls() {
    let mut history = History::new(vec![
        support::tool_call_reply("Go.", &["exec"]),
        Scripted::text("Done."),
    ]);
    let mut tool = support::TestTool::declaring(
        "exec",
        "done",
        vec![contract::shapes::Effect::Executes],
        None,
    );
    tool.subject = Some("run tests".into());
    let tool = Arc::new(tool);
    history.write(user_turn("one"), None);
    history.write(requested("read"), Some("a_1"));
    history.write(completed("Paris."), Some("a_1"));
    history.freeze();
    assert_eq!(
        history.history_kinds(),
        [
            "session_started",
            "turn_started",
            "tool_call_requested",
            "tool_call_completed",
        ]
    );

    let reviewer = Arc::new(ScriptedProvider::new(vec![Scripted::text("allow")]));
    let reviewer_provider = Arc::clone(&reviewer);
    let (tx, rx) = mpsc::channel();
    let looped = Loop::resume(
        Arc::clone(&history.log),
        r#loop::resumed(&history.dir).unwrap(),
        Arc::clone(&history.provider) as Arc<dyn Provider>,
        History::model(),
        history.prompt(),
        rx,
        vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)],
        r#loop::Permissions {
            workspace: history.workspace.clone(),
            credentials: history.credentials.clone(),
            rules: history.rules.clone(),
        },
    )
    .unwrap()
    .reviewer(
        Ok(r#loop::Reviewer {
            provider: reviewer,
            model: Model {
                reference: "fake/reviewer-1".into(),
                cost: None,
                subscription: false,
            },
        }),
        r#loop::BlockLimits::default(),
    );
    history.inbox_tx = tx;
    let outcome = history.run(looped, "two");

    assert_eq!(outcome, contract::events::TurnOutcome::Completed);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let requests = reviewer_provider.requests();
    assert_eq!(requests.len(), 1);
    // The reviewer's key is the session's id plus `reviewer`.
    assert_eq!(requests[0].cache_key, "s_1:reviewer");
    let conversation = &requests[0].conversation;
    assert!(matches!(&conversation[0], Input::User { text } if text == "The person: one"));
    assert!(
        matches!(&conversation[1], Input::User { text } if text.contains("\"read\"")),
        "{conversation:?}"
    );
}

#[test]
fn sent_counts_the_fixed_results_flushed_at_the_last_request() {
    // a1 is requested and never completed; the dead process's second
    // `assistant_message_started` flushes its fixed result into the rebuilt
    // conversation, so `sent` counts it.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(requested("read"), Some("a_1"));
    history.write(message_started(), Some("a_0"));
    history.freeze();
    assert_eq!(
        history.history_kinds(),
        [
            "session_started",
            "turn_started",
            "tool_call_requested",
            "assistant_message_started",
        ]
    );

    let looped = history.resume(Vec::new());
    history.run(looped, "two");

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    assert_eq!(conversation.len(), 5);
    let (id, text, _) = result_of(&conversation[3]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "It never ran.");
    assert_eq!(requests[0].previous_end, Some(3));
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_resume_builds_the_preamble_with_reason_resume() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.freeze();

    let looped = history.resume(Vec::new());
    history.run(looped, "two");

    let new = history.new_lines();
    assert_eq!(new[0].kind, "preamble_built");
    assert_eq!(new[0].payload["reason"], "resume");
    assert_eq!(new[0].payload["model"], support::MODEL);
    // The resumed request carries the rebuilt prompt.
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(new[0].payload["system_prompt"], requests[0].system_prompt);
}

#[test]
fn a_resume_over_a_log_with_an_opening_message_writes_none() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    // The log already holds an opening message, written before the turn
    // and rendered from its logged fields only: `test-os` never ran, so
    // the conversation proves it.
    history.write(
        Event::OpeningMessage(OpeningMessage {
            environment: Environment {
                date: "2023-11-14".into(),
                os: "test-os".into(),
                arch: "test-arch".into(),
                shell: "/bin/sh".into(),
                workspace: history.workspace.clone(),
                git: None,
                session_log: "test-log".into(),
            },
            instruction_files: Vec::new(),
            extension_sections: Vec::new(),
            skills: Vec::new(),
        }),
        None,
    );
    history.write(user_turn("one"), None);
    history.freeze();

    let looped = history.resume(Vec::new());
    history.run(looped, "two");

    assert!(
        history
            .new_lines()
            .iter()
            .all(|line| line.kind != "opening_message"),
        "{:?}",
        history.new_kinds()
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    assert!(matches!(&conversation[0], Input::User { text } if text.contains("test-os")));
    assert!(matches!(&conversation[1], Input::User { text } if text == "one"));
    assert!(matches!(&conversation[2], Input::User { text } if text == "two"));
}

#[test]
fn no_request_before_the_resume_leaves_previous_end_absent() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.freeze();
    assert_eq!(history.history_kinds(), ["session_started", "turn_started"]);

    let looped = history.resume(Vec::new());
    history.run(looped, "two");

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].previous_end, None);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn resumed_returns_the_first_workspace_and_the_last_model() {
    let history = History::new(vec![]);
    history.write(History::usage("g1", "fake/first", Some(1.0)), Some("a_1"));
    history.write(History::usage("g2", "fake/second", Some(2.0)), Some("a_2"));

    let lines = history.lines();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["session_started", "usage_recorded", "usage_recorded"]
    );
    let resumed = r#loop::resumed(&history.dir).unwrap();
    assert_eq!(resumed.session, "s_1");
    assert_eq!(resumed.workspace, history.workspace);
    assert_eq!(resumed.model.as_deref(), Some("fake/second"));
}

#[test]
fn resumed_returns_no_model_for_a_log_with_none() {
    let history = History::new(vec![]);
    history.write(user_turn("one"), None);

    let lines = history.lines();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["session_started", "turn_started"]
    );
    let resumed = r#loop::resumed(&history.dir).unwrap();
    assert_eq!(resumed.session, "s_1");
    assert_eq!(resumed.workspace, history.workspace);
    assert_eq!(resumed.model, None);
}

fn preamble(credential: Option<&str>) -> Event {
    Event::PreambleBuilt(contract::events::PreambleBuilt {
        reason: contract::events::PreambleReason::Start,
        model: MODEL.into(),
        context_window: 0,
        trigger_at: None,
        effort: None,
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: contract::events::CacheLifetime::OneHour,
        credential: credential.map(str::to_owned),
        system_prompt: String::new(),
        tools: Vec::new(),
        replaced: Vec::new(),
    })
}

#[test]
fn resumed_returns_the_credential_label_of_the_last_preamble() {
    let history = History::new(vec![]);
    history.write(preamble(Some("work")), None);
    history.write(preamble(Some("personal")), None);
    history.write(user_turn("one"), None);
    let resumed = r#loop::resumed(&history.dir).unwrap();
    assert_eq!(resumed.credential.as_deref(), Some("personal"));
}

#[test]
fn resumed_returns_no_credential_for_a_log_with_no_label() {
    let history = History::new(vec![]);
    assert_eq!(r#loop::resumed(&history.dir).unwrap().credential, None);
    history.write(preamble(None), None);
    assert_eq!(r#loop::resumed(&history.dir).unwrap().credential, None);
}

#[test]
fn resumed_fails_log_corrupt_on_a_log_with_no_session_started() {
    let root = fakes::TempDir::new("fiber-resume");
    let log = Log::create(
        root.path(),
        SessionId("s_9".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(
        &user_turn("one"),
        Some(contract::TurnId("t_1".into())),
        None,
    )
    .unwrap();
    let dir = root.path().join("s_9");
    let lines = log::read(&dir).unwrap();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["turn_started"]
    );

    let error = match r#loop::resumed(&dir) {
        Ok(_) => panic!("a log with no session_started resumes"),
        Err(error) => error,
    };
    assert_eq!(error.code(), ErrorCode::LogCorrupt);
}

/// Appends `line` to the log in `dir` as raw JSON, past the writer: a line
/// whose envelope reads and whose payload need not.
fn append_raw(dir: &std::path::Path, line: &Envelope) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(line).unwrap()).unwrap();
}

/// A `text_completed` line at `seq` whose payload does not read as its kind.
fn unreadable_text(seq: u64) -> Envelope {
    Envelope {
        kind: "text_completed".into(),
        session_id: SessionId("s_9".into()),
        ts: 1,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: Some(contract::TurnId("t_1".into())),
        action_id: Some(ActionId("a_0".into())),
        seq: Some(contract::Seq(seq)),
        payload: json!({"text": 7}).as_object().unwrap().clone(),
    }
}

#[test]
fn resume_fails_log_corrupt_on_an_unreadable_line_in_its_window() {
    // The pass reads no `text_completed` payload, so the line passes it; the
    // window's rebuild reads it and refuses the resume.
    let root = fakes::TempDir::new("fiber-resume");
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let created = Log::create(root.path(), SessionId("s_9".into()), Arc::clone(&clock)).unwrap();
    created
        .append(
            &Event::SessionStarted(SessionStarted {
                workspace: root.path().display().to_string(),
                variables: Variables {
                    path: String::new(),
                    names: Vec::new(),
                    source: VariablesSource::Inherited,
                },
                parent: None,
                forked_from: None,
                rewind: None,
            }),
            None,
            None,
        )
        .unwrap();
    created
        .append(&user_turn("one"), Some(contract::TurnId("t_1".into())), None)
        .unwrap();
    drop(created);
    let dir = root.path().join("s_9");
    append_raw(&dir, &unreadable_text(2));
    let log = Arc::new(Log::open(root.path(), SessionId("s_9".into()), clock).unwrap());
    let resumed = r#loop::resumed(&dir).unwrap();
    let (_tx, rx) = mpsc::channel();

    let error = match Loop::resume(
        log,
        resumed,
        Arc::new(ScriptedProvider::new(vec![])) as Arc<dyn Provider>,
        History::model(),
        resume_prompt(root.path()),
        rx,
        Vec::new(),
        r#loop::Permissions {
            workspace: root.path().display().to_string(),
            credentials: root.path().to_path_buf(),
            rules: Arc::new(support::FakeRules::empty()),
        },
    ) {
        Ok(_) => panic!("a window with an unreadable line resumes"),
        Err(error) => error,
    };
    assert_eq!(error.code(), ErrorCode::LogCorrupt);
}

// #302 part 3: re-raising a pending approval on resume.

use std::sync::Mutex;

use contract::events::{
    AskStep, Escalation, FiberExited, FiberStarted, Interaction, InteractionRequested,
    PermissionRequested, RuleOffer, RuleScope, StandingRule, TurnCompleted,
    TurnOutcome as CompletedOutcome,
};
use contract::inbox::{Ack, Answer};
use contract::shapes::{Effect, Usage};

/// A standing-ask approval for `action`, carrying `request_id`.
fn standing_request(request_id: &str) -> Event {
    Event::PermissionRequested(PermissionRequested {
        request_id: contract::RequestId(request_id.into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: true,
            paths: None,
        },
        step: AskStep::StandingAsk {
            standing_rule: StandingRule {
                scope: RuleScope::Project,
                prefix: "run tests".into(),
            },
        },
    })
}

/// A review approval for `action`, carrying `request_id`, an escalation and
/// a rule offer.
fn review_request(request_id: &str) -> Event {
    Event::PermissionRequested(PermissionRequested {
        request_id: contract::RequestId(request_id.into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: true,
            paths: None,
        },
        step: AskStep::Review {
            escalation: Some(Escalation::ConsecutiveBlocks {
                reason: "it writes".into(),
            }),
            rule: Some(RuleOffer {
                subject: "run tests".into(),
                prefix: "run tests".into(),
            }),
        },
    })
}

fn fiber_started() -> Event {
    Event::FiberStarted(FiberStarted {
        version: "0.0.0".into(),
        resumed: false,
    })
}

fn fiber_exited(suspended_on: Option<&str>) -> Event {
    Event::FiberExited(FiberExited {
        exit_code: 0,
        usage: Usage {
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        final_message: None,
        error: None,
        suspended_on: suspended_on.map(|id| contract::RequestId(id.into())),
        questions: None,
    })
}

fn turn_completed() -> Event {
    Event::TurnCompleted(TurnCompleted {
        outcome: CompletedOutcome::Completed,
        error: None,
        questions: None,
    })
}

fn denied_resolved(request_id: Option<&str>) -> Event {
    Event::PermissionResolved(PermissionResolved {
        request_id: request_id.map(|id| contract::RequestId(id.into())),
        decision: Decision::Deny,
        decided_by: DecidedBy::StandingRule,
        reason: Some("No person can answer an approval in this session.".into()),
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
    })
}

fn allowed_resolved() -> Event {
    Event::PermissionResolved(PermissionResolved {
        request_id: None,
        decision: Decision::Allow,
        decided_by: DecidedBy::Person,
        reason: None,
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
    })
}

fn confirm_requested(request_id: &str) -> Event {
    Event::InteractionRequested(InteractionRequested {
        request_id: contract::RequestId(request_id.into()),
        interaction: Interaction::Confirm {
            prompt: "Proceed?".into(),
        },
        action_ids: None,
        extension: None,
    })
}

/// An acknowledgement that records its answer.
fn recording() -> (Ack, Arc<Mutex<Option<Answer>>>) {
    let seen: Arc<Mutex<Option<Answer>>> = Arc::new(Mutex::new(None));
    let back = Arc::clone(&seen);
    let ack = Ack(Box::new(move |answer| {
        *back.lock().unwrap() = Some(answer);
    }));
    (ack, seen)
}

fn is_accepted(seen: &Arc<Mutex<Option<Answer>>>) -> bool {
    matches!(&*seen.lock().unwrap(), Some(Ok(_)))
}

impl History {
    /// Resumes with `tools` as an unattended session answers: no person
    /// can answer an approval.
    fn resume_headless(&mut self, tools: Vec<(String, Arc<dyn Tool>)>) -> Loop {
        let provider = Arc::clone(&self.provider) as Arc<dyn Provider>;
        self.resume_headless_on(provider, tools)
    }

    /// As [`History::resume_headless`], its model calls reaching `provider`.
    fn resume_headless_on(
        &mut self,
        provider: Arc<dyn Provider>,
        tools: Vec<(String, Arc<dyn Tool>)>,
    ) -> Loop {
        Loop::resume(
            Arc::clone(&self.log),
            r#loop::resumed(&self.dir).unwrap(),
            provider,
            Self::model(),
            self.prompt(),
            self.inbox_rx.take().unwrap(),
            tools,
            r#loop::Permissions {
                workspace: self.workspace.clone(),
                credentials: self.credentials.clone(),
                rules: self.rules.clone(),
            },
        )
        .unwrap()
        .answerable(false)
    }

    /// Runs one turn on its own thread, returning the loop for the next
    /// turn, and failing the test past [`support::DEADLINE`].
    fn step(&mut self, mut looped: Loop) -> (Loop, Option<contract::events::TurnOutcome>) {
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = looped.turn().unwrap();
            done.send((looped, outcome)).unwrap();
        });
        finished
            .recv_timeout(support::DEADLINE)
            .expect("the turn ended in time")
    }
}

/// A suspended history: one turn whose batch is `[a_1]` with a standing
/// ask `r_9` pending on it, then the process exited on that request.
fn suspended_history(script: Vec<Scripted>) -> History {
    let mut history = History::new(script);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    history
}

#[test]
fn an_open_batch_gets_no_fixed_result() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(requested("search"), Some("a_2"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    // The finishing turn's request holds both calls and no prompt, and
    // neither call has a fixed result: the finishing turn completes them.
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    let calls: Vec<&str> = conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolCall { action_id, .. } => Some(action_id.0.as_str()),
            Input::ToolResult { .. }
            | Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. } => None,
        })
        .collect();
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(calls, ["a_1", "a_2"]);
    for input in conversation {
        if let Input::ToolResult { text, .. } = input {
            assert!(
                !text.contains("never ran") && !text.contains("unknown"),
                "{text}"
            );
        }
    }
}

#[test]
fn without_a_suspend_the_same_batch_gets_fixed_results() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(requested("search"), Some("a_2"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let fixed: Vec<(&str, &str)> = requests[0]
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolResult {
                action_id, text, ..
            } if text.contains("never ran") => Some((action_id.0.as_str(), text.as_str())),
            Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. } => None,
        })
        .collect();
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(fixed.len(), 2);
    assert_eq!(fixed[0].0, "a_1");
    assert_eq!(fixed[1].0, "a_2");
}

#[test]
fn a_call_outside_the_open_batch_still_gets_its_fixed_result() {
    // `a_0`'s call sits before the last `assistant_message_started`, so it
    // is outside the suspended batch; only it gets a fixed result.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(requested("read"), Some("a_0"));
    history.write(message_started(), Some("a_9"));
    history.write(requested("read"), Some("a_1"));
    history.write(requested("search"), Some("a_2"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let fixed: Vec<&str> = requests[0]
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolResult {
                action_id, text, ..
            } if text.contains("never ran") => Some(action_id.0.as_str()),
            Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. } => None,
        })
        .collect();
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(fixed, ["a_0"]);
}

#[test]
fn no_suspended_on_resumes_as_a_cut_short_turn() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(None), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    // Part 1's behaviour: no re-raise, the call keeps its fixed result.
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].conversation.iter().any(|input| matches!(
            input,
            Input::ToolResult { text, .. } if text.contains("never ran")
        )),
        "{:?}",
        requests[0].conversation
    );
}

#[test]
fn a_suspended_on_naming_an_unknown_request_resumes_as_cut_short() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_unknown")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_suspended_on_naming_an_interaction_resumes_as_cut_short() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(confirm_requested("r_7"), None);
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_7")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    // Only approvals re-raise.
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
}

#[test]
fn an_already_resolved_request_resumes_as_cut_short() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(denied_resolved(Some("r_9")), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert_eq!(
        history
            .new_kinds()
            .iter()
            .filter(|k| *k == "permission_requested")
            .count(),
        0
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_line_after_fiber_exited_resumes_as_cut_short() {
    // A resumed process started a new turn and died: the last line is not
    // `fiber_exited`, so nothing re-raises.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.write(user_turn("two"), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(
            support::message("three"),
            support::ignore(),
        ))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_completed_turn_resumes_as_cut_short() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(turn_completed(), None);
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_request_outside_the_batch_resumes_as_cut_short() {
    // The request's action `a_9` was never requested after the last
    // `assistant_message_started`: the log is treated as cut short.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_9"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_suspended_turn_is_refused_then_the_prompt_runs_next() {
    let mut history = suspended_history(vec![Scripted::text("Hello."), Scripted::text("Second.")]);
    let (prompt_ack, prompt_seen) = recording();
    let (close_ack, close_seen) = recording();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), prompt_ack))
        .unwrap();
    history.inbox_tx.send(Delivery::Close(close_ack)).unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));
    let (looped, prompted) = history.step(looped);
    assert_eq!(prompted, Some(contract::events::TurnOutcome::Completed));
    let (_looped, closed) = history.step(looped);
    assert_eq!(closed, None);

    // Both deliveries were accepted: the prompt waited behind the
    // finishing turn instead of being rejected `busy`.
    assert!(is_accepted(&prompt_seen), "the prompt was accepted");
    assert!(is_accepted(&close_seen), "close was accepted");

    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let new = history.new_lines();
    // The re-raised request keeps its `request_id` on both lines.
    assert_eq!(new[2].payload["request_id"], "r_9");
    assert_eq!(new[3].payload["request_id"], "r_9");
    assert_eq!(new[3].payload["decision"], "deny");
    assert_eq!(new[3].payload["decided_by"], "standing_rule");
    assert_eq!(new[4].payload["status"], "denied");
    // The finished turn's `turn_completed` carries the original turn id,
    // and every line of it does.
    assert_eq!(new[2].turn_id.as_ref().unwrap().0, "t_1");
    assert_eq!(new[10].turn_id.as_ref().unwrap().0, "t_1");
    assert_eq!(new[10].payload["outcome"], "completed");
    for line in &new[2..=10] {
        assert_eq!(
            line.turn_id.as_ref().unwrap().0,
            "t_1",
            "every finished-turn line carries the original turn id: {}",
            line.kind
        );
    }
    // No resumed code path starts a call from before the resume.
    assert!(
        !new.iter().any(|l| l.kind == "tool_call_started"),
        "no tool_call_started"
    );

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 2);
    // The first request holds the call and the denial, and no prompt.
    let first = &requests[0].conversation;
    assert!(matches!(&first[2], Input::ToolCall { .. }), "{first:?}");
    assert!(
        matches!(&first[3], Input::ToolResult { text, .. } if text.contains("No person can answer")),
        "{first:?}"
    );
    assert!(
        !first
            .iter()
            .any(|input| matches!(input, Input::User { text } if text == "two")),
        "{first:?}"
    );
    // The second request holds the denial and then the prompt.
    let second = &requests[1].conversation;
    assert!(
        second.iter().any(
            |input| matches!(input, Input::ToolResult { text, .. } if text.contains("No person can answer"))
        ),
        "{second:?}"
    );
    assert!(
        matches!(second.last().unwrap(), Input::User { text } if text == "two"),
        "{second:?}"
    );
}

#[test]
fn a_three_call_batch_completes_in_request_order() {
    let mut history = History::new(vec![Scripted::text("Hello."), Scripted::text("Second.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(requested("exec"), Some("a_2"));
    history.write(requested("search"), Some("a_3"));
    // The first call was allowed in the log; the second is pending.
    history.write(allowed_resolved(), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_2"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let (prompt_ack, prompt_seen) = recording();
    let (close_ack, _) = recording();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), prompt_ack))
        .unwrap();
    history.inbox_tx.send(Delivery::Close(close_ack)).unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));
    let (_looped, prompted) = history.step(looped);
    assert_eq!(prompted, Some(contract::events::TurnOutcome::Completed));
    assert!(is_accepted(&prompt_seen));

    let new = history.new_lines();
    assert_eq!(new[2].kind, "permission_requested");
    assert_eq!(new[3].kind, "permission_resolved");
    let completions: Vec<(&str, &str)> = new[4..7]
        .iter()
        .map(|line| {
            assert_eq!(line.kind, "tool_call_completed");
            (
                line.action_id.as_ref().unwrap().0.as_str(),
                line.payload["status"].as_str().unwrap(),
            )
        })
        .collect();
    // In request order: the allowed call is cancelled all the same, the
    // pending one denied, the third cancelled. None started.
    assert_eq!(
        completions,
        [
            ("a_1", "cancelled"),
            ("a_2", "denied"),
            ("a_3", "cancelled")
        ]
    );
    assert!(!new.iter().any(|l| l.kind == "tool_call_started"));
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_review_request_re_raises_with_its_escalation_and_offer() {
    let mut history = History::new(vec![Scripted::text("Hello."), Scripted::text("Second.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("exec"), Some("a_1"));
    history.write(review_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    let before = history
        .lines()
        .iter()
        .find(|l| l.kind == "permission_requested")
        .unwrap()
        .payload
        .clone();

    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();
    let looped = history.resume_headless(Vec::new());
    let (looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    let new = history.new_lines();
    assert_eq!(new[2].kind, "permission_requested");
    assert_eq!(new[2].payload["request_id"], "r_9");
    assert_eq!(new[2].payload, before);
    assert_eq!(new[2].payload["step"], "review");
    assert!(new[2].payload.get("escalation").is_some());
    assert!(new[2].payload.get("rule").is_some());
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_tool_called_in_the_finishing_turn_runs() {
    let tool = Arc::new(support::TestTool::reads("read", "Paris."));
    let mut history = History::new(vec![
        support::tool_call_reply("Go.", &["read"]),
        Scripted::text("Done."),
        Scripted::text("Second."),
    ]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("exec"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();
    let looped = history.resume_headless(vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)]);
    let (looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    // The new call ran to completion inside the finishing turn.
    assert_eq!(tool.ran().len(), 1);
    let new = history.new_lines();
    let ran: Vec<&str> = new
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .map(|l| l.payload["status"].as_str().unwrap())
        .collect();
    // The suspended call denied, the new one completed.
    assert_eq!(ran, ["denied", "completed"]);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_completed_call_in_the_window_is_not_in_the_batch() {
    // `a_0` was requested and completed after the last
    // `assistant_message_started`; only the pending `a_1` is finished.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_9"));
    history.write(requested("read"), Some("a_0"));
    history.write(completed("Paris."), Some("a_0"));
    history.write(requested("exec"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    // Exactly one new completion, for the pending call: the completed
    // call keeps its single result.
    let new = history.new_lines();
    let completions: Vec<&str> = new
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .map(|l| l.action_id.as_ref().unwrap().0.as_str())
        .collect();
    assert_eq!(completions, ["a_1"]);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let results: Vec<(&str, &str)> = requests[0]
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolResult {
                action_id, text, ..
            } => Some((action_id.0.as_str(), text.as_str())),
            Input::ToolCall { .. }
            | Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. } => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, "a_0");
    assert_eq!(results[0].1, "Paris.");
    assert_eq!(results[1].0, "a_1");
    assert!(results[1].1.contains("No person can answer"));
}

// Orphans: a job a crash left running is marked on open.

fn job_started(id: &str) -> Event {
    Event::JobStarted(contract::events::JobStarted {
        job_id: contract::JobId(id.into()),
        tool: Some("shell".into()),
        extension: None,
        description: "npm test".into(),
        output_path: format!("artifacts/{id}.log"),
    })
}

fn job_completed(id: &str) -> Event {
    Event::JobCompleted(contract::events::JobCompleted {
        job_id: contract::JobId(id.into()),
        status: contract::events::Outcome::Completed,
        error: None,
        process: None,
        output_tail: None,
    })
}

const ORPHANED: &str = "The process that ran this job died; it may still be running.";

/// The `orphaned` completion a resume writes for `id`.
fn assert_orphaned(line: &Envelope, id: &str) {
    assert_eq!(line.kind, "job_completed");
    assert_eq!(line.turn_id, None);
    assert_eq!(line.action_id, None);
    assert_eq!(
        serde_json::Value::Object(line.payload.clone()),
        json!({
            "job_id": id,
            "status": "failed",
            "error": {"code": "orphaned", "message": ORPHANED},
        })
    );
}

#[test]
fn a_job_with_no_completion_is_marked_orphaned_on_resume() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(requested("shell"), Some("a_1"));
    history.write(job_started("j_a"), Some("a_1"));
    history.write(job_started("j_b"), Some("a_1"));
    history.write(job_started("j_c"), Some("a_1"));
    // As a `jobs wait` records it, under its call's action.
    history.write(job_completed("j_b"), Some("a_1"));
    history.write(completed("Started."), Some("a_1"));
    history.freeze();

    let looped = history.resume(Vec::new());
    // Written on open, before any turn, in start order.
    let marked = history.new_lines();
    assert_eq!(kinds_of(&marked), ["job_completed", "job_completed"]);
    assert_orphaned(&marked[0], "j_a");
    assert_orphaned(&marked[1], "j_c");
    let last = history.lines()[history.history_len - 1].seq.unwrap().0;
    assert_eq!(marked[0].seq.unwrap().0, last + 1);

    assert_eq!(
        history.run(looped, "two"),
        contract::events::TurnOutcome::Completed
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let users: Vec<&str> = requests[0]
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text } => Some(text.as_str()),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. } => None,
        })
        .collect();
    let notice = |id: &str| format!("Fiber: background job {id} ended: failed.\n{ORPHANED}");
    assert_eq!(users[2], notice("j_a"));
    assert_eq!(users[3], notice("j_c"));
    assert_eq!(*users.last().unwrap(), "two");
    assert_eq!(
        history.new_kinds(),
        [
            "job_completed",
            "job_completed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_job_a_rewind_handed_on_is_not_marked() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(requested("shell"), Some("a_1"));
    history.write(job_started("j_a"), Some("a_1"));
    history.write(completed("Started."), Some("a_1"));
    history
        .log
        .append(
            &Event::Rewound(contract::events::Rewound {
                new_session_id: SessionId("s_2".into()),
                seq: contract::Seq(1),
                jobs: vec![contract::JobId("j_a".into())],
            }),
            None,
            None,
        )
        .unwrap();
    history.freeze();

    let looped = history.resume(Vec::new());
    assert!(history.new_lines().is_empty());
    drop(looped);
}

#[test]
fn a_job_with_a_completion_is_not_marked() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(job_started("j_a"), Some("a_1"));
    history.write(job_completed("j_a"), None);
    history.freeze();

    let looped = history.resume(Vec::new());
    assert!(history.new_lines().is_empty());
    drop(looped);
}

#[test]
fn an_orphan_behind_a_suspended_batch_renders_after_its_results() {
    // The orphan line is written on open, as on every resume; the notice
    // joins the conversation after the open batch's results, live and on
    // rebuild alike, so no message separates a call from its result.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_9"));
    history.write(requested("shell"), Some("a_0"));
    history.write(job_started("j_a"), Some("a_0"));
    history.write(completed("Started j_a."), Some("a_0"));
    history.write(requested("exec"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless(Vec::new());
    let marked = history.new_lines();
    assert_eq!(kinds_of(&marked), ["job_completed"]);
    assert_orphaned(&marked[0], "j_a");
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    drop(looped);

    assert_eq!(
        history.new_kinds(),
        [
            "job_completed",
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    assert!(matches!(
        conversation[conversation.len() - 2],
        Input::ToolResult { ref action_id, .. } if action_id.0 == "a_1"
    ));
    assert_eq!(
        conversation.last(),
        Some(&Input::User {
            text: format!("Fiber: background job j_a ended: failed.\n{ORPHANED}")
        })
    );
    // A later resume renders the same conversation from the log.
    let rebuilt = r#loop::rebuild(&history.lines(), MODEL).unwrap();
    assert_eq!(&rebuilt[..conversation.len()], conversation.as_slice());
}

#[test]
fn a_second_resume_over_a_logged_orphan_keeps_it_after_the_results() {
    // The first resume logs the orphan and the process exits again on the
    // same request; the second resume reads the orphan from the log and
    // still sends it after the open batch's results.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_9"));
    history.write(requested("shell"), Some("a_0"));
    history.write(job_started("j_a"), Some("a_0"));
    history.write(completed("Started j_a."), Some("a_0"));
    history.write(requested("exec"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);

    drop(history.resume_headless(Vec::new()));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    let (tx, rx) = mpsc::channel();
    history.inbox_tx = tx;
    history.inbox_rx = Some(rx);
    let looped = history.resume_headless(Vec::new());
    // The orphan is logged once.
    assert!(history.new_lines().is_empty());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    drop(looped);

    let lines = history.lines();
    assert_eq!(
        lines.iter().filter(|l| l.kind == "job_completed").count(),
        1
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    let rebuilt = r#loop::rebuild(&lines, MODEL).unwrap();
    assert_eq!(&rebuilt[..conversation.len()], conversation.as_slice());
    // The notice follows the open call's result. (The opening message the
    // finishing turn writes between a_1 and its result is #696's.)
    let result = conversation
        .iter()
        .position(
            |input| matches!(input, Input::ToolResult { action_id, .. } if action_id.0 == "a_1"),
        )
        .unwrap();
    assert_eq!(result, conversation.len() - 2);
    assert_eq!(
        conversation.last(),
        Some(&Input::User {
            text: format!("Fiber: background job j_a ended: failed.\n{ORPHANED}")
        })
    );
}

// Handoff windows (`docs/handoff.md`, "Resume"): the rebuild renders what a
// handoff leaves in force.

fn opening_of(os: &str) -> Event {
    Event::OpeningMessage(OpeningMessage {
        environment: Environment {
            date: "2023-11-14".into(),
            os: os.into(),
            arch: "test-arch".into(),
            shell: "/bin/sh".into(),
            workspace: "/w".into(),
            git: None,
            session_log: "/log/events.jsonl".into(),
        },
        instruction_files: Vec::new(),
        extension_sections: Vec::new(),
        skills: Vec::new(),
    })
}

fn handoff_started() -> Event {
    Event::HandoffStarted(contract::events::HandoffStarted {
        trigger: contract::events::HandoffTrigger::Auto,
    })
}

fn handoff_done(outcome: contract::events::Outcome, note: &[&str]) -> Event {
    let failed = outcome == contract::events::Outcome::Failed;
    Event::HandoffCompleted(contract::events::HandoffCompleted {
        outcome,
        error: failed.then(|| contract::shapes::Failure {
            code: contract::ErrorCode::RateLimited,
            message: "slow down".into(),
            retry_after: None,
            provider: None,
        }),
        note: (!note.is_empty()).then(|| contract::events::Note::Actions {
            note: note.iter().map(|id| ActionId((*id).into())).collect(),
        }),
        tokens_before: 400_120,
        instructions: None,
    })
}

fn text_of(input: &Input) -> &str {
    match input {
        Input::User { text } | Input::Assistant { text, .. } => text,
        other @ (Input::Reasoning { .. } | Input::ToolCall { .. } | Input::ToolResult { .. }) => {
            panic!("not a message: {other:?}")
        }
    }
}

fn texts(conversation: &[Input]) -> Vec<&str> {
    conversation.iter().map(text_of).collect()
}

/// A log: an opening message, the turn's input and a steer, then (when
/// `jobs`) a job still running and one that ended.
fn before_handoff(log: &LogLines, jobs: bool) {
    log.append(opening_of("old-os"), None);
    if jobs {
        log.append(job_started("j_1"), None);
        log.append(job_started("j_2"), None);
        log.append(job_completed("j_2"), None);
    }
    log.append(user_turn("one"), None);
    log.append(steering("steer"), None);
}

/// The note request's lines under the message action `a_note`.
fn note_lines(log: &LogLines) {
    log.append(handoff_started(), None);
    log.append(message_started(), Some("a_note"));
    log.append(assistant("the note"), Some("a_note"));
}

const JOBS_LINE: &str = "Fiber: these background jobs are still running. Each one's end is reported when it happens.\n\n- j_1: npm test";

#[test]
fn a_completed_handoff_is_the_opening_the_turn_input_the_note_and_the_jobs_line() {
    let log = LogLines::new();
    before_handoff(&log, true);
    note_lines(&log);
    log.append(
        handoff_done(contract::events::Outcome::Completed, &["a_note"]),
        None,
    );
    log.append(opening_of("new-os"), None);

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    let seen = texts(&conversation);
    assert_eq!(seen.len(), 5, "{seen:?}");
    assert!(seen[0].contains("new-os"), "{seen:?}");
    assert_eq!(seen[1..], ["one", "steer", "the note", JOBS_LINE]);
}

#[test]
fn a_completed_handoff_with_no_job_running_has_no_jobs_line() {
    let log = LogLines::new();
    before_handoff(&log, false);
    log.append(job_started("j_3"), None);
    log.append(job_completed("j_3"), None);
    note_lines(&log);
    log.append(
        handoff_done(contract::events::Outcome::Completed, &["a_note"]),
        None,
    );
    log.append(opening_of("new-os"), None);

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    assert_eq!(texts(&conversation)[1..], ["one", "steer", "the note"]);
}

#[test]
fn a_handoff_carries_only_the_latest_turns_input() {
    let log = LogLines::new();
    before_handoff(&log, false);
    log.append(assistant("answer"), Some("a_1"));
    log.append(user_turn("two"), None);
    note_lines(&log);
    log.append(
        handoff_done(contract::events::Outcome::Completed, &["a_note"]),
        None,
    );
    log.append(opening_of("new-os"), None);

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    // The first turn's "one" and its steer stay behind in the log.
    assert_eq!(texts(&conversation)[1..], ["two", "the note"]);
}

/// The conversation before `handoff_started`.
fn before(jobs: bool) -> Vec<Input> {
    let log = LogLines::new();
    before_handoff(&log, jobs);
    r#loop::rebuild(&log.lines(), MODEL).unwrap()
}

#[test]
fn a_failed_handoff_leaves_the_conversation_as_it_was() {
    let log = LogLines::new();
    before_handoff(&log, true);
    note_lines(&log);
    log.append(handoff_done(contract::events::Outcome::Failed, &[]), None);

    assert_eq!(r#loop::rebuild(&log.lines(), MODEL).unwrap(), before(true));
}

#[test]
fn a_cancelled_handoff_leaves_the_conversation_as_it_was() {
    let log = LogLines::new();
    before_handoff(&log, true);
    note_lines(&log);
    log.append(
        handoff_done(contract::events::Outcome::Cancelled, &[]),
        None,
    );

    assert_eq!(r#loop::rebuild(&log.lines(), MODEL).unwrap(), before(true));
}

#[test]
fn a_handoff_that_never_completed_leaves_the_conversation_as_it_was() {
    let log = LogLines::new();
    before_handoff(&log, true);
    note_lines(&log);

    assert_eq!(r#loop::rebuild(&log.lines(), MODEL).unwrap(), before(true));
}

#[test]
fn a_turn_resumed_after_an_unfinished_handoff_is_never_discarded() {
    let log = LogLines::new();
    before_handoff(&log, false);
    note_lines(&log);
    log.append(fiber_started(), None);
    log.append(user_turn("three"), None);
    log.append(assistant("answer"), Some("a_9"));

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    let mut expected = before(false);
    expected.push(Input::User {
        text: "three".into(),
    });
    expected.push(Input::Assistant {
        model: MODEL.into(),
        text: "answer".into(),
        provider_item: None,
    });
    assert_eq!(conversation, expected);
}

#[test]
fn a_handoff_window_closes_at_each_completion_even_in_a_later_window() {
    let log = LogLines::new();
    before_handoff(&log, false);
    note_lines(&log);
    log.append(handoff_done(contract::events::Outcome::Failed, &[]), None);
    log.append(user_turn("two"), None);
    log.append(handoff_started(), None);
    log.append(message_started(), Some("a_note2"));
    log.append(assistant("second note"), Some("a_note2"));
    log.append(
        handoff_done(contract::events::Outcome::Completed, &["a_note2"]),
        None,
    );
    log.append(opening_of("new-os"), None);

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    // The first note's text never reaches the second window's note.
    assert_eq!(texts(&conversation)[1..], ["two", "second note"]);
}

#[test]
fn a_note_call_cut_short_by_a_crash_leaves_no_result_behind() {
    let log = LogLines::new();
    before_handoff(&log, false);
    note_lines(&log);
    // The process died after logging the note reply's call, before its
    // completion; a new process resumed and took a turn.
    log.append(requested("read"), Some("a_call"));
    log.append(fiber_started(), None);
    log.append(user_turn("two"), None);

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    let mut expected = before(false);
    expected.push(Input::User { text: "two".into() });
    assert_eq!(conversation, expected);
}

#[test]
fn the_nudge_renders_with_the_openings_session_log_path() {
    let log = LogLines::new();
    log.append(opening_of("old-os"), None);
    log.append(user_turn("one"), None);
    log.append(
        Event::ContextNudged(contract::events::ContextNudged {
            tokens: 266_700,
            trigger_at: 400_000,
        }),
        None,
    );

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    assert_eq!(
        text_of(conversation.last().unwrap()),
        "Fiber: your context holds 266700 tokens. At 400000 tokens Fiber will ask you for a handoff note and continue this work from it in a fresh context, so carry on as normal. The whole session stays in the session log at /log/events.jsonl."
    );
}

#[test]
fn a_second_opening_message_sits_at_the_front() {
    let log = LogLines::new();
    before_handoff(&log, false);
    note_lines(&log);
    log.append(
        handoff_done(contract::events::Outcome::Completed, &["a_note"]),
        None,
    );
    log.append(opening_of("new-os"), None);
    log.append(user_turn("two"), None);

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    let seen = texts(&conversation);
    assert!(seen[0].contains("new-os"), "{seen:?}");
    assert_eq!(seen[1..], ["one", "steer", "the note", "two"]);
    assert_eq!(
        seen.iter().filter(|text| text.contains("old-os")).count(),
        0
    );
}

// A person's and a tool's handoff render as every handoff does.

fn handoff_turn(items: Vec<InputItem>) -> Event {
    Event::TurnStarted(TurnStarted { input: items })
}

fn message_item(text: &str) -> InputItem {
    InputItem::Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: Sender {
            origin: Origin::Driver,
            command_id: Some(CommandId("c_1".into())),
        },
        changed_by: None,
    }
}

fn handoff_item(id: &str) -> InputItem {
    InputItem::Handoff {
        command_id: CommandId(id.into()),
    }
}

#[test]
fn a_handoff_item_renders_nothing_and_a_turn_of_only_one_carries_no_input() {
    let log = LogLines::new();
    log.append(opening_of("old-os"), None);
    log.append(
        handoff_turn(vec![handoff_item("c_h"), message_item("one")]),
        None,
    );
    note_lines(&log);
    log.append(
        handoff_done(contract::events::Outcome::Completed, &["a_note"]),
        None,
    );
    log.append(opening_of("new-os"), None);
    log.append(handoff_turn(vec![handoff_item("c_h2")]), None);
    note_lines(&log);

    // The first handoff carries the message beside the command's item.
    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();
    assert_eq!(texts(&conversation)[1..], ["one", "the note"]);

    log.append(
        handoff_done(contract::events::Outcome::Completed, &["a_note"]),
        None,
    );
    log.append(opening_of("newer-os"), None);

    // A turn of only the command has no input to carry.
    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();
    let seen = texts(&conversation);
    assert!(seen[0].contains("newer-os"), "{seen:?}");
    assert_eq!(seen[1..], ["the note"]);
}

fn completed_with_note(text: &str, note: Option<&str>) -> Event {
    let Event::ToolCallCompleted(mut done) = completed(text) else {
        panic!("a completion");
    };
    done.control = note.map(|handoff| contract::events::Control {
        handoff: handoff.into(),
    });
    Event::ToolCallCompleted(done)
}

/// A turn whose reply made the calls `a_w1` (`get_weather`), `a_h1`
/// (`wrapup`, which set `control.handoff`) and `a_w2` (`get_weather`) and
/// whose results were all written, then the tool handoff and the new
/// opening message.
fn tool_handoff(log: &LogLines, notes: &[(&str, &str)]) {
    log.append(opening_of("old-os"), None);
    log.append(user_turn("one"), None);
    log.append(message_started(), Some("a_m"));
    for (id, name) in [
        ("a_w1", "get_weather"),
        ("a_h1", "wrapup"),
        ("a_w2", "get_weather"),
    ] {
        log.append(requested(name), Some(id));
    }
    for id in ["a_w1", "a_h1", "a_w2"] {
        log.append(started(), Some(id));
    }
    for id in ["a_w1", "a_h1", "a_w2"] {
        let note = notes
            .iter()
            .find(|(call, _)| *call == id)
            .map(|(_, note)| *note);
        log.append(
            completed_with_note(&format!("result of {id}"), note),
            Some(id),
        );
    }
    let ids: Vec<&str> = notes.iter().map(|(call, _)| *call).collect();
    log.append(
        Event::HandoffCompleted(contract::events::HandoffCompleted {
            outcome: contract::events::Outcome::Completed,
            error: None,
            note: Some(contract::events::Note::Actions {
                note: ids.iter().map(|id| ActionId((*id).into())).collect(),
            }),
            tokens_before: 500,
            instructions: None,
        }),
        None,
    );
    log.append(opening_of("new-os"), None);
}

fn call_ids(conversation: &[Input]) -> Vec<(&'static str, String)> {
    conversation
        .iter()
        .map(|input| match input {
            Input::ToolCall { action_id, .. } => ("call", action_id.0.clone()),
            Input::ToolResult {
                action_id, text, ..
            } => {
                assert_eq!(*text, format!("result of {}", action_id.0));
                ("result", action_id.0.clone())
            }
            Input::User { .. } | Input::Assistant { .. } | Input::Reasoning { .. } => {
                ("text", text_of(input).to_owned())
            }
        })
        .collect()
}

#[test]
fn a_tool_handoff_renders_the_note_then_the_other_calls_and_their_results() {
    let log = LogLines::new();
    tool_handoff(&log, &[("a_h1", "Tool note.")]);

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    let seen = call_ids(&conversation);
    assert!(seen[0].1.contains("new-os"), "{seen:?}");
    assert_eq!(
        seen[1..],
        [
            ("text", "one".to_owned()),
            ("text", "Tool note.".to_owned()),
            ("call", "a_w1".to_owned()),
            ("call", "a_w2".to_owned()),
            ("result", "a_w1".to_owned()),
            ("result", "a_w2".to_owned()),
        ]
    );
}

#[test]
fn two_tool_notes_join_in_the_order_the_line_lists_the_calls() {
    let log = LogLines::new();
    tool_handoff(&log, &[("a_w1", "First."), ("a_h1", "Second.")]);

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    let seen = call_ids(&conversation);
    assert_eq!(
        seen[1..],
        [
            ("text", "one".to_owned()),
            ("text", "First.\n\nSecond.".to_owned()),
            ("call", "a_w2".to_owned()),
            ("result", "a_w2".to_owned()),
        ]
    );
}

#[test]
fn a_tool_handoff_survives_a_crash_and_a_second_resume() {
    let log = LogLines::new();
    tool_handoff(&log, &[("a_h1", "Tool note.")]);
    let after_handoff = call_ids(&r#loop::rebuild(&log.lines(), MODEL).unwrap());
    for _ in 0..2 {
        log.append(fiber_started(), None);
        log.append(user_turn("again"), None);
        log.append(assistant("answer"), Some("a_9"));
    }

    let seen = call_ids(&r#loop::rebuild(&log.lines(), MODEL).unwrap());

    // The context from the handoff on is intact, with each resumed turn
    // after it.
    assert_eq!(seen[..after_handoff.len()], after_handoff[..]);
    assert_eq!(
        seen[after_handoff.len()..],
        [
            ("text", "again".to_owned()),
            ("text", "answer".to_owned()),
            ("text", "again".to_owned()),
            ("text", "answer".to_owned()),
        ]
    );
}

// Resuming over handoffs (`docs/handoff.md`, "Resume").

/// A history holding an opening, one turn, and the note request's lines.
fn handoff_history(script: Vec<Scripted>) -> History {
    let history = History::new(script);
    history.write(opening_of("old-os"), None);
    history.write(user_turn("one"), None);
    history.write(handoff_started(), None);
    history.write(message_started(), Some("a_note"));
    history.write(assistant("the note"), Some("a_note"));
    history
}

fn resume_conversation(history: &mut History, prompt: &str) -> Vec<Input> {
    let looped = history.resume(Vec::new());
    history.run(looped, prompt);
    history
        .provider
        .requests()
        .last()
        .unwrap()
        .conversation
        .clone()
}

#[test]
fn a_resume_after_a_completed_handoff_sends_from_the_last_handoff() {
    let mut history = handoff_history(vec![Scripted::text("Hello.")]);
    history.write(
        handoff_done(contract::events::Outcome::Completed, &["a_note"]),
        None,
    );
    history.write(opening_of("new-os"), None);
    history.freeze();

    let conversation = resume_conversation(&mut history, "two");

    let seen = texts(&conversation);
    assert!(seen[0].contains("new-os"), "{seen:?}");
    assert_eq!(seen[1..], ["one", "the note", "two"]);
    // The new context has had no request, and its opening is written once.
    assert_eq!(history.provider.requests()[0].previous_end, None);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_resume_after_a_crash_mid_handoff_sends_the_context_as_it_was() {
    let mut history = handoff_history(vec![Scripted::text("Two."), Scripted::text("Three.")]);
    // The process that resumes writes `fiber_started`.
    history.write(fiber_started(), None);
    history.freeze();

    let conversation = resume_conversation(&mut history, "two");

    let seen = texts(&conversation);
    assert!(seen[0].contains("old-os"), "{seen:?}");
    assert_eq!(seen[1..], ["one", "two"]);

    // A second resume, after that further turn, still holds it.
    let (tx, rx) = mpsc::channel();
    history.inbox_tx = tx;
    history.inbox_rx = Some(rx);
    history.write(fiber_started(), None);
    history.freeze();
    let conversation = resume_conversation(&mut history, "three");

    let seen = texts(&conversation);
    assert!(seen[0].contains("old-os"), "{seen:?}");
    assert_eq!(seen[1..4], ["one", "two", "Two."]);
    assert_eq!(seen.last().copied(), Some("three"));
}

#[test]
fn a_crash_between_the_completion_and_the_new_opening_writes_it_at_the_next_turn() {
    let mut history = handoff_history(vec![Scripted::text("Hello.")]);
    history.write(
        handoff_done(contract::events::Outcome::Completed, &["a_note"]),
        None,
    );
    history.freeze();

    let conversation = resume_conversation(&mut history, "two");

    let seen = texts(&conversation);
    assert!(
        seen[0].starts_with("This message is from Fiber"),
        "{seen:?}"
    );
    assert!(seen[0].contains("Session log:"), "{seen:?}");
    assert!(!seen[0].contains("old-os"));
    assert_eq!(seen[1..], ["one", "the note", "two"]);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

/// The kinds of a resumed turn whose first reply calls a tool and whose
/// second is text.
const RESUMED_TURN: [&str; 15] = [
    "preamble_built",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

/// A resumed turn whose first reply calls a tool with `tokens` in its
/// prompt: the context is past two thirds of the default trigger.
fn nudge_after_resume(nudged_before: bool) -> Vec<String> {
    let mut history = History::new(vec![
        support::with_tokens(support::tool_call_reply("", &["get_weather"]), 270_000, 0),
        Scripted::text("Done."),
    ]);
    history.write(opening_of("old-os"), None);
    history.write(user_turn("one"), None);
    if nudged_before {
        history.write(
            Event::ContextNudged(contract::events::ContextNudged {
                tokens: 270_000,
                trigger_at: 400_000,
            }),
            None,
        );
    }
    history.freeze();
    let looped = history.resume(vec![(
        "builtin".to_owned(),
        Arc::new(support::TestTool::reads("get_weather", "abcd")) as Arc<dyn Tool>,
    )]);
    history.run(looped, "two");
    history.new_kinds()
}

#[test]
fn a_context_nudged_before_the_crash_is_not_nudged_again() {
    assert_eq!(nudge_after_resume(true), RESUMED_TURN);
}

#[test]
fn a_context_not_nudged_before_the_crash_is_nudged_once_a_reply_measures_it() {
    let mut expected = RESUMED_TURN.to_vec();
    expected.insert(10, "context_nudged");
    assert_eq!(nudge_after_resume(false), expected);
}

fn hosted_requested() -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: "web_search".into(),
        arguments: json!({"query": "rust 1.90"}),
        provider_id: Some(contract::ProviderCallId("srvtoolu_01".into())),
        repair: None,
        ran_by: None,
        provider_item: Some(json!({"type": "server_tool_use", "id": "srvtoolu_01"})),
    })
}

fn hosted_completed() -> Event {
    let Event::ToolCallCompleted(done) = completed("https://blog.rust-lang.org/") else {
        panic!("completed builds a tool_call_completed");
    };
    Event::ToolCallCompleted(ToolCallCompleted {
        provider_item: Some(json!({"type": "web_search_tool_result"})),
        ..done
    })
}

fn raw(item: serde_json::Value) -> Input {
    Input::Assistant {
        model: MODEL.into(),
        text: String::new(),
        provider_item: Some(item),
    }
}

#[test]
fn a_hosted_pair_rebuilds_as_its_two_raw_blocks_and_no_result() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(message_started(), Some("a_0"));
    log.append(hosted_requested(), Some("a_1"));
    log.append(started(), Some("a_1"));
    log.append(hosted_completed(), Some("a_1"));
    log.append(assistant("Done."), Some("a_0"));

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    assert_eq!(
        conversation[1..],
        [
            raw(json!({"type": "server_tool_use", "id": "srvtoolu_01"})),
            raw(json!({"type": "web_search_tool_result"})),
            Input::Assistant {
                model: MODEL.into(),
                text: "Done.".into(),
                provider_item: None
            }
        ]
    );
    assert!(
        conversation
            .iter()
            .all(|i| !matches!(i, Input::ToolCall { .. } | Input::ToolResult { .. }))
    );
}

#[test]
fn a_hosted_call_cut_from_its_result_by_a_crash_renders_nothing_and_gets_no_fixed_result() {
    let log = LogLines::new();
    log.append(user_turn("one"), None);
    log.append(message_started(), Some("a_0"));
    log.append(hosted_requested(), Some("a_1"));
    log.append(started(), Some("a_1"));

    let conversation = r#loop::rebuild(&log.lines(), MODEL).unwrap();

    assert_eq!(conversation.len(), 1, "{conversation:?}");
    assert!(matches!(conversation[0], Input::User { .. }));
}

#[test]
fn a_suspended_batch_leaves_a_hosted_call_out() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read"), Some("a_1"));
    history.write(hosted_requested(), Some("a_2"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless(Vec::new());
    let (_looped, outcome) = history.step(looped);

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let new = history.new_lines();
    let done: Vec<_> = new
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .map(|l| l.action_id.clone().unwrap().0)
        .collect();
    assert_eq!(done, ["a_1"], "the hosted call is given no result");
    let conversation = &history.provider.requests()[0].conversation;
    assert!(conversation.iter().all(|i| !matches!(
        i,
        Input::Assistant {
            provider_item: Some(_),
            ..
        }
    )));
    assert!(conversation.iter().all(
        |i| !matches!(i, Input::ToolCall { action_id, .. } | Input::ToolResult { action_id, .. } if action_id.0 == "a_2")
    ));
}

#[test]
fn a_resumed_session_replays_a_logged_section_byte_for_byte() {
    let held = fakes::TempDir::new("fiber-resume-sections");
    let file = held.path().join("index.md");
    std::fs::write(&file, "- [[x]]").unwrap();
    let mut live = support::Session::sectioned(
        vec![Scripted::text("Done.")],
        vec![("fiber.test/notes".into(), vec![file.clone()], Some(5))],
    );
    live.inbox.send(support::delivery("hi")).unwrap();
    live.turn();
    let live_lines = live.lines();
    assert_eq!(
        live_lines
            .iter()
            .map(|l| l.kind.as_str())
            .collect::<Vec<_>>(),
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
        ]
    );
    let live_opening = live_lines
        .iter()
        .find(|line| line.kind == "opening_message")
        .unwrap();
    let Input::User { text: live_text } = &live.requests()[0].conversation[0] else {
        panic!("not an opening message");
    };
    let live_text = live_text.clone();
    assert!(live_text.contains("# From the fiber.test/notes extension"));
    // The file is gone before the resume: identical bytes prove the
    // opening is rendered from the log, never the disk.
    std::fs::remove_file(&file).unwrap();
    let message: OpeningMessage =
        serde_json::from_value(serde_json::Value::Object(live_opening.payload.clone())).unwrap();
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(Event::OpeningMessage(message), None);
    history.write(user_turn("one"), None);
    history.freeze();

    let looped = history.resume(Vec::new());
    history.run(looped, "two");
    assert_eq!(
        history.history_kinds(),
        ["session_started", "opening_message", "turn_started"]
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let Input::User { text } = &requests[0].conversation[0] else {
        panic!("not an opening message");
    };
    assert_eq!(text, &live_text);
}

/// A provider that starts a shutdown on `cancel` as each call is made, then
/// answers from `inner`.
struct ShutsDown {
    inner: Arc<ScriptedProvider>,
    cancel: Arc<r#loop::TurnCancel>,
}

impl Provider for ShutsDown {
    fn call(
        &self,
        request: &contract::provider::ModelRequest,
    ) -> Box<dyn contract::provider::ModelCall> {
        self.cancel.shutdown(143);
        self.inner.call(request)
    }
}

/// `fiber_exited` under SIGTERM after the resumed process's lines.
fn exited_on_signal(history: &History) -> Envelope {
    let written =
        r#loop::fiber_exited(&history.log, &history.dir, Ok(()), true, Some(143)).unwrap();
    assert_eq!(written.code, 143);
    history.lines().pop().unwrap()
}

#[test]
fn a_shutdown_before_the_finishing_turn_writes_nothing_and_keeps_the_request() {
    let mut history = suspended_history(vec![Scripted::text("Done.")]);
    let cancel = Arc::new(r#loop::TurnCancel::default());
    let looped = history
        .resume_headless(Vec::new())
        .cancelled_by(Arc::clone(&cancel));
    r#loop::fiber_started(&history.log, "0.0.1", true).unwrap();
    cancel.shutdown(143);
    let (_looped, outcome) = history.step(looped);

    assert_eq!(outcome, None);
    // No preamble, no re-raise: only this process's `fiber_started`.
    assert_eq!(history.new_kinds(), ["fiber_started"]);
    assert!(history.provider.requests().is_empty());
    let exited = exited_on_signal(&history);
    assert_eq!(exited.payload["suspended_on"], "r_9");
}

#[test]
fn a_shutdown_after_the_refusal_ends_the_finishing_turn_interrupted() {
    let mut history = suspended_history(vec![Scripted::text("Done.")]);
    let cancel = Arc::new(r#loop::TurnCancel::default());
    let provider = Arc::new(ShutsDown {
        inner: Arc::clone(&history.provider),
        cancel: Arc::clone(&cancel),
    });
    // A prompt waiting behind the finishing turn is held aside, then
    // answered by the shutdown.
    let (prompt, answer) = recording();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), prompt))
        .unwrap();
    let looped = history
        .resume_headless_on(provider, Vec::new())
        .cancelled_by(Arc::clone(&cancel));
    let (looped, outcome) = history.step(looped);

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Interrupted));
    let kinds = history.new_kinds();
    assert!(kinds.contains(&"permission_requested".to_owned()));
    assert!(kinds.contains(&"permission_resolved".to_owned()));
    assert_eq!(kinds.last().map(String::as_str), Some("turn_completed"));
    // The loop starts no other turn and rejects the held prompt.
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || done.send(looped.run().is_ok()).unwrap());
    assert!(
        finished
            .recv_timeout(support::DEADLINE)
            .expect("run returned")
    );
    match &*answer.lock().unwrap() {
        Some(Err(rejected)) => assert_eq!(rejected.code, contract::ErrorCode::Closing),
        other => panic!("the prompt was answered closing, not {other:?}"),
    }
    assert_eq!(
        history.new_kinds().last().map(String::as_str),
        Some("turn_completed")
    );
    let exited = exited_on_signal(&history);
    assert_eq!(exited.payload.get("suspended_on"), None);
}
