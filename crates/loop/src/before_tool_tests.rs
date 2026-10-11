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
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, SessionId};
use fakes::Deadline;
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

/// A scripted `Hooks` fake: per-method seen calls, a queue of answers per
/// method, and an optional shutdown to start inside `before_tool`.
struct FakeHooks {
    before_seen: Mutex<Vec<Seen>>,
    check_seen: Mutex<Vec<Seen>>,
    before_answers: Mutex<VecDeque<BeforeToolOutcome>>,
    check_answers: Mutex<VecDeque<CheckOutcome>>,
    notices_before: Mutex<Vec<contract::events::Notice>>,
    notices_check: Mutex<Vec<contract::events::Notice>>,
    shutdown: Option<Arc<TurnCancel>>,
    rewrite_and_shutdown: bool,
}

impl FakeHooks {
    fn new() -> Self {
        Self {
            before_seen: Mutex::new(Vec::new()),
            check_seen: Mutex::new(Vec::new()),
            before_answers: Mutex::new(VecDeque::new()),
            check_answers: Mutex::new(VecDeque::new()),
            notices_before: Mutex::new(Vec::new()),
            notices_check: Mutex::new(Vec::new()),
            shutdown: None,
            rewrite_and_shutdown: false,
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
        _call: &contract::hook::AfterToolCall<'_>,
    ) -> contract::hook::AfterToolAnswer {
        contract::hook::AfterToolAnswer {
            outcome: contract::hook::AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn before_tool(&self, call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        self.before_seen.lock().unwrap().push(seen_of(call));
        if self.rewrite_and_shutdown
            && let Some(cancel) = &self.shutdown
        {
            cancel.shutdown(143);
        } else if let Some(cancel) = &self.shutdown
            && !self.rewrite_and_shutdown
        {
            let _ = cancel;
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

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<Delivery>) {}
}

struct NoRules;

impl Rules for NoRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

/// A schema'd counting tool: schema takes an optional string `path` and an
/// optional integer `count`; effects declare `path` as the touched path and
/// record every call. An effects error when `path` is `effects-boom`.
struct CountingTool {
    runs: Mutex<Vec<Map<String, Value>>>,
    effects_calls: Mutex<Vec<Map<String, Value>>>,
    output: String,
}

impl CountingTool {
    fn new(output: &str) -> Self {
        Self {
            runs: Mutex::new(Vec::new()),
            effects_calls: Mutex::new(Vec::new()),
            output: output.to_owned(),
        }
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
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
    }

    fn run(&self, arguments: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        self.runs.lock().unwrap().push(arguments.clone());
        Output {
            content: vec![ContentPart::Text {
                text: self.output.clone(),
            }],
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
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
    #[allow(dead_code, reason = "task 2.2 reads the session directory")]
    #[allow(dead_code, reason = "task 2.2 reads the session directory")]
    session: std::path::PathBuf,
    _home: fakes::TempDir,
}

impl Ran {
    fn of_kind(&self, kind: &str) -> Vec<&Envelope> {
        self.lines.iter().filter(|l| l.kind == kind).collect()
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
        TURN_DEADLINE,
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
    Ran {
        turn,
        streamed,
        lines: log::read(&session).unwrap(),
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

    let tool = Arc::new(CountingTool::new("out"));
    let ran_no_hooks = run(
        Arc::clone(&tool),
        script("probe", json!({"path": "a.txt"})),
        None,
        Arc::new(TurnCancel::default()),
    );
    assert_eq!(ran.kinds(), ran_no_hooks.kinds());
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
    let decided = looped.prepared(&call, &turn).unwrap();
    let Err(completed) = decided else {
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
    fake.rewrite_and_shutdown = true;
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
}
