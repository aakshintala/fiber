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
