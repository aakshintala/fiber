//! The process-wide variables source (`docs/invocation.md`, "A session's environment").
//!
//! Each test runs in its own nextest process, so setting the source here
//! never leaks into another test.
//!
//! The write path every event takes, and what a completed handoff leaves
//! the reviewer's input holding (`docs/permissions.md`, "At a handoff").

use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::commands::Handoff;
use contract::emit::Emit;
use contract::events::{
    CacheLifetime, Event, HandoffCompleted, InputItem, Note, Outcome, ToolCallRequested,
    TurnOutcome, TurnStarted, VariablesSource,
};
use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{Delta, Provider, ReplyAction, ToolDefinition};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Sender};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{ActionId, CommandId, ErrorCode, SessionId, TurnId};
use fakes::{Deadline, NoRules};
use log::Log;
use serde_json::{Map, Value, json};

use super::{set_variables_source, variables, variables_source};
use crate::{BlockLimits, Loop, Model, Reviewer, TurnCancel};

#[test]
fn unset_source_is_inherited() {
    assert_eq!(variables_source(), VariablesSource::Inherited);
    assert_eq!(variables().source, VariablesSource::Inherited);
}

#[test]
fn setting_login_shell_is_observed() {
    set_variables_source(VariablesSource::LoginShell);
    assert_eq!(variables_source(), VariablesSource::LoginShell);
    assert_eq!(variables().source, VariablesSource::LoginShell);
}

/// How long a driven turn may take before the test fails naming the
/// missing turn end, instead of hanging.
const TURN_DEADLINE: Duration = Duration::from_secs(10);

/// A tool whose calls only read, and return one line.
struct ProbeTool;

impl Tool for ProbeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "probe".to_owned(),
            description: "A reading tool.".to_owned(),
            input_schema: json!({"type": "object"}),
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
            subject: None,
            prefix: None,
            // Every call meets the reviewer, so the driven tests read
            // what each reviewer request carried.
            always_reviewed: true,
            hosts: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "Ran it.".into(),
            }],
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

/// A reviewer-less loop on a scripted provider, with its inbox, log and
/// session directory held.
struct Driven {
    looped: Option<Loop>,
    inbox: mpsc::Sender<Delivery>,
    dir: std::path::PathBuf,
    _home: fakes::TempDir,
}

fn harness(script: Vec<fakes::Scripted>) -> Driven {
    let home = fakes::TempDir::new("fiber-handoff-cut");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let provider = Arc::new(fakes::ScriptedProvider::new(script));
    let (inbox, rx) = mpsc::channel();
    let session_log = home
        .path()
        .join("s_test/events.jsonl")
        .display()
        .to_string();
    let looped = Loop::start(
        crate::Session {
            log: Arc::clone(&log),
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                session_log,
                Arc::clone(&clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: vec![("probe".to_owned(), Arc::new(ProbeTool) as Arc<dyn Tool>)],
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(NoRules),
            },
        },
        None,
    )
    .unwrap()
    .answerable(false)
    .cancelled_by(Arc::new(TurnCancel::default()));
    Driven {
        looped: Some(looped),
        inbox,
        dir: home.path().join("s_test"),
        _home: home,
    }
}

impl Driven {
    /// Gives the loop a reviewer answering `script`, returning its
    /// provider so the test reads what each reviewer request carried.
    fn reviewer(&mut self, script: Vec<fakes::Scripted>) -> Arc<fakes::ScriptedProvider> {
        let provider = Arc::new(fakes::ScriptedProvider::new(script));
        let looped = self.looped.take().unwrap().reviewer(
            Ok(Reviewer {
                provider: Arc::clone(&provider) as Arc<dyn Provider>,
                model: Model {
                    reference: "fake/reviewer".into(),
                    cost: None,
                    subscription: false,
                },
                thinking_levels: Vec::new(),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: 1_000_000,
            }),
            BlockLimits::default(),
        );
        self.looped = Some(looped);
        provider
    }

