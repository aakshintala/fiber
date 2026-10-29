//! Task 2.1: the prepared call, end to end on the live path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::emit::Emit;
use contract::events::{TextDelta, ToolCallArgumentsDelta, ToolCallRequested};
use contract::hook::{
    BeforeMessage, BeforeMessageAnswer, BeforeMessageOutcome, BeforeToolAnswer, BeforeToolCall,
    BeforeToolOutcome, CheckAnswer, CheckOutcome, Hooks,
};
use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{Delta, Provider, ReplyAction};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, SessionId};
use fakes::{Deadline, NoRules};
use log::Log;
use serde_json::{Map, Value, json};

use crate::{Loop, Model, TurnCancel};

const TURN_DEADLINE: Duration = Duration::from_secs(10);

/// What a `before_tool` or `check_tool` call was shown, owned.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    tool: String,
    arguments: Map<String, Value>,
    paths: Option<Vec<String>>,
}

/// What an `after_tool` call was shown, owned.
#[derive(Debug, Clone, PartialEq)]
struct AfterSeen {
    tool: String,
    arguments: Map<String, Value>,
    content: String,
}

/// A scripted `Hooks` fake: per-method seen calls, a queue of answers per
/// method, and an optional shutdown to start inside `before_tool`.
struct FakeHooks {
    before_seen: Mutex<Vec<Seen>>,
    check_seen: Mutex<Vec<Seen>>,
    after_seen: Mutex<Vec<AfterSeen>>,
    before_answers: Mutex<VecDeque<BeforeToolOutcome>>,
    check_answers: Mutex<VecDeque<CheckOutcome>>,
    after_answers: Mutex<VecDeque<(contract::hook::AfterToolOutcome, Vec<String>)>>,
    notices_before: Mutex<Vec<contract::events::Notice>>,
    notices_check: Mutex<Vec<contract::events::Notice>>,
    shutdown: Option<Arc<TurnCancel>>,
    cancel_in_before: Option<Arc<TurnCancel>>,
}

impl FakeHooks {
    fn new() -> Self {
        Self {
            before_seen: Mutex::new(Vec::new()),
            check_seen: Mutex::new(Vec::new()),
            after_seen: Mutex::new(Vec::new()),
            before_answers: Mutex::new(VecDeque::new()),
            check_answers: Mutex::new(VecDeque::new()),
            after_answers: Mutex::new(VecDeque::new()),
            notices_before: Mutex::new(Vec::new()),
            notices_check: Mutex::new(Vec::new()),
            shutdown: None,
            cancel_in_before: None,
        }
    }

    fn with_before(outcome: BeforeToolOutcome) -> Self {
        let mut hooks = Self::new();
        hooks.before_answers.get_mut().unwrap().push_back(outcome);
        hooks
    }

    fn before_seen(&self) -> Vec<Seen> {
        self.before_seen.lock().unwrap().clone()
    }

    fn check_seen(&self) -> Vec<Seen> {
        self.check_seen.lock().unwrap().clone()
    }
}

fn seen_of(call: &BeforeToolCall<'_>) -> Seen {
    Seen {
        tool: call.tool.to_owned(),
        arguments: call.arguments.clone(),
        paths: call.declared.paths.clone(),
    }
}

impl Hooks for FakeHooks {
    fn after_tool(
        &self,
        call: &contract::hook::AfterToolCall<'_>,
    ) -> contract::hook::AfterToolAnswer {
        self.after_seen.lock().unwrap().push(AfterSeen {
            tool: call.tool.to_owned(),
            arguments: call.arguments.clone(),
            content: call.content.to_owned(),
        });
        let (outcome, changed_by) = self
            .after_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or((contract::hook::AfterToolOutcome::Unchanged, Vec::new()));
        contract::hook::AfterToolAnswer {
            outcome,
            changed_by,
            notices: Vec::new(),
        }
    }

    fn before_tool(&self, call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        self.before_seen.lock().unwrap().push(seen_of(call));
        if let Some(cancel) = &self.cancel_in_before {
            assert!(cancel.cancel());
        }
        if let Some(cancel) = &self.shutdown {
            cancel.shutdown(143);
        }
        let outcome = self
            .before_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(BeforeToolOutcome::Unchanged);
        let notices = std::mem::take(&mut *self.notices_before.lock().unwrap());
        BeforeToolAnswer { outcome, notices }
    }

    fn check_tool(&self, call: &BeforeToolCall<'_>) -> CheckAnswer {
        self.check_seen.lock().unwrap().push(seen_of(call));
        let outcome = self
            .check_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(CheckOutcome::Passed);
        let notices = std::mem::take(&mut *self.notices_check.lock().unwrap());
        CheckAnswer { outcome, notices }
    }

