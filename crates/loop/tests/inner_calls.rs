//! Inner calls through the loop, with fake tools and a scripted provider.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

mod support;

use contract::Envelope;
use contract::emit::Emit;
use contract::events::{CallStatus, ToolCallCompleted, TurnOutcome};
use contract::provider::{Input, ToolDefinition};
use contract::shapes::{ContentPart, DeclaredEffects, Effect};
use contract::tool::{Ask, Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};
use support::{Session, calls_reply};

type Run = dyn Fn(&Map<String, Value>, &dyn Cancel, &dyn Emit, &dyn Ask) -> Output + Send + Sync;

struct FakeTool {
    name: &'static str,
    schema: Value,
    effects: Effects,
    effects_error: Option<EffectsError>,
    paths: Mutex<Option<Vec<String>>>,
    ran: Mutex<Vec<Map<String, Value>>>,
    run: Arc<Run>,
}

impl FakeTool {
    fn new(
        name: &'static str,
        run: impl Fn(&Map<String, Value>, &dyn Cancel, &dyn Emit, &dyn Ask) -> Output
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            name,
            schema: json!({"type":"object"}),
            effects: Effects {
                declared: DeclaredEffects {
                    effects: vec![Effect::Reads],
                    reversible: true,
                    paths: None,
                },
                subject: Some("x".into()),
                prefix: None,
                always_reviewed: false,
            },
            effects_error: None,
            paths: Mutex::default(),
            ran: Mutex::default(),
            run: Arc::new(run),
        }
    }
}

impl Tool for FakeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: "fake tool".into(),
            input_schema: self.schema.clone(),
            deferred: false,
            hosted: None,
        }
    }
    fn effects(&self, args: &Map<String, Value>) -> Result<Effects, EffectsError> {
        if let Some(error) = &self.effects_error {
            return Err(error.clone());
        }
        let mut effects = self.effects.clone();
        if let Some(paths) = self.paths.lock().unwrap().as_ref() {
            effects.declared.paths = Some(paths.clone());
        }
        if let Some(path) = args.get("path").and_then(Value::as_str) {
            effects.declared.paths = Some(vec![path.into()]);
        }
        Ok(effects)
    }
    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        panic!("loop must use run_asking")
    }
    fn run_asking(
        &self,
        args: &Map<String, Value>,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        self.ran.lock().unwrap().push(args.clone());
        (self.run)(args, cancel, emit, ask)
    }
}

fn text(text: impl Into<String>) -> Output {
    Output {
        content: vec![ContentPart::Text { text: text.into() }],
        ..Default::default()
    }
}

fn outer(
    name: &'static str,
    arguments: Value,
) -> (Arc<FakeTool>, Arc<Mutex<Vec<ToolCallCompleted>>>) {
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let seen = outcomes.clone();
    let tool = FakeTool::new("outer", move |_, _, _, ask| {
        let Some(runner) = ask.inner_calls() else {
            return text("no inner runner");
        };
        match runner.run("ext", name, arguments.clone()) {
            Ok(done) => {
                let output = text(serde_json::to_string(&done.content).unwrap());
                seen.lock().unwrap().push(*done);
                output
            }
            Err(error) => text(error),
        }
    });
    (Arc::new(tool), outcomes)
}

fn session(tools: Vec<Arc<dyn Tool>>) -> Session {
    let session = Session::with_tools(
        vec![
            calls_reply("", &[("outer", json!({}))]),
            fakes::Scripted::text("done"),
        ],
        None,
        tools,
    );
    session.inbox.send(support::delivery("go")).unwrap();
    session
}

fn assert_turn_events(lines: &[Envelope], inner: &[&str]) {
    let mut expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "tool_call_arguments_delta",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "tool_call_started",
    ];
    expected.extend_from_slice(inner);
    expected.extend_from_slice(&[
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
    assert_eq!(support::kinds(lines), expected);
}

fn inner_id(lines: &[Envelope]) -> contract::ActionId {
    lines
        .iter()
        .find(|l| l.kind == "tool_call_requested" && l.payload.contains_key("ran_by"))
        .unwrap()
        .action_id
        .clone()
        .unwrap()
}

fn action_kinds<'a>(lines: &'a [Envelope], id: &contract::ActionId) -> Vec<&'a str> {
    lines
        .iter()
        .filter(|l| l.action_id.as_ref() == Some(id))
        .map(|l| l.kind.as_str())
        .collect()
}