    /// Sends the person's `text` as the next turn's prompt.
    fn prompt(&self, text: &str) {
        self.inbox
            .send(Delivery::Prompt(
                Message {
                    content: vec![ContentPart::Text { text: text.into() }],
                    sender: Sender {
                        origin: Origin::Driver,
                        command_id: Some(CommandId("c_go".into())),
                    },
                },
                Ack(Box::new(|_| {})),
            ))
            .unwrap();
    }

    /// Sends the person's `handoff` command.
    fn handoff(&self) {
        self.inbox
            .send(Delivery::Handoff(
                CommandId("c_h".into()),
                Handoff { instructions: None },
                Ack(Box::new(|_| {})),
            ))
            .unwrap();
    }

    /// Runs one turn on its own thread, failing at [`TURN_DEADLINE`]
    /// naming the missing turn end.
    #[track_caller]
    fn turn(&mut self) -> Option<TurnOutcome> {
        let mut looped = self.looped.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let outcome = looped.turn().unwrap();
            done.send((looped, outcome)).unwrap();
        });
        let (looped, outcome) = Deadline::after(TURN_DEADLINE)
            .recv(&finished)
            .expect("the turn ended in time");
        self.looped = Some(looped);
        outcome
    }

    /// The `seq` of the `turn_started` line holding the person's `text`.
    #[track_caller]
    fn turn_started_seq(&self, text: &str) -> u64 {
        log::read(&self.dir)
            .unwrap()
            .iter()
            .find(|line| {
                line.kind == "turn_started"
                    && line.payload["input"].as_array().is_some_and(|input| {
                        input.iter().any(|item| {
                            item["content"].as_array().is_some_and(|content| {
                                content.iter().any(|part| part["text"] == text)
                            })
                        })
                    })
            })
            .unwrap_or_else(|| panic!("no turn_started holds {text:?}"))
            .seq
            .unwrap()
            .0
    }
}

/// A model reply requesting one `probe` call.
fn call_reply() -> fakes::Scripted {
    let arguments = json!({"city": "Paris"});
    let mut end = fakes::reply("Checking.");
    end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
        name: "probe".into(),
        arguments: arguments.clone(),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }));
    fakes::Scripted {
        deltas: vec![
            Delta::Text(contract::events::TextDelta {
                text: "Checking.".into(),
            }),
            Delta::ToolCallArguments(contract::events::ToolCallArgumentsDelta {
                index: 0,
                name: Some("probe".into()),
                text: arguments.to_string(),
            }),
        ],
        end: Ok(end),
    }
}