    fn before_message(&self, _message: &BeforeMessage<'_>) -> BeforeMessageAnswer {
        BeforeMessageAnswer {
            outcome: BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(
        &self,
        _start: &contract::hook::SessionStart<'_>,
    ) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _turn: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(
        &self,
        _call: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _end: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_handoff(
        &self,
        _handoff: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        fakes::no_note()
    }

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<Delivery>) {}
}

/// A schema'd counting tool: schema takes an optional string `path` and an
/// optional integer `count`; effects declare `path` as the touched path and
/// record every call. An effects error when `path` is `effects-boom`.
struct CountingTool {
    runs: Mutex<Vec<Map<String, Value>>>,
    effects_calls: Mutex<Vec<Map<String, Value>>>,
    output: Output,
    bound: Bound,
}

impl CountingTool {
    fn new(output: &str) -> Self {
        Self {
            runs: Mutex::new(Vec::new()),
            effects_calls: Mutex::new(Vec::new()),
            output: Output {
                content: vec![ContentPart::Text {
                    text: output.to_owned(),
                }],
                ..Output::default()
            },
            bound: Bound::DEFAULT,
        }
    }

    fn with_output(output: Output) -> Self {
        Self {
            runs: Mutex::new(Vec::new()),
            effects_calls: Mutex::new(Vec::new()),
            output,
            bound: Bound::DEFAULT,
        }
    }

    fn with_bound(mut self, bound: Bound) -> Self {
        self.bound = bound;
        self
    }

    fn runs(&self) -> Vec<Map<String, Value>> {
        self.runs.lock().unwrap().clone()
    }

    fn effects_calls(&self) -> Vec<Map<String, Value>> {
        self.effects_calls.lock().unwrap().clone()
    }
}

impl Tool for CountingTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "probe".to_owned(),
            description: "A counting tool.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "count": {"type": "integer"}
                },
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        self.effects_calls.lock().unwrap().push(arguments.clone());
        if arguments.get("path").and_then(Value::as_str) == Some("effects-boom") {
            return Err(EffectsError::Tool("the tool could not classify".to_owned()));
        }
        let paths = arguments
            .get("path")
            .and_then(Value::as_str)
            .map(|path| vec![path.to_owned()]);
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths,
                confined: false,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(&self, arguments: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        self.runs.lock().unwrap().push(arguments.clone());
        self.output.clone()
    }

    fn bound(&self) -> Bound {
        self.bound
    }
}

fn script(name: &str, arguments: Value) -> Vec<fakes::Scripted> {
    let mut end = fakes::reply("Checking.");
    end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
        name: name.into(),
        arguments: arguments.clone(),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }));
    vec![
        fakes::Scripted {
            deltas: vec![
                Delta::Text(TextDelta {
                    text: "Checking.".into(),
                }),
                Delta::ToolCallArguments(ToolCallArgumentsDelta {
                    index: 0,
                    name: Some(name.into()),
                    text: arguments.to_string(),
                }),
            ],
            end: Ok(end),
        },
        fakes::Scripted::text("Done."),
    ]
}

struct Ran {
    turn: Result<Option<contract::events::TurnOutcome>, crate::Error>,
    lines: Vec<Envelope>,
    streamed: Vec<Envelope>,
    requests: Vec<contract::provider::ModelRequest>,
    session: std::path::PathBuf,
    _home: fakes::TempDir,
}

impl Ran {
    fn of_kind(&self, kind: &str) -> Vec<&Envelope> {
        self.lines.iter().filter(|l| l.kind == kind).collect()
    }

    /// The complete, ordered kinds of the durable lines: every event the
    /// turn wrote, so a duplicated, missing or reordered event fails.
    fn durable_kinds(&self) -> Vec<&str> {
        self.lines.iter().map(|line| line.kind.as_str()).collect()
    }

    fn streamed_of_kind(&self, kind: &str) -> Vec<&Envelope> {
        self.streamed.iter().filter(|l| l.kind == kind).collect()
    }

    fn kinds(&self) -> Vec<&str> {
        self.streamed.iter().map(|l| l.kind.as_str()).collect()
    }
}

#[track_caller]
fn run(
    tool: Arc<CountingTool>,
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    cancel: Arc<TurnCancel>,
) -> Ran {
    run_with_home(
        fakes::TempDir::new("fiber-before-tool"),
        tool,
        script,
        hooks,
        cancel,
    )
}