#[test]
fn the_inner_result_is_logged_before_the_outer_and_only_the_outer_reaches_the_provider() {
    let (outer, outcomes) = outer("inner", json!({}));
    let inner = Arc::new(FakeTool::new("inner", |_, _, _, _| text("inner answer")));
    let mut session = session(vec![outer, inner]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let id = inner_id(&lines);
    assert_turn_events(
        &lines,
        &[
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
        ],
    );
    assert_eq!(
        action_kinds(&lines, &id),
        [
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed"
        ]
    );
    let requested = lines
        .iter()
        .find(|l| l.action_id.as_ref() == Some(&id))
        .unwrap();
    let outer = lines
        .iter()
        .find(|l| l.kind == "tool_call_requested" && !l.payload.contains_key("ran_by"))
        .unwrap();
    assert_ne!(id, outer.action_id.clone().unwrap());
    assert_eq!(requested.turn_id, outer.turn_id);
    assert_eq!(
        requested.payload["ran_by"],
        json!({"extension":"ext","outer_action_id":outer.action_id})
    );
    let completions: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .map(|l| l.action_id.as_ref().unwrap())
        .collect();
    assert_eq!(completions, [&id, outer.action_id.as_ref().unwrap()]);
    let outcomes = outcomes.lock().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, CallStatus::Completed);
    assert_eq!(outcomes[0].content, text("inner answer").content);
    let requests = session.requests();
    let calls: Vec<_> = requests[1]
        .conversation
        .iter()
        .filter(|i| matches!(i, Input::ToolCall { .. } | Input::ToolResult { .. }))
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(matches!(calls[0], Input::ToolCall { call, .. } if call.name == "outer"));
    assert!(matches!(calls[1], Input::ToolResult { text, .. } if text.contains("inner answer")));
    assert!(
        !serde_json::to_string(&requests[1].conversation)
            .unwrap()
            .contains(&id.0)
    );
}

#[test]
fn credentials_and_standing_denies_refuse_inner_calls_without_starting_them() {
    use contract::rules::{Rule, RuleDecision, StandingRules};
    for credentials in [false, true] {
        let (outer, outcomes) = outer("inner", json!({}));
        let inner = Arc::new(FakeTool::new("inner", |_, _, _, _| text("secret")));
        let mut session = session(vec![outer, inner.clone()]);
        if credentials {
            *inner.paths.lock().unwrap() = Some(vec![
                session.credentials.join("token").display().to_string(),
            ]);
        } else {
            session.rules.set(StandingRules {
                global: vec![],
                project: vec![Rule {
                    decision: RuleDecision::Deny,
                    tool: "inner".into(),
                    prefix: "x".into(),
                    added: None,
                    session_id: None,
                }],
            });
        }
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let lines = session.lines();
        let id = inner_id(&lines);
        assert_eq!(
            action_kinds(&lines, &id),
            [
                "tool_call_requested",
                "permission_resolved",
                "tool_call_completed"
            ]
        );
        let reason = if credentials {
            "credentials"
        } else {
            "standing_rule"
        };
        assert_eq!(outcomes.lock().unwrap()[0].status, CallStatus::Denied);
        assert_eq!(outcomes.lock().unwrap()[0].reason.as_deref(), Some(reason));
        assert!(inner.ran.lock().unwrap().is_empty());
        let resolved = lines
            .iter()
            .find(|l| l.kind == "permission_resolved")
            .unwrap();
        assert_eq!(resolved.payload["decision"], "deny");
        assert_eq!(
            resolved.payload["decided_by"],
            if credentials {
                "credential_deny"
            } else {
                "standing_rule"
            }
        );
        assert!(
            resolved.payload["reason"]
                .as_str()
                .unwrap()
                .contains(if credentials {
                    "credential"
                } else {
                    "standing"
                })
        );
    }
}

#[test]
fn standing_asks_executes_and_outer_only_effects_do_not_ask_or_review_inner_calls() {
    use contract::rules::{Rule, RuleDecision, StandingRules};
    for effect in [Effect::Reads, Effect::Executes, Effect::Writes] {
        let (outer, outcomes) = outer("inner", json!({}));
        let mut inner = FakeTool::new("inner", |_, _, _, _| text("allowed"));
        inner.effects.declared.effects = vec![effect];
        if effect == Effect::Writes {
            inner.effects.declared.paths = Some(vec!["/outside/workspace/file".into()]);
        }
        let inner = Arc::new(inner);
        let mut session = session(vec![outer, inner.clone()]);
        session.rules.set(StandingRules {
            global: vec![],
            project: vec![Rule {
                decision: RuleDecision::Ask,
                tool: "inner".into(),
                prefix: "x".into(),
                added: None,
                session_id: None,
            }],
        });
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let lines = session.lines();
        assert_eq!(
            action_kinds(&lines, &inner_id(&lines)),
            [
                "tool_call_requested",
                "tool_call_started",
                "tool_call_completed"
            ]
        );
        assert_eq!(outcomes.lock().unwrap()[0].status, CallStatus::Completed);
        assert_eq!(inner.ran.lock().unwrap().len(), 1);
        assert!(!lines.iter().any(|l| l.kind.starts_with("permission_")));
    }
}