/// A person's message as a `turn_started` event.
fn person_turn(text: &str) -> Event {
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

/// One `probe` call as a `tool_call_requested` event.
fn probe_call() -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: "probe".into(),
        arguments: json!({"city": "Paris"}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

fn handoff_completed() -> Event {
    Event::HandoffCompleted(HandoffCompleted {
        outcome: Outcome::Completed,
        error: None,
        note: Some(Note::Actions {
            note: vec![ActionId("a_note".into())],
        }),
        tokens_before: 1000,
        instructions: None,
    })
}

fn handoff_failed() -> Event {
    Event::HandoffCompleted(HandoffCompleted {
        outcome: Outcome::Failed,
        error: Some(Failure {
            code: ErrorCode::ContextOverflow,
            message: "the note did not fit".into(),
            retry_after_ms: None,
            provider: None,
        }),
        note: None,
        tokens_before: 1000,
        instructions: None,
    })
}

fn handoff_cancelled() -> Event {
    Event::HandoffCompleted(HandoffCompleted {
        outcome: Outcome::Cancelled,
        error: None,
        note: None,
        tokens_before: 1000,
        instructions: None,
    })
}

fn user_text(input: &contract::provider::Input) -> &str {
    match input {
        contract::provider::Input::User { text, .. } => text.as_str(),
        contract::provider::Input::Assistant { .. }
        | contract::provider::Input::Reasoning { .. }
        | contract::provider::Input::ToolCall { .. }
        | contract::provider::Input::ToolResult { .. }
        | contract::provider::Input::ToolSearchResult { .. } => {
            panic!("a reviewer item is always a user item")
        }
    }
}

#[test]
fn a_completed_handoff_write_resets_the_reviewer_cursor_and_drops_calls() {
    let mut driven = harness(Vec::new());
    let mut looped = driven.looped.take().unwrap();
    let turn = TurnId("t_1".into());
    let action = ActionId("a_1".into());
    looped
        .write(&person_turn("never push to main"), Some(&turn), None)
        .unwrap();
    looped
        .write(&probe_call(), Some(&turn), Some(&action))
        .unwrap();
    // A review ran before the handoff, so its prefix end is set.
    looped.reviewer_sent = Some(2);
    looped
        .write(&handoff_completed(), Some(&turn), None)
        .unwrap();
    assert_eq!(looped.reviewer_sent, None);
    assert_eq!(looped.reviewed.len(), 1, "{:?}", looped.reviewed);
    assert_eq!(
        user_text(&looped.reviewed[0].input),
        "The person: never push to main"
    );
}

#[test]
fn a_failed_or_cancelled_handoff_write_changes_neither_cursor_nor_input() {
    for event in [handoff_failed(), handoff_cancelled()] {
        let mut driven = harness(Vec::new());
        let mut looped = driven.looped.take().unwrap();
        let turn = TurnId("t_1".into());
        let action = ActionId("a_1".into());
        looped
            .write(&person_turn("never push to main"), Some(&turn), None)
            .unwrap();
        looped
            .write(&probe_call(), Some(&turn), Some(&action))
            .unwrap();
        looped.reviewer_sent = Some(2);
        let before = looped.reviewed.clone();
        looped.write(&event, Some(&turn), None).unwrap();
        assert_eq!(looped.reviewer_sent, Some(2));
        assert_eq!(looped.reviewed.len(), before.len());
        assert_eq!(looped.reviewed, before);
    }
}

#[test]
fn an_ordinary_write_keeps_the_reviewer_cursor() {
    let mut driven = harness(Vec::new());
    let mut looped = driven.looped.take().unwrap();
    let turn = TurnId("t_1".into());
    looped.reviewer_sent = Some(1);
    looped
        .write(&person_turn("say hi"), Some(&turn), None)
        .unwrap();
    assert_eq!(looped.reviewer_sent, Some(1));
    assert_eq!(looped.reviewed.len(), 1);
}

#[test]
fn a_durable_write_carries_the_log_seq_into_the_reviewed_identity() {
    let mut driven = harness(Vec::new());
    let mut looped = driven.looped.take().unwrap();
    let turn = TurnId("t_1".into());
    looped
        .write(&person_turn("never push to main"), Some(&turn), None)
        .unwrap();
    // The identity is the durable line's own `seq`: the fold renders
    // nothing for a person message with none, so a later `reviewer_kept`
    // always names a line the log holds.
    let seq = driven.turn_started_seq("never push to main");
    assert_eq!(
        format!("{:?}", looped.reviewed[0].shown),
        format!("Person(KeptMessage {{ seq: Seq({seq}), item: 0 }})")
    );
    assert_eq!(
        user_text(&looped.reviewed[0].input),
        "The person: never push to main"
    );
}

#[test]
fn a_poisoned_log_error_converts_to_the_exact_loop_error() {
    // No deterministic in-test write failure reaches `Log::append`
    // without failure injection (`unsafe` is denied, and no new seam may
    // be added for this): this pins the conversion half, that a refused
    // append surfaces the poison verbatim instead of a bare failure.
    let poisoned = log::Error::Poisoned {
        session: "s_1".into(),
        cause: "a failed write".into(),
    };
    let error = crate::Error::from(poisoned);
    assert!(
        matches!(
            &error,
            crate::Error::Log(log::Error::Poisoned { session, cause })
            if session == "s_1" && cause == "a failed write"
        ),
        "{error:?}"
    );
}

#[test]
fn a_reviewerless_handoff_cut_drops_pre_handoff_calls_from_reviewed() {
    let mut driven = harness(vec![
        call_reply(),
        fakes::Scripted::text("Done."),
        fakes::Scripted::text("The note."),
        call_reply(),
        fakes::Scripted::text("Done."),
    ]);
    driven.prompt("never push to main");
    assert_eq!(driven.turn(), Some(TurnOutcome::Completed));
    driven.handoff();
    assert_eq!(driven.turn(), Some(TurnOutcome::Completed));
    driven.prompt("now run it");
    assert_eq!(driven.turn(), Some(TurnOutcome::Completed));
    let looped = driven.looped.as_ref().unwrap();
    // Both person messages survive word for word; only the post-cut call
    // stays, exactly once. The last reply stays held at the tail: no
    // person message answers it yet.
    let texts: Vec<&str> = looped
        .reviewed
        .iter()
        .map(|item| user_text(&item.input))
        .collect();
    assert_eq!(
        texts,
        [
            "The person: never push to main",
            "The person: now run it",
            r#"Tool call: {"tool":"probe","arguments":{"city":"Paris"}}"#,
            "The agent: Done.",
        ],
        "{texts:?}"
    );
    assert_eq!(looped.reviewer_sent, None);
    let first = driven.turn_started_seq("never push to main");
    let second = driven.turn_started_seq("now run it");
    assert_eq!(
        format!("{:?}", looped.reviewed[0].shown),
        format!("Person(KeptMessage {{ seq: Seq({first}), item: 0 }})")
    );
    assert_eq!(
        format!("{:?}", looped.reviewed[1].shown),
        format!("Person(KeptMessage {{ seq: Seq({second}), item: 0 }})")
    );
}

#[test]
fn the_selection_request_after_the_cut_sends_no_previous_end() {
    let mut driven = harness(vec![
        call_reply(),
        fakes::Scripted::text("Done."),
        fakes::Scripted::text("The note."),
        call_reply(),
        fakes::Scripted::text("Done."),
    ]);
    let reviewer = driven.reviewer(vec![
        fakes::Scripted::text("allow"),
        fakes::Scripted::text("1"),
        fakes::Scripted::text("check"),
        fakes::Scripted::text("allow"),
    ]);
    driven.prompt("never push to main");
    assert_eq!(driven.turn(), Some(TurnOutcome::Completed));
    driven.handoff();
    assert_eq!(driven.turn(), Some(TurnOutcome::Completed));
    driven.prompt("now run it");
    assert_eq!(driven.turn(), Some(TurnOutcome::Completed));
    let requests = reviewer.requests();
    assert_eq!(requests.len(), 4);
    // The first review and the first review after the handoff both send
    // the whole input: no stale prefix end survives the cut.
    assert_eq!(requests[0].previous_end, None);
    // The selection itself restarts from the kept persons: the pre-cut
    // call is gone before the reviewer chooses.
    assert_eq!(requests[1].previous_end, None);
    let selecting: Vec<&str> = requests[1].conversation.iter().map(user_text).collect();
    assert_eq!(selecting.len(), 2, "{selecting:?}");
    assert_eq!(selecting[0], "The person: never push to main");
    assert!(
        !selecting.iter().any(|text| text.starts_with("Tool call: ")),
        "{selecting:?}"
    );
    assert_eq!(requests[2].previous_end, None);
    // A second review in the same context reuses its prefix: only a
    // cut, a resume or a model switch clears the cursor.
    assert!(
        requests[3].previous_end.is_some(),
        "{:?}",
        requests[3].previous_end
    );
}