#[track_caller]
fn run_with_home(
    home: fakes::TempDir,
    tool: Arc<CountingTool>,
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    cancel: Arc<TurnCancel>,
) -> Ran {
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let mut watcher = log.watch();
    let provider = Arc::new(fakes::ScriptedProvider::new(script));
    let (inbox, rx) = mpsc::channel();
    inbox
        .send(Delivery::Prompt(
            Message {
                content: vec![ContentPart::Text { text: "go".into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId("c_go".into())),
                },
            },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
    let mut looped = Loop::start(
        crate::Session {
            log,
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: vec![("builtin".to_owned(), tool as Arc<dyn Tool>)],
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
    .cancelled_by(cancel);
    if let Some(hooks) = hooks {
        looped = looped.hooks(hooks);
    }
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done_tx.send(looped.turn());
    });
    let turn = Deadline::after(TURN_DEADLINE)
        .recv(&done_rx)
        .expect("the turn ended");
    assert!(turn.is_ok(), "{turn:?}");
    drop(inbox);
    let streamed = fakes::within(
        "a turn_completed line or the log's end",
        Deadline::after(TURN_DEADLINE),
        move || {
            let mut streamed = Vec::new();
            while let Ok(Some(line)) = watcher.recv() {
                let last = line.kind == "turn_completed";
                streamed.push(line);
                if last {
                    break;
                }
            }
            streamed
        },
    );
    let session = home.path().join("s_test");
    let requests = provider.requests();
    Ran {
        turn,
        streamed,
        lines: log::read(&session).unwrap(),
        requests,
        session,
        _home: home,
    }
}

fn completed_of(ran: &Ran) -> &Envelope {
    ran.of_kind("tool_call_completed")
        .into_iter()
        .next()
        .expect("a completion")
}

fn obj(arguments: Value) -> Map<String, Value> {
    arguments.as_object().unwrap().clone()
}

/// The complete durable kinds of a turn whose call ran and whose next
/// step says "Done.".
fn ran_kinds() -> Vec<&'static str> {
    vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
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
}

/// The complete durable kinds of a turn whose call never started and no
/// permission line answers: as [`ran_kinds`], without `tool_call_started`.
fn unstarted_kinds() -> Vec<&'static str> {
    ran_kinds()
        .into_iter()
        .filter(|kind| *kind != "tool_call_started")
        .collect()
}

/// The complete durable kinds of a turn whose rewritten call the
/// credential deny refused: the `permission_resolved` line stands where
/// `tool_call_started` would.
fn credential_kinds() -> Vec<&'static str> {
    ran_kinds()
        .into_iter()
        .map(|kind| {
            if kind == "tool_call_started" {
                "permission_resolved"
            } else {
                kind
            }
        })
        .collect()
}

/// The complete durable kinds of a turn that ends with its batch: a
/// shutdown or a cancel before the start leaves no second step after the
/// completion.
fn ended_kinds() -> Vec<&'static str> {
    vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "tool_call_completed",
        "turn_completed",
    ]
}

#[test]
fn a_rewrite_that_fails_the_schema_fails_invalid_arguments_and_never_starts() {
    let tool = Arc::new(CountingTool::new("out"));
    let hooks = Arc::new(FakeHooks::with_before(BeforeToolOutcome::Rewritten {
        arguments: obj(json!({"count": "5"})),
        changed_by: vec!["fiber.test/guard".to_owned()],
    }));
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"count": 5})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = completed_of(&ran);
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "invalid_arguments");
    let text = completed.payload["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with(
            "Extension fiber.test/guard rewrote this call's arguments, and they do not match the tool's schema:"
        ),
        "{text}"
    );
    assert!(tool.runs().is_empty());
    assert!(ran.of_kind("tool_call_started").is_empty());
    assert!(ran.of_kind("permission_resolved").is_empty());
    assert!(hooks.check_seen().is_empty());
    assert_eq!(ran.durable_kinds(), unstarted_kinds());
}

#[test]
fn the_effects_function_sees_the_model_then_the_rewritten_arguments() {
    let tool = Arc::new(CountingTool::new("out"));
    let hooks = Arc::new(FakeHooks::with_before(BeforeToolOutcome::Rewritten {
        arguments: obj(json!({"path": "effects-boom"})),
        changed_by: vec!["fiber.test/guard".to_owned()],
    }));
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let calls = tool.effects_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].get("path").and_then(Value::as_str), Some("a.txt"));
    assert_eq!(
        calls[1].get("path").and_then(Value::as_str),
        Some("effects-boom")
    );
    let completed = completed_of(&ran);
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "tool_error");
    let text = completed.payload["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text,
        "Extension fiber.test/guard rewrote this call's arguments, and the tool could not classify them: the tool could not classify"
    );
    assert!(hooks.check_seen().is_empty());
    assert_eq!(ran.durable_kinds(), unstarted_kinds());
}