#[test]
fn unknown_schema_nonobject_and_effects_failures_are_ordinary_inner_returns() {
    for (name, arguments, schema, effects_error, code) in [
        (
            "missing",
            json!({}),
            json!({"type":"object"}),
            None,
            "unknown_tool",
        ),
        (
            "inner",
            json!({}),
            json!({"type":"object","required":["word"]}),
            None,
            "invalid_arguments",
        ),
        (
            "inner",
            json!([1]),
            json!({"type":"object"}),
            None,
            "invalid_arguments",
        ),
        (
            "inner",
            json!({}),
            json!({"type":"object"}),
            Some(EffectsError::Tool("cannot classify".into())),
            "tool_error",
        ),
    ] {
        let (outer, outcomes) = outer(name, arguments);
        let mut inner = FakeTool::new("inner", |_, _, _, _| text("must not run"));
        inner.schema = schema;
        inner.effects_error = effects_error;
        let inner = Arc::new(inner);
        let mut session = session(vec![outer, inner.clone()]);
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let lines = session.lines();
        assert_eq!(
            action_kinds(&lines, &inner_id(&lines)),
            ["tool_call_requested", "tool_call_completed"]
        );
        let outcomes = outcomes.lock().unwrap();
        assert_eq!(outcomes[0].status, CallStatus::Failed);
        assert_eq!(
            serde_json::to_value(&outcomes[0].error).unwrap()["code"],
            code
        );
        assert!(inner.ran.lock().unwrap().is_empty());
    }
}

struct FakeHooks {
    before: contract::hook::BeforeToolOutcome,
    check: contract::hook::CheckOutcome,
    after: contract::hook::AfterToolOutcome,
    seen: Mutex<Vec<String>>,
}

impl Default for FakeHooks {
    fn default() -> Self {
        Self {
            before: contract::hook::BeforeToolOutcome::Unchanged,
            check: contract::hook::CheckOutcome::Passed,
            after: contract::hook::AfterToolOutcome::Unchanged,
            seen: Mutex::default(),
        }
    }
}

