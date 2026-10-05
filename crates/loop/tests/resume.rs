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
                command_id: CommandId("c_1".into()),
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
            command_id: CommandId("c_2".into()),
        },
        changed_by: None,
    })
}

fn result_of(input: &Input) -> (&ActionId, &str, bool) {
    let Input::ToolResult {
        action_id,
        text,
        is_error,
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
        let lines = self.lines();
        Loop::resume(
            Arc::clone(&self.log),
            &lines,
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
    assert!(matches!(&conversation[0], Input::User { text } if text == "one"));
    let Input::ToolCall { action_id, .. } = &conversation[1] else {
        panic!("{conversation:?}");
    };
    assert_eq!(action_id.0, "a_1");
    let (id, text, is_error) = result_of(&conversation[2]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "It never ran.");
    assert!(is_error);
    // The log holds no opening message, so the resume writes one at its
    // first turn, after the rebuilt history.
    assert!(
        matches!(&conversation[3], Input::User { text } if text.starts_with("This message is from Fiber"))
    );
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
    let lines = history.lines();
    let (tx, rx) = mpsc::channel();
    let looped = Loop::resume(
        Arc::clone(&history.log),
        &lines,
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
    let lines = history.lines();
    let (tx, rx) = mpsc::channel();
    let looped = Loop::resume(
        Arc::clone(&history.log),
        &lines,
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
    let lines = history.lines();
    let (tx, rx) = mpsc::channel();
    let looped = Loop::resume(
        Arc::clone(&history.log),
        &lines,
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
    let (id, text, _) = result_of(&conversation[2]);
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
    let resumed = r#loop::resumed(&lines).unwrap();
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
    let resumed = r#loop::resumed(&lines).unwrap();
    assert_eq!(resumed.session, "s_1");
    assert_eq!(resumed.workspace, history.workspace);
    assert_eq!(resumed.model, None);
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

    let error = match r#loop::resumed(&lines) {
        Ok(_) => panic!("a log with no session_started resumes"),
        Err(error) => error,
    };
    assert_eq!(error.code(), ErrorCode::LogCorrupt);
}

#[test]
fn resume_fails_log_corrupt_on_a_log_with_no_session_started() {
    let root = fakes::TempDir::new("fiber-resume");
    let log = Arc::new(
        Log::create(
            root.path(),
            SessionId("s_9".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap(),
    );
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
    let (_tx, rx) = mpsc::channel();

    let error = match Loop::resume(
        log,
        &lines,
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
        Ok(_) => panic!("a log with no session_started resumes"),
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
        let lines = self.lines();
        Loop::resume(
            Arc::clone(&self.log),
            &lines,
            Arc::clone(&self.provider) as Arc<dyn Provider>,
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
    assert!(matches!(&first[1], Input::ToolCall { .. }), "{first:?}");
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