#[test]
fn the_decision_judges_the_rewritten_call() {
    let home = fakes::TempDir::new("fiber-before-tool-cred");
    let secret = home.path().join("credentials").join("secret.txt");
    let tool = Arc::new(CountingTool::new("out"));
    let hooks = Arc::new(FakeHooks::with_before(BeforeToolOutcome::Rewritten {
        arguments: obj(json!({"path": secret.display().to_string()})),
        changed_by: vec!["fiber.test/guard".to_owned()],
    }));
    let ran = run_with_home(
        home,
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    assert_eq!(ran.of_kind("permission_resolved").len(), 1);
    let resolved = &ran.of_kind("permission_resolved")[0];
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "credential_deny");
    let completed = completed_of(&ran);
    assert_eq!(completed.payload["status"], "denied");
    assert_eq!(completed.payload["reason"], "credentials");
    assert!(tool.runs().is_empty());
    assert_eq!(ran.durable_kinds(), credential_kinds());
}

#[test]
fn a_rewrite_out_of_credentials_runs_with_the_rewritten_arguments() {
    let home = fakes::TempDir::new("fiber-before-tool-uncred");
    let secret = home.path().join("credentials").join("secret.txt");
    let tool = Arc::new(CountingTool::new("out"));
    let hooks = Arc::new(FakeHooks::with_before(BeforeToolOutcome::Rewritten {
        arguments: obj(json!({"path": "a.txt"})),
        changed_by: vec!["fiber.test/guard".to_owned()],
    }));
    let ran = run_with_home(
        home,
        Arc::clone(&tool),
        script("probe", json!({"path": secret.display().to_string()})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    assert!(ran.turn.is_ok());
    assert_eq!(tool.runs().len(), 1);
    assert_eq!(
        tool.runs()[0].get("path").and_then(Value::as_str),
        Some("a.txt")
    );
    let started = &ran.of_kind("tool_call_started")[0];
    assert_eq!(started.payload["arguments"], json!({"path": "a.txt"}));
    assert_eq!(started.payload["changed_by"], json!(["fiber.test/guard"]));
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn a_rewritten_start_carries_arguments_and_changed_by_and_an_unchanged_one_carries_neither() {
    let tool = Arc::new(CountingTool::new("out"));
    let hooks = Arc::new(FakeHooks::with_before(BeforeToolOutcome::Rewritten {
        arguments: obj(json!({"path": "b.txt"})),
        changed_by: vec!["fiber.test/guard".to_owned()],
    }));
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let started = &ran.of_kind("tool_call_started")[0];
    assert_eq!(started.payload["arguments"], json!({"path": "b.txt"}));
    assert_eq!(started.payload["changed_by"], json!(["fiber.test/guard"]));

    let tool = Arc::new(CountingTool::new("out"));
    assert_eq!(ran.durable_kinds(), ran_kinds());
    let hooks = Arc::new(FakeHooks::new());
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let started = &ran.of_kind("tool_call_started")[0];
    assert!(started.payload.get("arguments").is_none());
    assert!(started.payload.get("changed_by").is_none());
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn a_refusal_is_denied_hook_and_never_runs() {
    let tool = Arc::new(CountingTool::new("out"));
    let hooks = Arc::new(FakeHooks::with_before(BeforeToolOutcome::Refused(
        contract::hook::Refusal {
            extension: "fiber.test/guard".to_owned(),
            reason: "no writes".to_owned(),
        },
    )));
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = completed_of(&ran);
    assert_eq!(completed.payload["status"], "denied");
    assert_eq!(completed.payload["reason"], "hook");
    assert_eq!(
        completed.payload["content"][0]["text"],
        "Extension fiber.test/guard refused this call: no writes. It did not run."
    );
    assert!(tool.runs().is_empty());
    assert!(ran.of_kind("tool_call_started").is_empty());
    assert!(ran.of_kind("permission_resolved").is_empty());
    assert!(hooks.check_seen().is_empty());
    assert_eq!(ran.durable_kinds(), unstarted_kinds());
}

#[test]
fn a_blocking_failure_fails_hook_failed_and_never_starts() {
    let tool = Arc::new(CountingTool::new("out"));
    let hooks = Arc::new(FakeHooks::with_before(BeforeToolOutcome::Failed(
        contract::hook::HookFailure {
            extension: "fiber.test/guard".to_owned(),
            message: "boom".to_owned(),
        },
    )));
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = completed_of(&ran);
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "hook_failed");
    assert_eq!(
        completed.payload["content"][0]["text"],
        "The `before_tool` hook of extension fiber.test/guard failed, so the call did not run: boom"
    );
    assert!(tool.runs().is_empty());
    assert!(hooks.check_seen().is_empty());
    assert_eq!(ran.durable_kinds(), unstarted_kinds());
}

#[test]
fn check_tool_sees_the_rewritten_call() {
    let tool = Arc::new(CountingTool::new("out"));
    let mut hooks = FakeHooks::with_before(BeforeToolOutcome::Rewritten {
        arguments: obj(json!({"path": "b.txt"})),
        changed_by: vec!["fiber.test/guard".to_owned()],
    });
    hooks
        .check_answers
        .get_mut()
        .unwrap()
        .push_back(CheckOutcome::Passed);
    let hooks = Arc::new(hooks);
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    assert_eq!(hooks.check_seen().len(), 1);
    assert_eq!(
        Value::Object(hooks.check_seen()[0].arguments.clone()),
        json!({"path": "b.txt"})
    );
    assert_eq!(hooks.check_seen()[0].paths, Some(vec!["b.txt".to_owned()]));
    assert!(ran.turn.is_ok());
    assert_eq!(tool.runs().len(), 1);
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn a_check_refusal_denies_and_a_check_failure_fails() {
    for (outcome, status, reason) in [
        (
            CheckOutcome::Refused(contract::hook::Refusal {
                extension: "fiber.test/guard".to_owned(),
                reason: "no".to_owned(),
            }),
            "denied",
            "hook",
        ),
        (
            CheckOutcome::Failed(contract::hook::HookFailure {
                extension: "fiber.test/guard".to_owned(),
                message: "boom".to_owned(),
            }),
            "failed",
            "hook_failed",
        ),
    ] {
        let tool = Arc::new(CountingTool::new("out"));
        let mut hooks = FakeHooks::new();
        hooks.check_answers.get_mut().unwrap().push_back(outcome);
        let hooks = Arc::new(hooks);
        let ran = run(
            Arc::clone(&tool),
            script("probe", json!({"path": "a.txt"})),
            Some(Arc::clone(&hooks)),
            Arc::new(TurnCancel::default()),
        );
        let completed = completed_of(&ran);
        assert_eq!(completed.payload["status"], status);
        if status == "denied" {
            assert_eq!(completed.payload["reason"], reason);
        } else {
            assert_eq!(completed.payload["error"]["code"], reason);
        }
        assert!(tool.runs().is_empty());
        assert_eq!(ran.durable_kinds(), unstarted_kinds());
    }
}

#[test]
fn notices_from_both_stages_carry_no_action_id_and_come_first() {
    let tool = Arc::new(CountingTool::new("out"));
    let mut hooks = FakeHooks::with_before(BeforeToolOutcome::Rewritten {
        arguments: obj(json!({"path": "b.txt"})),
        changed_by: vec!["fiber.test/guard".to_owned()],
    });
    hooks
        .notices_before
        .get_mut()
        .unwrap()
        .push(contract::events::Notice {
            code: contract::ErrorCode::HookFailed,
            message: "stage one".to_owned(),
            extension: Some("fiber.test/guard".to_owned()),
        });
    hooks
        .notices_check
        .get_mut()
        .unwrap()
        .push(contract::events::Notice {
            code: contract::ErrorCode::HookFailed,
            message: "stage two".to_owned(),
            extension: Some("fiber.test/guard".to_owned()),
        });
    hooks
        .check_answers
        .get_mut()
        .unwrap()
        .push_back(CheckOutcome::Passed);
    let hooks = Arc::new(hooks);
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let notices = ran.streamed_of_kind("notice");
    assert_eq!(notices.len(), 2);
    assert!(notices[0].action_id.is_none());
    assert!(notices[1].action_id.is_none());
    assert_eq!(notices[0].payload["message"], "stage one");
    assert_eq!(notices[1].payload["message"], "stage two");
    let kinds = ran.kinds();
    let at = |kind: &str| kinds.iter().position(|k| *k == kind).unwrap();
    assert!(at("notice") < at("tool_call_started"));
    assert!(ran.of_kind("permission_resolved").is_empty());
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn two_notices_from_one_stage_group_by_code() {
    // One `before_tool` batch reaches the log as one notice: the count
    // and the first item up front, with every item and its extension in
    // the list, still before the call starts.
    let tool = Arc::new(CountingTool::new("out"));
    let mut hooks = FakeHooks::with_before(BeforeToolOutcome::Unchanged);
    for (extension, message) in [
        ("fiber.test/a", "extension fiber.test/a failed softly"),
        ("fiber.test/b", "extension fiber.test/b failed softly"),
    ] {
        hooks
            .notices_before
            .get_mut()
            .unwrap()
            .push(contract::events::Notice {
                code: contract::ErrorCode::HookFailed,
                message: message.to_owned(),
                extension: Some(extension.to_owned()),
            });
    }
    hooks
        .check_answers
        .get_mut()
        .unwrap()
        .push_back(CheckOutcome::Passed);
    let hooks = Arc::new(hooks);
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let notices = ran.streamed_of_kind("notice");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].payload["code"], "hook_failed");
    assert_eq!(notices[0].payload.get("extension"), None);
    let message = notices[0].payload["message"].as_str().unwrap();
    assert!(
        message.starts_with("2 hooks failed: extension fiber.test/a failed softly\n"),
        "{message}"
    );
    assert!(
        message.contains("extension fiber.test/a failed softly (extension `fiber.test/a`)"),
        "{message}"
    );
    assert!(
        message.contains("extension fiber.test/b failed softly (extension `fiber.test/b`)"),
        "{message}"
    );
    let kinds = ran.kinds();
    let at = |kind: &str| kinds.iter().position(|k| *k == kind).unwrap();
    assert!(at("notice") < at("tool_call_started"));
}

#[test]
fn unchanged_and_passed_run_with_the_models_arguments() {
    let tool = Arc::new(CountingTool::new("out"));
    let hooks = Arc::new(FakeHooks::new());
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    assert_eq!(tool.runs().len(), 1);
    assert_eq!(
        Value::Object(tool.runs()[0].clone()),
        json!({"path": "a.txt"})
    );
    assert_eq!(ran.durable_kinds(), ran_kinds());

    let tool = Arc::new(CountingTool::new("out"));
    let ran_no_hooks = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        None,
        Arc::new(TurnCancel::default()),
    );
    assert_eq!(ran.kinds(), ran_no_hooks.kinds());
    assert_eq!(ran_no_hooks.durable_kinds(), ran_kinds());
}

#[test]
fn prepared_under_a_shutdown_cancels_and_calls_no_hook() {
    let home = fakes::TempDir::new("fiber-before-tool-shutdown");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let (_inbox, rx) = mpsc::channel();
    let cancel = Arc::new(TurnCancel::default());
    cancel.shutdown(143);
    let hooks = Arc::new(FakeHooks::new());
    let mut looped = Loop::start(
        crate::Session {
            log,
            provider: Arc::new(fakes::ScriptedProvider::new(vec![fakes::Scripted::text(
                "x",
            )])) as Arc<dyn Provider>,
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: vec![(
                "builtin".to_owned(),
                Arc::new(CountingTool::new("out")) as Arc<dyn Tool>,
            )],
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
    .cancelled_by(Arc::clone(&cancel))
    .hooks(hooks.clone() as Arc<dyn contract::hook::Hooks>);
    let call = ToolCallRequested {
        name: "probe".into(),
        arguments: json!({"path": "a.txt"}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    };
    let turn = contract::TurnId("t_test".into());
    let decided = looped.prepared(&call, Some(&turn)).unwrap();
    let (Err(completed), _) = decided else {
        panic!("a shutdown cancels the call");
    };
    assert_eq!(completed.status, contract::events::CallStatus::Cancelled);
    assert!(hooks.before_seen().is_empty());
    assert!(hooks.check_seen().is_empty());
}

#[test]
fn a_before_tool_that_starts_a_shutdown_cancels_before_check_tool() {
    let tool = Arc::new(CountingTool::new("out"));
    let cancel = Arc::new(TurnCancel::default());
    let mut fake = FakeHooks::with_before(BeforeToolOutcome::Rewritten {
        arguments: obj(json!({"path": "b.txt"})),
        changed_by: vec!["fiber.test/guard".to_owned()],
    });
    fake.shutdown = Some(Arc::clone(&cancel));
    let hooks = Arc::new(fake);
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        cancel,
    );
    assert!(hooks.check_seen().is_empty());
    assert!(ran.of_kind("tool_call_started").is_empty());
    let completed = completed_of(&ran);
    assert_eq!(completed.payload["status"], "cancelled");
    assert_eq!(ran.durable_kinds(), ended_kinds());
}

fn rewritten(arguments: Value, changed_by: &[&str]) -> FakeHooks {
    FakeHooks::with_before(BeforeToolOutcome::Rewritten {
        arguments: obj(arguments),
        changed_by: changed_by.iter().map(|s| (*s).to_owned()).collect(),
    })
}

fn completion_text(completed: &Envelope) -> String {
    completed.payload["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn a_rewritten_call_heads_its_result_with_the_rewrite_line() {
    let tool = Arc::new(CountingTool::new("the output"));
    let hooks = Arc::new(rewritten(json!({"path": "b.txt"}), &["fiber.test/guard"]));
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = completed_of(&ran);
    let line = "Extension fiber.test/guard rewrote this call's arguments; it ran with {\"path\":\"b.txt\"}.";
    assert_eq!(completion_text(completed), format!("{line}\nthe output"));
    // The call's single text part stays one part under the line.
    assert_eq!(completed.payload["content"].as_array().unwrap().len(), 1);
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn two_extensions_share_the_plural_line_in_changed_by_order() {
    let tool = Arc::new(CountingTool::new("the output"));
    let hooks = Arc::new(rewritten(
        json!({"path": "b.txt"}),
        &["fiber.test/a", "fiber.test/b"],
    ));
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let text = completion_text(completed_of(&ran));
    assert!(
        text.starts_with(
            "Extensions fiber.test/a, fiber.test/b rewrote this call's arguments; it ran with {\"path\":\"b.txt\"}.\n"
        ),
        "{text}"
    );
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn two_text_parts_are_joined_into_one_part_under_the_line() {
    let tool = Arc::new(CountingTool::with_output(Output {
        content: vec![
            ContentPart::Text { text: "one".into() },
            ContentPart::Text { text: "two".into() },
        ],
        ..Output::default()
    }));
    let hooks = Arc::new(rewritten(json!({"path": "b.txt"}), &["fiber.test/guard"]));
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = completed_of(&ran);
    let content = completed.payload["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    let line = "Extension fiber.test/guard rewrote this call's arguments; it ran with {\"path\":\"b.txt\"}.";
    assert_eq!(content[0]["text"], format!("{line}\none\ntwo"));
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn an_after_tool_change_keeps_the_line_first_and_sees_the_tools_output() {
    let tool = Arc::new(CountingTool::new("the output"));
    let mut hooks = rewritten(json!({"path": "b.txt"}), &["fiber.test/guard"]);
    hooks.after_answers.get_mut().unwrap().push_back((
        contract::hook::AfterToolOutcome::Changed {
            content: Some("redacted".to_owned()),
            details: None,
            artifact: None,
        },
        vec!["fiber.test/scrub".to_owned()],
    ));
    let hooks = Arc::new(hooks);
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let seen = hooks.after_seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        Value::Object(seen[0].arguments.clone()),
        json!({"path": "b.txt"})
    );
    assert_eq!(seen[0].content, "the output");
    let line = "Extension fiber.test/guard rewrote this call's arguments; it ran with {\"path\":\"b.txt\"}.";
    assert_eq!(
        completion_text(completed_of(&ran)),
        format!("{line}\nredacted")
    );
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn a_withheld_result_keeps_only_the_withheld_line() {
    let tool = Arc::new(CountingTool::new("the output"));
    let mut hooks = rewritten(json!({"path": "b.txt"}), &["fiber.test/guard"]);
    hooks.after_answers.get_mut().unwrap().push_back((
        contract::hook::AfterToolOutcome::Withheld {
            extension: "fiber.test/scrub".to_owned(),
        },
        vec!["fiber.test/scrub".to_owned()],
    ));
    let hooks = Arc::new(hooks);
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = completed_of(&ran);
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": "Output withheld: the `after_tool` hook of extension fiber.test/scrub failed."}])
    );
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn a_rewritten_call_cancelled_before_it_starts_has_no_line() {
    let tool = Arc::new(CountingTool::new("the output"));
    let cancel = Arc::new(TurnCancel::default());
    let mut hooks = rewritten(json!({"path": "b.txt"}), &["fiber.test/guard"]);
    hooks.cancel_in_before = Some(Arc::clone(&cancel));
    let hooks = Arc::new(hooks);
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        cancel,
    );
    assert!(ran.of_kind("tool_call_started").is_empty());
    let completed = completed_of(&ran);
    assert_eq!(completed.payload["status"], "cancelled");
    let text = completed.payload["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(!text.contains("rewrote this call's arguments"), "{text}");
    assert_eq!(ran.durable_kinds(), ended_kinds());
}

#[test]
fn a_cut_result_keeps_the_whole_line_and_the_artifact_holds_it_all() {
    let tool = Arc::new(CountingTool::new("the output"));
    let hooks = Arc::new(rewritten(json!({"path": "b.txt"}), &["fiber.test/guard"]));
    let uncut = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let full = completion_text(completed_of(&uncut));
    let line = full.lines().next().unwrap().to_owned();
    assert_eq!(uncut.durable_kinds(), ran_kinds());
    let start = line.len() + 1;

    let tool = Arc::new(CountingTool::with_bound(
        CountingTool::new("the output"),
        Bound { start, end: 0 },
    ));
    let hooks = Arc::new(rewritten(json!({"path": "b.txt"}), &["fiber.test/guard"]));
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = completed_of(&ran);
    let content = completion_text(completed);
    assert!(content.starts_with(&format!("{line}\n")), "{content}");
    let artifact = completed.payload["artifact"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(ran.session.join(artifact)).unwrap(),
        full
    );
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn a_one_byte_cap_cuts_the_line_itself() {
    let tool = Arc::new(CountingTool::with_bound(
        CountingTool::new("the output"),
        Bound { start: 1, end: 0 },
    ));
    let hooks = Arc::new(rewritten(json!({"path": "b.txt"}), &["fiber.test/guard"]));
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = completed_of(&ran);
    let content = completion_text(completed);
    assert!(content.starts_with("E\n["), "{content}");
    assert!(content.contains("bytes cut"), "{content}");
    let artifact = completed.payload["artifact"].as_str().unwrap();
    let text = std::fs::read_to_string(ran.session.join(artifact)).unwrap();
    let line = "Extension fiber.test/guard rewrote this call's arguments; it ran with {\"path\":\"b.txt\"}.";
    assert_eq!(text, format!("{line}\nthe output"));
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn a_hooks_artifact_replaces_the_cut_artifact_but_not_the_lines_head() {
    let tool = Arc::new(CountingTool::new("raw"));
    let mut hooks = rewritten(json!({"path": "b.txt"}), &["fiber.test/guard"]);
    hooks.after_answers.get_mut().unwrap().push_back((
        contract::hook::AfterToolOutcome::Changed {
            content: Some("summary".to_owned()),
            details: None,
            artifact: Some("the whole log".to_owned()),
        },
        vec!["fiber.test/scrub".to_owned()],
    ));
    let hooks = Arc::new(hooks);
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = completed_of(&ran);
    let artifact = completed.payload["artifact"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(ran.session.join(artifact)).unwrap(),
        "the whole log"
    );
    let line = "Extension fiber.test/guard rewrote this call's arguments; it ran with {\"path\":\"b.txt\"}.";
    assert!(
        completion_text(completed).starts_with(&format!("{line}\nsummary")),
        "{}",
        completion_text(completed)
    );
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

#[test]
fn the_next_request_sends_the_models_call_with_the_lines_result() {
    let tool = Arc::new(CountingTool::new("the output"));
    let hooks = Arc::new(rewritten(json!({"path": "b.txt"}), &["fiber.test/guard"]));
    let ran = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    assert_eq!(ran.requests.len(), 2);
    let second = &ran.requests[1];
    let call = second
        .conversation
        .iter()
        .find_map(|input| match input {
            contract::provider::Input::ToolCall { call, .. } => Some(call.clone()),
            contract::provider::Input::User { .. }
            | contract::provider::Input::Assistant { .. }
            | contract::provider::Input::Reasoning { .. }
            | contract::provider::Input::ToolResult { .. }
            | contract::provider::Input::ToolSearchResult { .. } => None,
        })
        .expect("the second request carries the tool call");
    assert_eq!(
        serde_json::to_value(&call).unwrap()["arguments"],
        json!({"path": "a.txt"})
    );
    let result = second
        .conversation
        .iter()
        .find_map(|input| match input {
            contract::provider::Input::ToolResult { text, .. } => Some(text.clone()),
            contract::provider::Input::User { .. }
            | contract::provider::Input::Assistant { .. }
            | contract::provider::Input::Reasoning { .. }
            | contract::provider::Input::ToolCall { .. }
            | contract::provider::Input::ToolSearchResult { .. } => None,
        })
        .expect("the second request carries the tool result");
    let line = "Extension fiber.test/guard rewrote this call's arguments; it ran with {\"path\":\"b.txt\"}.";
    assert!(
        result.starts_with(&format!("{line}\nthe output")),
        "{result}"
    );
    assert_eq!(ran.durable_kinds(), ran_kinds());
}

/// `Prepared::rewrite` yields the judged arguments with who rewrote them,
/// or neither when no hook rewrote the call.
#[test]
fn rewrite_yields_both_fields_or_neither() {
    let tool = Arc::new(CountingTool::new("ok"));
    let arguments: Map<String, Value> = serde_json::from_value(json!({"path": "a.txt"})).unwrap();
    let effects = Effects {
        declared: DeclaredEffects {
            effects: vec![Effect::Reads],
            reversible: true,
            paths: None,
            confined: false,
        },
        subject: Some(String::new()),
        prefix: None,
        always_reviewed: false,
        hosts: None,
    };
    let prepared = |rewritten: Option<Vec<String>>| super::Prepared {
        tool: tool.clone() as Arc<dyn Tool>,
        arguments: arguments.clone(),
        effects: effects.clone(),
        rewritten,
    };
    assert_eq!(
        prepared(Some(Vec::new())).rewrite(),
        (Some(arguments.clone()), Some(Vec::new()))
    );
    assert_eq!(
        prepared(Some(vec!["e".to_owned()])).rewrite(),
        (Some(arguments.clone()), Some(vec!["e".to_owned()]))
    );
    assert_eq!(prepared(None).rewrite(), (None, None));
}