impl contract::hook::Hooks for FakeHooks {
    fn before_tool(
        &self,
        call: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::BeforeToolAnswer {
        self.seen
            .lock()
            .unwrap()
            .push(format!("before {}", call.tool));
        contract::hook::BeforeToolAnswer {
            outcome: if call.tool == "inner" {
                self.before.clone()
            } else {
                contract::hook::BeforeToolOutcome::Unchanged
            },
            notices: vec![],
        }
    }
    fn check_tool(&self, call: &contract::hook::BeforeToolCall<'_>) -> contract::hook::CheckAnswer {
        self.seen
            .lock()
            .unwrap()
            .push(format!("check {}", call.tool));
        contract::hook::CheckAnswer {
            outcome: if call.tool == "inner" {
                self.check.clone()
            } else {
                contract::hook::CheckOutcome::Passed
            },
            notices: vec![],
        }
    }
    fn after_tool(
        &self,
        call: &contract::hook::AfterToolCall<'_>,
    ) -> contract::hook::AfterToolAnswer {
        self.seen
            .lock()
            .unwrap()
            .push(format!("after {}", call.tool));
        contract::hook::AfterToolAnswer {
            outcome: if call.tool == "inner" {
                self.after.clone()
            } else {
                contract::hook::AfterToolOutcome::Unchanged
            },
            changed_by: vec![],
            notices: vec![],
        }
    }
    fn before_message(
        &self,
        _: &contract::hook::BeforeMessage<'_>,
    ) -> contract::hook::BeforeMessageAnswer {
        contract::hook::BeforeMessageAnswer {
            outcome: contract::hook::BeforeMessageOutcome::Passed {
                replaced: None,
                context: vec![],
            },
            notices: vec![],
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
    fn deliver_to(&self, _: std::sync::mpsc::Sender<contract::inbox::Delivery>) {}
}

#[test]
fn inner_arguments_are_rewritten_and_results_sanitized_before_return() {
    use contract::hook::{AfterToolOutcome, BeforeToolOutcome};
    let (outer, outcomes) = outer("inner", json!({"word":"original"}));
    let inner = Arc::new(FakeTool::new("inner", |_, _, _, _| text("raw secret")));
    let hooks = Arc::new(FakeHooks {
        before: BeforeToolOutcome::Rewritten {
            arguments: json!({"word":"rewrite"}).as_object().unwrap().clone(),
            changed_by: vec!["guard".into()],
        },
        after: AfterToolOutcome::Changed {
            content: Some("[redacted]".into()),
            details: None,
            artifact: None,
        },
        ..Default::default()
    });
    let mut session = session(vec![outer, inner.clone()]);
    session.looped = Some(session.looped.take().unwrap().hooks(hooks.clone()));
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(inner.ran.lock().unwrap()[0]["word"], "rewrite");
    let id = inner_id(&lines);
    let started = lines
        .iter()
        .find(|l| l.kind == "tool_call_started" && l.action_id.as_ref() == Some(&id))
        .unwrap();
    assert_eq!(started.payload["arguments"], json!({"word":"rewrite"}));
    assert_eq!(started.payload["changed_by"], json!(["guard"]));
    let result = &outcomes.lock().unwrap()[0];
    assert!(
        serde_json::to_string(&result.content)
            .unwrap()
            .contains("[redacted]")
    );
    assert!(
        !serde_json::to_string(&result.content)
            .unwrap()
            .contains("raw secret")
    );
    assert_eq!(
        *hooks.seen.lock().unwrap(),
        [
            "before outer",
            "check outer",
            "before inner",
            "check inner",
            "after inner",
            "after outer"
        ]
    );
}

#[test]
fn before_and_check_refusals_and_hook_failures_never_start_the_inner_tool() {
    use contract::hook::{BeforeToolOutcome, CheckOutcome, HookFailure, Refusal};
    for case in 0..3 {
        let refusal = Refusal {
            extension: "guard".into(),
            reason: "unsafe".into(),
        };
        let hooks = Arc::new(match case {
            0 => FakeHooks {
                before: BeforeToolOutcome::Refused(refusal),
                ..Default::default()
            },
            1 => FakeHooks {
                check: CheckOutcome::Refused(refusal),
                ..Default::default()
            },
            _ => FakeHooks {
                before: BeforeToolOutcome::Failed(HookFailure {
                    extension: "guard".into(),
                    message: "failed".into(),
                }),
                ..Default::default()
            },
        });
        let (outer, outcomes) = outer("inner", json!({}));
        let inner = Arc::new(FakeTool::new("inner", |_, _, _, _| text("must not run")));
        let mut session = session(vec![outer, inner.clone()]);
        session.looped = Some(session.looped.take().unwrap().hooks(hooks));
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let lines = session.lines();
        assert_eq!(
            action_kinds(&lines, &inner_id(&lines)),
            ["tool_call_requested", "tool_call_completed"]
        );
        assert!(inner.ran.lock().unwrap().is_empty());
        let result = &outcomes.lock().unwrap()[0];
        if case == 2 {
            assert_eq!(result.status, CallStatus::Failed);
        } else {
            assert_eq!(result.status, CallStatus::Denied);
            assert_eq!(result.reason.as_deref(), Some("hook"));
        }
    }
}

fn chain(name: &'static str, next: &'static str) -> Arc<FakeTool> {
    Arc::new(FakeTool::new(name, move |_, _, _, ask| {
        let Some(runner) = ask.inner_calls() else {
            return text("no runner");
        };
        match runner.run("ext", next, json!({})) {
            Err(error) => text(error),
            Ok(done) => text(serde_json::to_string(&done.content).unwrap()),
        }
    }))
}

#[test]
fn eight_inner_levels_run_and_the_ninth_is_an_error_without_a_line() {
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("a", json!({}))]),
            fakes::Scripted::text("done"),
        ],
        None,
        vec![chain("a", "b"), chain("b", "a")],
    );
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let requested: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_requested")
        .collect();
    assert_eq!(requested.len(), 9);
    for pair in requested.windows(2) {
        assert_eq!(
            pair[1].payload["ran_by"]["outer_action_id"],
            serde_json::to_value(&pair[0].action_id).unwrap()
        );
        assert_eq!(pair[1].payload["ran_by"]["extension"], "ext");
        assert_eq!(pair[1].turn_id, pair[0].turn_id);
        assert_eq!(
            action_kinds(&lines, pair[1].action_id.as_ref().unwrap()),
            [
                "tool_call_requested",
                "tool_call_started",
                "tool_call_completed"
            ]
        );
    }
    let completed: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .collect();
    assert_eq!(completed.len(), 9);
    assert_eq!(
        completed
            .iter()
            .map(|l| l.action_id.as_ref())
            .collect::<Vec<_>>(),
        requested
            .iter()
            .rev()
            .map(|l| l.action_id.as_ref())
            .collect::<Vec<_>>()
    );
    assert!(
        completed[0].payload["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("depth exceeds 8")
    );
}

#[test]
fn a_direct_self_call_is_an_error_without_an_inner_line() {
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("a", json!({}))]),
            fakes::Scripted::text("done"),
        ],
        None,
        vec![chain("a", "a")],
    );
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.kind == "tool_call_requested")
            .count(),
        1
    );
    let done = lines
        .iter()
        .find(|l| l.kind == "tool_call_completed")
        .unwrap();
    assert!(
        done.payload["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cannot run itself")
    );
}

struct CancelWake(std::sync::mpsc::Sender<()>);
impl contract::clock::Wake for CancelWake {
    fn wake(&self) {
        let _sent = self.0.send(());
    }
}

fn waiting_inner(
    running: std::sync::mpsc::Sender<()>,
    cancelled: std::sync::mpsc::Sender<()>,
    release: Option<std::sync::mpsc::Receiver<()>>,
) -> Arc<FakeTool> {
    let release = Mutex::new(release);
    Arc::new(FakeTool::new("inner", move |_, cancel, _, _| {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake: Arc<dyn contract::clock::Wake> = Arc::new(CancelWake(tx));
        cancel.subscribe(Arc::downgrade(&wake));
        running.send(()).unwrap();
        if !cancel.is_cancelled() {
            fakes::Deadline::after(std::time::Duration::from_secs(2))
                .recv_or_fail(&rx, "inner cancellation");
        }
        assert!(cancel.is_cancelled());
        cancelled.send(()).unwrap();
        if let Some(release) = release.lock().unwrap().as_ref() {
            let _released = release.recv();
        }
        text("stopped")
    }))
}

fn start_turn(
    session: &mut Session,
) -> (
    std::sync::mpsc::Receiver<(r#loop::Loop, Option<TurnOutcome>)>,
    std::thread::ThreadId,
) {
    let mut looped = session.looped.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        let _sent = tx.send((looped, outcome));
    });
    (rx, thread.thread().id())
}

#[test]
fn turn_cancellation_and_shutdown_complete_the_inner_before_the_outer() {
    for shutdown in [false, true] {
        let deadline = fakes::Deadline::after(std::time::Duration::from_secs(3));
        let (started, running) = std::sync::mpsc::channel();
        let (stopped, cancelled) = std::sync::mpsc::channel();
        let inner = waiting_inner(started, stopped, None);
        let (outer, outcomes) = outer("inner", json!({}));
        let mut session = session(vec![outer, inner]);
        let hooks = Arc::new(FakeHooks::default());
        session.looped = Some(session.looped.take().unwrap().hooks(hooks.clone()));
        let (done, _) = start_turn(&mut session);
        deadline.recv_or_fail(&running, "inner running");
        if shutdown {
            session.cancel.shutdown(143);
        } else {
            assert!(session.cancel.cancel());
        }
        deadline.recv_or_fail(&cancelled, "inner saw cancellation");
        let (looped, outcome) = deadline.recv_or_fail(&done, "cancelled turn");
        session.looped = Some(looped);
        assert_eq!(outcome, Some(TurnOutcome::Interrupted));
        let lines = session.lines();
        let id = inner_id(&lines);
        let completions: Vec<_> = lines
            .iter()
            .filter(|l| l.kind == "tool_call_completed")
            .collect();
        assert_eq!(completions.len(), 2);
        assert_eq!(completions[0].action_id.as_ref(), Some(&id));
        assert_eq!(completions[0].payload["status"], "cancelled");
        assert_eq!(completions[1].payload["status"], "cancelled");
        assert_eq!(outcomes.lock().unwrap()[0].status, CallStatus::Cancelled);
        if shutdown {
            assert_eq!(
                *hooks.seen.lock().unwrap(),
                ["before outer", "check outer", "before inner", "check inner"]
            );
        } else {
            assert!(hooks.seen.lock().unwrap().contains(&"after inner".into()));
        }
    }
}

#[test]
fn an_outer_return_cancels_its_inner_and_holds_its_completion_without_cancelling_itself() {
    let deadline = fakes::Deadline::after(std::time::Duration::from_secs(3));
    let (started, running) = std::sync::mpsc::channel();
    let (stopped, cancelled) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let inner = waiting_inner(started, stopped, Some(held));
    let running = Mutex::new(running);
    let (answer, received) = std::sync::mpsc::channel();
    let outer = Arc::new(FakeTool::new("outer", move |_, _, _, ask| {
        let Some(runner) = ask.inner_calls() else {
            return text("no runner");
        };
        let answer = answer.clone();
        std::thread::spawn(move || {
            let _sent = answer.send(runner.run("ext", "inner", json!({})));
        });
        deadline.recv_or_fail(
            &running.lock().unwrap(),
            "inner running before outer returns",
        );
        text("outer returned")
    }));
    let mut session = session(vec![outer, inner]);
    let (done, id) = start_turn(&mut session);
    deadline.recv_or_fail(&cancelled, "anchor cancellation");
    assert!(
        session
            .clock
            .await_thread_parked(id, None, std::time::Duration::from_secs(2)),
        "loop waits for the cancelled inner result before completing its outer"
    );
    let durable = log::read(&session.dir).unwrap();
    assert!(!durable.iter().any(|l| l.kind == "tool_call_completed"));
    release.send(()).unwrap();
    let (looped, outcome) = deadline.recv_or_fail(&done, "outer and inner completion");
    session.looped = Some(looped);
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(
        deadline
            .recv_or_fail(&received, "inner answer")
            .unwrap()
            .status,
        CallStatus::Cancelled
    );
    let lines = session.lines();
    let completed: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .collect();
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0].action_id.as_ref(), Some(&inner_id(&lines)));
    assert_eq!(completed[0].payload["status"], "cancelled");
    assert_eq!(completed[1].payload["status"], "completed");
    assert_eq!(completed[1].payload["content"][0]["text"], "outer returned");
}

#[test]
fn a_request_taken_after_turn_cancellation_completes_without_starting() {
    let signal = Arc::new(Mutex::new(None::<Arc<r#loop::TurnCancel>>));
    let worker_signal = signal.clone();
    let results = Arc::new(Mutex::new(Vec::new()));
    let worker_results = results.clone();
    let outer = Arc::new(FakeTool::new("outer", move |_, _, _, ask| {
        let Some(runner) = ask.inner_calls() else {
            return text("no runner");
        };
        assert!(worker_signal.lock().unwrap().as_ref().unwrap().cancel());
        worker_results
            .lock()
            .unwrap()
            .push(runner.run("ext", "inner", json!({})).unwrap().status);
        text("cancelled")
    }));
    let inner = Arc::new(FakeTool::new("inner", |_, _, _, _| text("must not run")));
    let mut session = session(vec![outer, inner.clone()]);
    *signal.lock().unwrap() = Some(session.cancel.clone());
    assert_eq!(session.turn(), Some(TurnOutcome::Interrupted));
    let lines = session.lines();
    assert_eq!(
        action_kinds(&lines, &inner_id(&lines)),
        ["tool_call_requested", "tool_call_completed"]
    );
    assert_eq!(*results.lock().unwrap(), [CallStatus::Cancelled]);
    assert!(inner.ran.lock().unwrap().is_empty());
}

#[test]
fn inner_deltas_are_paced_on_the_injected_clock_and_flushed_under_the_inner_action() {
    use contract::clock::Clock;
    use contract::events::{Event, Progress};
    let deadline = fakes::Deadline::after(std::time::Duration::from_secs(3));
    let (release_first, first) = std::sync::mpsc::channel();
    let (release_last, last) = std::sync::mpsc::channel();
    let (held, holding) = std::sync::mpsc::channel();
    let first = Mutex::new(first);
    let last = Mutex::new(last);
    let inner = Arc::new(FakeTool::new("inner", move |_, _, emit, _| {
        emit.emit(&Event::ToolCallDelta(Progress {
            text: Some("a".into()),
            details: None,
        }));
        let _released = first.lock().unwrap().recv();
        emit.emit(&Event::ToolCallDelta(Progress {
            text: Some("b".into()),
            details: None,
        }));
        held.send(()).unwrap();
        let _released = last.lock().unwrap().recv();
        emit.emit(&Event::ToolCallDelta(Progress {
            text: Some("final".into()),
            details: None,
        }));
        text("answer")
    }));
    let (outer, _) = outer("inner", json!({}));
    let mut session = session(vec![outer, inner]);
    let tap = support::Tap::new(&session.log);
    let (done, thread) = start_turn(&mut session);
    let a = tap.wait_for_delta("a");
    release_first.send(()).unwrap();
    deadline.recv_or_fail(&holding, "the inner stream holds b");
    let interval = std::time::Duration::from_millis(100);
    assert!(
        session.clock.await_thread_parked(
            thread,
            Some(session.clock.now() + interval),
            std::time::Duration::from_secs(2)
        ),
        "inner delta waits on its clock interval"
    );
    session.clock.advance(interval);
    let b = tap.wait_for_delta("b");
    assert_eq!(b.action_id, a.action_id);
    release_last.send(()).unwrap();
    let (looped, outcome) = deadline.recv_or_fail(&done, "inner delta turn");
    session.looped = Some(looped);
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let lines = session.lines();
    let id = inner_id(&lines);
    assert_eq!(
        action_kinds(&lines, &id),
        [
            "tool_call_requested",
            "tool_call_started",
            "tool_call_delta",
            "tool_call_delta",
            "tool_call_delta",
            "tool_call_completed"
        ]
    );
    let deltas: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_delta")
        .map(|l| l.payload["text"].as_str().unwrap())
        .collect();
    assert_eq!(deltas, ["a", "b", "final"]);
}

fn asking(prompt: &str) -> contract::tool::Asking {
    contract::tool::Asking {
        interaction: contract::events::Interaction::Confirm {
            prompt: prompt.into(),
        },
        action_ids: vec![],
        until: None,
        check: None,
        suspends: false,
    }
}

#[test]
fn an_inner_ask_is_declined_but_a_model_ask_in_the_same_answerable_session_is_raised() {
    let deadline = fakes::Deadline::after(std::time::Duration::from_secs(3));
    let inner = Arc::new(FakeTool::new("inner", |_, _, _, ask| {
        assert!(!ask.answerable());
        // Deliberately does not test answerability before raising.
        assert_eq!(
            ask.ask(asking("inner?")),
            contract::tool::Answered::NoAnswer
        );
        text("no answer")
    }));
    let model_ask = Arc::new(FakeTool::new("question", |_, _, _, ask| {
        assert!(ask.answerable());
        assert_eq!(
            ask.ask(asking("model?")),
            contract::tool::Answered::Reply(contract::events::Answer::Confirmed {
                confirmed: true
            })
        );
        text("confirmed")
    }));
    let (outer, outcomes) = outer("inner", json!({}));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("outer", json!({}))]),
            calls_reply("", &[("question", json!({}))]),
            fakes::Scripted::text("done"),
        ],
        None,
        vec![outer, inner, model_ask],
    )
    .answerable(true)
    .inbox_woken();
    session.inbox.send(support::delivery("go")).unwrap();
    let tap = support::Tap::new(&session.log);
    let (done, _) = start_turn(&mut session);
    let asked = tap.wait_for("interaction_requested");
    assert_eq!(asked.payload["prompt"], "model?");
    session.reply(contract::commands::Reply {
        request_id: contract::RequestId(asked.payload["request_id"].as_str().unwrap().into()),
        answer: contract::commands::ReplyAnswer::Confirmed { confirmed: true },
    });
    let (looped, outcome) = deadline.recv_or_fail(&done, "answered model call");
    session.looped = Some(looped);
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.kind == "interaction_requested")
            .count(),
        1
    );
    let resolved: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "interaction_resolved")
        .collect();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].payload["declined"], true);
    assert_eq!(resolved[0].payload["by"], "fiber");
    assert_eq!(resolved[1].payload["confirmed"], true);
    assert_eq!(resolved[1].payload["by"], "person");
    assert_eq!(outcomes.lock().unwrap()[0].status, CallStatus::Completed);
}

#[test]
fn inner_control_names_the_session_but_never_loads_a_model_skill_or_starts_a_handoff() {
    let control = contract::events::Control {
        name: Some("inner name".into()),
        skill: Some(contract::events::SkillLoad {
            name: "inner-skill".into(),
            path: "/skill/SKILL.md".into(),
        }),
        handoff: Some("inner handoff".into()),
        ..Default::default()
    };
    let inner = Arc::new(FakeTool::new("inner", move |_, _, _, _| Output {
        control: Some(control.clone()),
        ..text("skill text")
    }));
    let (outer, outcomes) = outer("inner", json!({}));
    let mut session = session(vec![outer, inner]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        action_kinds(&lines, &inner_id(&lines)),
        [
            "tool_call_requested",
            "tool_call_started",
            "session_named",
            "tool_call_completed"
        ]
    );
    let named = lines.iter().find(|l| l.kind == "session_named").unwrap();
    assert_eq!(named.payload["name"], "inner name");
    assert!(!lines.iter().any(|l| l.kind == "handoff_started"));
    assert_eq!(
        outcomes.lock().unwrap()[0]
            .control
            .as_ref()
            .unwrap()
            .skill
            .as_ref()
            .unwrap()
            .name,
        "inner-skill"
    );
    assert!(
        !serde_json::to_string(&session.requests()[1].conversation)
            .unwrap()
            .contains("inner-skill")
    );
}

#[test]
fn an_inner_write_tracks_its_instruction_file_and_appends_the_prune_line_before_completion() {
    let root = fakes::TempDir::new("fiber-inner-instruction");
    let path = root.path().join("notes.md");
    std::fs::write(&path, "1234").unwrap();
    let written = path.clone();
    let mut inner = FakeTool::new("inner", move |_, _, _, _| {
        std::fs::write(&written, "123456").unwrap();
        text("written")
    });
    inner.effects.declared.effects = vec![Effect::Writes];
    inner.effects.declared.paths = Some(vec![path.display().to_string()]);
    let (outer, outcomes) = outer("inner", json!({}));
    let mut session = Session::with_tools_sectioned(
        vec![
            calls_reply("", &[("outer", json!({}))]),
            fakes::Scripted::text("done"),
        ],
        vec![outer, Arc::new(inner)],
        vec![("notes".into(), vec![path], Some(5))],
    );
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let id = inner_id(&lines);
    assert_eq!(
        action_kinds(&lines, &id),
        [
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
            "instruction_file"
        ]
    );
    let instruction = lines
        .iter()
        .find(|l| l.kind == "instruction_file" && l.action_id.as_ref() == Some(&id))
        .unwrap();
    assert_eq!(instruction.payload["reason"], "own_edit");
    assert_eq!(instruction.payload["sent"], "none");
    let results = outcomes.lock().unwrap();
    assert_eq!(
        results[0].content.last(),
        Some(&ContentPart::Text {
            text: "Fiber: these files are 6 bytes, over their budget of 5 bytes. Prune them."
                .into()
        })
    );
}

/// A leaf inner call. It reports when it runs, then whether its cancel
/// arrives within two seconds. It never fails inside its worker, so a missed
/// cancel reports `false` rather than hanging the turn.
fn leaf(
    running: std::sync::mpsc::Sender<()>,
    cancelled: std::sync::mpsc::Sender<bool>,
) -> Arc<FakeTool> {
    Arc::new(FakeTool::new("deep", move |_, cancel, _, _| {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake: Arc<dyn contract::clock::Wake> = Arc::new(CancelWake(tx));
        cancel.subscribe(Arc::downgrade(&wake));
        let _sent = running.send(());
        let seen = cancel.is_cancelled()
            || fakes::Deadline::after(std::time::Duration::from_secs(2))
                .recv(&rx)
                .is_ok();
        let _sent = cancelled.send(seen);
        text("deep done")
    }))
}

/// The anchor of a nested inner call returns while its own inner call is
/// still running, which only the anchor's return can cancel.
#[test]
fn a_nested_inner_call_is_cancelled_when_its_anchor_returns() {
    let (running_tx, running) = std::sync::mpsc::channel();
    let (seen_tx, seen) = std::sync::mpsc::channel();
    let deep = leaf(running_tx, seen_tx);
    let running = Mutex::new(running);
    let middle = Arc::new(FakeTool::new("middle", move |_, _, _, ask| {
        let Some(runner) = ask.inner_calls() else {
            return text("no runner");
        };
        std::thread::spawn(move || runner.run("ext", "deep", json!({})));
        fakes::Deadline::after(std::time::Duration::from_secs(5)).recv_or_fail(
            &running.lock().unwrap(),
            "deep running before middle returns",
        );
        text("middle returned")
    }));
    let outer = Arc::new(FakeTool::new("outer", |_, _, _, ask| {
        let Some(runner) = ask.inner_calls() else {
            return text("no runner");
        };
        match runner.run("ext", "middle", json!({})) {
            Ok(_) => text("outer returned"),
            Err(error) => text(error),
        }
    }));
    let mut session = session(vec![outer, middle, deep]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert!(
        fakes::Deadline::after(std::time::Duration::from_secs(5))
            .recv_or_fail(&seen, "deep cancel report"),
        "the anchor's return cancels the nested call"
    );
}

/// The anchor of a nested inner call is cancelled by its own anchor's
/// return while it is still running, so the nested call is cancelled too.
#[test]
fn a_nested_inner_call_is_cancelled_when_its_anchor_is_cancelled() {
    let deadline = fakes::Deadline::after(std::time::Duration::from_secs(5));
    let (running_tx, _running) = std::sync::mpsc::channel();
    let (seen_tx, seen) = std::sync::mpsc::channel();
    let deep = leaf(running_tx, seen_tx);
    let (middle_tx, middle_running) = std::sync::mpsc::channel();
    let middle = Arc::new(FakeTool::new("middle", move |_, _, _, ask| {
        let Some(runner) = ask.inner_calls() else {
            return text("no runner");
        };
        let _sent = middle_tx.send(());
        match runner.run("ext", "deep", json!({})) {
            Ok(_) => text("middle done"),
            Err(error) => text(error),
        }
    }));
    let middle_running = Mutex::new(middle_running);
    let outer = Arc::new(FakeTool::new("outer", move |_, _, _, ask| {
        let Some(runner) = ask.inner_calls() else {
            return text("no runner");
        };
        std::thread::spawn(move || runner.run("ext", "middle", json!({})));
        deadline.recv_or_fail(
            &middle_running.lock().unwrap(),
            "middle running before outer returns",
        );
        text("outer returned")
    }));
    let mut session = session(vec![outer, middle, deep]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert!(
        fakes::Deadline::after(std::time::Duration::from_secs(5))
            .recv_or_fail(&seen, "deep cancel report"),
        "the cancelled anchor cancels the nested call"
    );
}
