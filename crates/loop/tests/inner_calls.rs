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
use fakes::MUST_SUCCEED_WITHIN;
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
                    confined: false,
                },
                subject: Some("x".into()),
                prefix: None,
                always_reviewed: false,
                review_when_confined: false,
                hosts: None,
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
        !serde_json::to_string(&requests[1].conversation[..])
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

#[test]
fn an_inner_call_matching_one_other_tool_gets_no_suggestion() {
    // An extension's inner call names `shell` with `read`'s arguments, and
    // `read` is the one other tool whose schema fits them. The failure
    // still keeps only its field lines: the suggestion answers the model,
    // and an inner call's result goes to the extension instead
    // (`docs/extensions.md`, "When an extension calls a tool").
    let mut shell = FakeTool::new("shell", |_, _, _, _| text("must not run"));
    shell.schema = json!({
        "type": "object",
        "properties": {"command": {"type": "string"}, "limit": {"type": "integer"}},
        "required": ["command"],
        "additionalProperties": false
    });
    let mut read = FakeTool::new("read", |_, _, _, _| text("must not run"));
    read.schema = json!({
        "type": "object",
        "properties": {
            "path": {"type": "string"},
            "offset": {"type": "integer"},
            "limit": {"type": "integer"}
        },
        "required": ["path"],
        "additionalProperties": false
    });
    let arguments = json!({"path": "x.rs", "offset": 1, "limit": 20});
    let (outer, outcomes) = outer("shell", arguments);
    let shell = Arc::new(shell);
    let read = Arc::new(read);
    let mut session = session(vec![outer, shell.clone(), read.clone()]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let done = lines
        .iter()
        .find(|l| {
            l.kind == "tool_call_completed" && l.action_id.as_ref() == Some(&inner_id(&lines))
        })
        .unwrap();
    // One line per bad field, in no promised order, and no suggestion line
    // on an inner call (`docs/tools.md`, "Before a call runs").
    let text = support::text_first(done);
    for line in [
        "`/command`: missing",
        "`/offset`: not allowed",
        "`/path`: not allowed",
    ] {
        assert!(text.contains(line), "missing field line {line}: {text}");
    }
    assert!(
        !text.contains("These arguments fit"),
        "unexpected suggestion: {text}"
    );
    let outcomes = outcomes.lock().unwrap();
    // The outer tool makes one inner call (the `outer` helper above), so the
    // extension sees one outcome.
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, CallStatus::Failed);
    assert!(shell.ran.lock().unwrap().is_empty());
    assert!(read.ran.lock().unwrap().is_empty());
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
    fn before_handoff(
        &self,
        _handoff: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        fakes::no_note()
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
            fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "inner cancellation");
        }
        assert!(cancel.is_cancelled());
        cancelled.send(()).unwrap();
        if let Some(release) = release.lock().unwrap().as_ref() {
            let _released = release.recv();
        }
        text("stopped")
    }))
}

struct RequestDrop(Option<std::sync::mpsc::Sender<()>>);

impl Drop for RequestDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _sent = sender.send(());
        }
    }
}

struct CommandRequest {
    request: contract::inbox::InnerCallRequest,
    answer: std::sync::mpsc::Receiver<Box<ToolCallCompleted>>,
    taken: std::sync::mpsc::Receiver<()>,
    dropped: std::sync::mpsc::Receiver<()>,
    cancel: Arc<fakes::CancelToken>,
}

fn command_request(name: &str, arguments: Value) -> CommandRequest {
    let cancel = Arc::new(fakes::CancelToken::new());
    let (answered, answer) = std::sync::mpsc::channel();
    let (taking, taken) = std::sync::mpsc::channel();
    let (dropping, dropped) = std::sync::mpsc::channel();
    let mut guard = RequestDrop(Some(dropping));
    CommandRequest {
        request: contract::inbox::InnerCallRequest {
            extension: "ext".into(),
            command: contract::CommandId("c_1".into()),
            name: name.into(),
            arguments,
            cancel: cancel.clone(),
            taken: Box::new(move || {
                let _sent = taking.send(());
            }),
            answer: Box::new(move |completed| {
                drop(guard.0.take());
                let _sent = answered.send(completed);
            }),
        },
        answer,
        taken,
        dropped,
        cancel,
    }
}

fn command_lines(session: &mut Session) -> Vec<Envelope> {
    session.events_until("command completion", |line| {
        line.kind == "tool_call_completed"
    })
}

fn close_idle(
    session: &mut Session,
    done: &std::sync::mpsc::Receiver<(r#loop::Loop, Option<TurnOutcome>)>,
) {
    session
        .inbox
        .send(contract::inbox::Delivery::Close(support::ignore()))
        .unwrap();
    let (looped, outcome) =
        fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(done, "idle loop closed");
    session.looped = Some(looped);
    assert_eq!(outcome, None);
}

fn assert_command_anchor(lines: &[Envelope]) {
    let id = inner_id(lines);
    let requested = lines
        .iter()
        .find(|line| line.action_id.as_ref() == Some(&id))
        .unwrap();
    assert_eq!(
        requested.payload["ran_by"],
        json!({"extension":"ext", "command_id":"c_1"})
    );
    let chain: Vec<_> = lines
        .iter()
        .filter(|line| line.action_id.as_ref() == Some(&id))
        .collect();
    assert!(chain.iter().all(|line| line.turn_id.is_none()));
    assert_eq!(
        chain
            .iter()
            .filter(|line| line.payload.contains_key("ran_by"))
            .count(),
        1
    );
}

#[test]
fn an_idle_command_runs_turnless_and_answers_with_the_logged_completion() {
    let tool = Arc::new(FakeTool::new("read", |_, _, _, _| text("answer")));
    let mut session = Session::with_tools(vec![], None, vec![tool]);
    let mut request = command_request("read", json!({}));
    let dir = session.dir.clone();
    let taken = request.request.taken;
    request.request.taken = Box::new(move || {
        assert!(
            !log::read(&dir)
                .unwrap()
                .iter()
                .any(|line| line.kind == "tool_call_requested")
        );
        taken();
    });
    session
        .inbox
        .send(contract::inbox::Delivery::InnerCall(request.request))
        .unwrap();
    let (done, _) = start_turn(&mut session);
    fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&request.taken, "request taken");
    let completed = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&request.answer, "written completion");
    let lines = command_lines(&mut session);
    assert_eq!(
        support::kinds(&lines),
        [
            "session_started",
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed"
        ]
    );
    assert_command_anchor(&lines);
    assert_eq!(
        serde_json::to_value(&*completed).unwrap(),
        Value::Object(lines.last().unwrap().payload.clone())
    );
    assert_eq!(completed.content, text("answer").content);
    assert!(session.requests().is_empty());
    close_idle(&mut session, &done);
}

#[test]
fn an_ended_command_or_a_shutdown_drops_the_request_without_taking_or_logging_it() {
    for shutdown in [false, true] {
        let mut session = Session::with_tools(vec![], None, vec![]);
        let request = command_request("read", json!({}));
        if shutdown {
            session.cancel.shutdown(143);
        } else {
            request.cancel.cancel();
        }
        session
            .inbox
            .send(contract::inbox::Delivery::InnerCall(request.request))
            .unwrap();
        let (done, _) = start_turn(&mut session);
        // A shutdown stops before draining the inbox; dropping the returned
        // loop drops its unanswered deliveries too.
        if shutdown {
            let (looped, outcome) =
                fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&done, "shutdown loop");
            assert_eq!(outcome, None);
            drop(looped);
        } else {
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&request.dropped, "ended request dropped");
            close_idle(&mut session, &done);
        }
        assert!(request.answer.try_recv().is_err());
        assert!(request.taken.try_recv().is_err());
        let lines = log::read(&session.dir).unwrap();
        assert_eq!(support::kinds(&lines), ["session_started",]);
    }
}

#[test]
fn a_command_end_cancels_its_running_inner_call_and_shutdown_runs_no_after_hook() {
    for shutdown in [false, true] {
        let (started, running) = std::sync::mpsc::channel();
        let (stopped, _cancelled) = std::sync::mpsc::channel();
        let tool = waiting_inner(started, stopped, None);
        let mut session = Session::with_tools(vec![], None, vec![tool]);
        let hooks = Arc::new(FakeHooks::default());
        session.looped = Some(session.looped.take().unwrap().hooks(hooks.clone()));
        let request = command_request("inner", json!({}));
        session
            .inbox
            .send(contract::inbox::Delivery::InnerCall(request.request))
            .unwrap();
        let (done, _) = start_turn(&mut session);
        fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&running, "inner tool running");
        if shutdown {
            session.cancel.shutdown(143);
        } else {
            request.cancel.cancel();
        }
        let completed = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&request.answer, "cancelled inner completion");
        assert_eq!(completed.status, CallStatus::Cancelled);
        let lines = command_lines(&mut session);
        assert_command_anchor(&lines);
        assert_eq!(
            support::kinds(&lines),
            [
                "session_started",
                "tool_call_requested",
                "tool_call_started",
                "tool_call_completed"
            ]
        );
        if shutdown {
            assert!(completed.content.is_empty());
            assert_eq!(*hooks.seen.lock().unwrap(), ["before inner", "check inner"]);
            let (looped, outcome) = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&done, "shutdown loop stopped");
            session.looped = Some(looped);
            assert_eq!(outcome, None);
        } else {
            assert_eq!(
                *hooks.seen.lock().unwrap(),
                ["before inner", "check inner", "after inner"]
            );
            close_idle(&mut session, &done);
        }
    }
}

#[test]
fn a_command_chain_has_eight_inner_levels_and_turnless_outer_action_anchors() {
    fn recursive(name: &'static str, other: &'static str) -> Arc<FakeTool> {
        Arc::new(FakeTool::new(name, move |args, _, _, ask| {
            let left = args["left"].as_u64().unwrap();
            if left == 0 {
                return text("end");
            }
            match ask
                .inner_calls()
                .unwrap()
                .run("ext", other, json!({"left":left-1}))
            {
                Ok(done) => Output {
                    content: done.content,
                    ..Default::default()
                },
                Err(error) => text(error),
            }
        }))
    }
    for (left, count, result) in [
        (6, 7, "end"),
        (7, 8, "end"),
        (8, 8, "host.tool: inner-call depth exceeds 8"),
    ] {
        let mut session =
            Session::with_tools(vec![], None, vec![recursive("a", "b"), recursive("b", "a")]);
        let request = command_request("a", json!({"left":left}));
        session
            .inbox
            .send(contract::inbox::Delivery::InnerCall(request.request))
            .unwrap();
        let (done, _) = start_turn(&mut session);
        let completed = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&request.answer, "command chain completion");
        assert_eq!(completed.status, CallStatus::Completed);
        assert_eq!(completed.content, text(result).content);
        close_idle(&mut session, &done);
        let lines = log::read(&session.dir).unwrap();
        let requested: Vec<_> = lines
            .iter()
            .filter(|line| line.kind == "tool_call_requested")
            .collect();
        assert_eq!(requested.len(), count);
        assert_command_anchor(&lines);
        for pair in requested.windows(2) {
            assert_eq!(
                pair[1].payload["ran_by"],
                json!({"extension":"ext","outer_action_id":pair[0].action_id})
            );
        }
        let completions: Vec<_> = lines
            .iter()
            .filter(|line| line.kind == "tool_call_completed")
            .map(|line| &line.action_id)
            .collect();
        assert_eq!(
            completions,
            requested
                .iter()
                .rev()
                .map(|line| &line.action_id)
                .collect::<Vec<_>>()
        );
        assert!(lines.iter().all(|line| line.turn_id.is_none()));
        let mut expected = vec!["session_started"];
        for _ in 0..count {
            expected.extend(["tool_call_requested", "tool_call_started"]);
        }
        expected.extend(std::iter::repeat_n("tool_call_completed", count));
        assert_eq!(support::kinds(&lines), expected);
    }
}

#[test]
fn an_ask_in_a_command_chain_is_declined_even_in_an_answerable_session() {
    let tool = Arc::new(FakeTool::new("inner", |_, _, _, ask| {
        assert!(!ask.answerable());
        assert_eq!(
            ask.ask(asking("inner?")),
            contract::tool::Answered::NoAnswer
        );
        text("declined")
    }));
    let mut session = Session::with_tools(vec![], None, vec![tool])
        .answerable(true)
        .inbox_woken();
    let request = command_request("inner", json!({}));
    session
        .inbox
        .send(contract::inbox::Delivery::InnerCall(request.request))
        .unwrap();
    let (done, _) = start_turn(&mut session);
    let completed = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&request.answer, "declined command ask");
    assert_eq!(completed.content, text("declined").content);
    let lines = command_lines(&mut session);
    assert_eq!(
        support::kinds(&lines),
        [
            "session_started",
            "tool_call_requested",
            "tool_call_started",
            "interaction_resolved",
            "tool_call_completed"
        ]
    );
    assert_eq!(lines[3].payload["declined"], true);
    assert!(lines.iter().all(|line| line.turn_id.is_none()));
    close_idle(&mut session, &done);
}

#[derive(Clone, Copy)]
enum StepHold {
    Approval,
    Tool,
    Interaction,
}

fn command_at_step_boundary(hold: StepHold, cancel: bool) {
    let (started, running) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let held = Mutex::new(held);
    let outer = Arc::new(FakeTool::new("outer", move |_, _, _, ask| {
        match hold {
            StepHold::Tool => {
                started.send(()).unwrap();
                let _released = held.lock().unwrap().recv();
            }
            StepHold::Interaction => {
                assert_eq!(
                    ask.ask(asking("model?")),
                    contract::tool::Answered::Reply(contract::events::Answer::Confirmed {
                        confirmed: true
                    })
                );
            }
            StepHold::Approval => {}
        }
        text("model result")
    }));
    let (inner_started, inner_running) = std::sync::mpsc::channel();
    let (stopped, _cancelled) = std::sync::mpsc::channel();
    let inner: Arc<dyn Tool> = if cancel {
        waiting_inner(inner_started, stopped, None)
    } else {
        Arc::new(FakeTool::new("inner", |_, _, _, _| text("inner result")))
    };
    let mut session = session(vec![outer, inner]).answerable(true).inbox_woken();
    if let StepHold::Approval = hold {
        session.rules.set(contract::rules::StandingRules {
            global: vec![],
            project: vec![contract::rules::Rule {
                decision: contract::rules::RuleDecision::Ask,
                tool: "outer".into(),
                prefix: "x".into(),
                added: None,
                session_id: None,
            }],
        });
    }
    let tap = support::Tap::new(&session.log);
    let (done, _) = start_turn(&mut session);
    let pending = match hold {
        StepHold::Approval => Some(tap.wait_for("permission_requested")),
        StepHold::Tool => {
            fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&running, "step tool held");
            None
        }
        StepHold::Interaction => Some(tap.wait_for("interaction_requested")),
    };
    let request = command_request("inner", json!({}));
    session
        .inbox
        .send(contract::inbox::Delivery::InnerCall(request.request))
        .unwrap();
    match hold {
        StepHold::Tool => release.send(()).unwrap(),
        StepHold::Approval | StepHold::Interaction => {
            let pending = pending.unwrap();
            let answer = match hold {
                StepHold::Approval => support::allow(),
                StepHold::Interaction => {
                    contract::commands::ReplyAnswer::Confirmed { confirmed: true }
                }
                StepHold::Tool => unreachable!(),
            };
            session
                .inbox
                .send(support::reply_delivery(
                    contract::RequestId(pending.payload["request_id"].as_str().unwrap().into()),
                    answer,
                ))
                .unwrap();
        }
    }
    if cancel {
        fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&inner_running, "command tool at step boundary");
        assert!(session.cancel.cancel());
    }
    let completed = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&request.answer, "boundary command completion");
    assert_eq!(
        completed.status,
        if cancel {
            CallStatus::Cancelled
        } else {
            CallStatus::Completed
        }
    );
    let (looped, outcome) =
        fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&done, "step ended");
    session.looped = Some(looped);
    assert_eq!(
        outcome,
        Some(if cancel {
            TurnOutcome::Interrupted
        } else {
            TurnOutcome::Completed
        })
    );
    let lines = session.lines();
    assert_command_anchor(&lines);
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
    ];
    if let StepHold::Approval = hold {
        expected.extend(["permission_requested", "permission_resolved"]);
    }
    expected.push("tool_call_started");
    if let StepHold::Interaction = hold {
        expected.extend(["interaction_requested", "interaction_resolved"]);
    }
    expected.extend([
        "tool_call_completed",
        "step_started",
        "tool_call_requested",
        "tool_call_started",
        "tool_call_completed",
    ]);
    if !cancel {
        expected.extend_from_slice(support::REPLY);
    }
    expected.push("turn_completed");
    assert_eq!(support::kinds(&lines), expected);
}

#[test]
fn a_command_queued_on_an_approval_wait_runs_at_the_next_step_boundary() {
    command_at_step_boundary(StepHold::Approval, false);
}

#[test]
fn a_command_queued_mid_step_follows_the_steps_last_tool_result() {
    command_at_step_boundary(StepHold::Tool, false);
}

#[test]
fn a_command_queued_during_an_interaction_waits_for_the_reply_and_step_boundary() {
    command_at_step_boundary(StepHold::Interaction, false);
}

#[test]
fn a_turn_cancel_stops_a_command_inner_call_started_by_the_boundary_drain() {
    command_at_step_boundary(StepHold::Approval, true);
}

#[test]
fn command_inner_calls_keep_credential_and_standing_denies_but_skip_standing_asks() {
    use contract::rules::{Rule, RuleDecision, StandingRules};
    for (credentials, decision, expected) in [
        (true, RuleDecision::Ask, CallStatus::Denied),
        (false, RuleDecision::Deny, CallStatus::Denied),
        (false, RuleDecision::Ask, CallStatus::Completed),
    ] {
        let tool = Arc::new(FakeTool::new("inner", |_, _, _, _| text("allowed")));
        let mut session = Session::with_tools(vec![], None, vec![tool.clone()]);
        session.rules.set(StandingRules {
            global: vec![],
            project: vec![Rule {
                decision,
                tool: "inner".into(),
                prefix: "x".into(),
                added: None,
                session_id: None,
            }],
        });
        let path = if credentials {
            session.credentials.join("secret")
        } else {
            session.workspace.join("public")
        };
        let request = command_request("inner", json!({"path":path}));
        session
            .inbox
            .send(contract::inbox::Delivery::InnerCall(request.request))
            .unwrap();
        let (done, _) = start_turn(&mut session);
        let completed = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&request.answer, "command permission outcome");
        assert_eq!(completed.status, expected);
        let lines = command_lines(&mut session);
        assert_command_anchor(&lines);
        let mut kinds = vec!["session_started", "tool_call_requested"];
        if expected == CallStatus::Denied {
            kinds.push("permission_resolved");
            assert_eq!(
                lines[2].payload["decided_by"],
                if credentials {
                    "credential_deny"
                } else {
                    "standing_rule"
                }
            );
            assert!(tool.ran.lock().unwrap().is_empty());
        } else {
            kinds.push("tool_call_started");
            assert_eq!(tool.ran.lock().unwrap().len(), 1);
        }
        kinds.push("tool_call_completed");
        assert_eq!(support::kinds(&lines), kinds);
        assert!(lines.iter().all(|line| line.turn_id.is_none()));
        close_idle(&mut session, &done);
    }
}

#[test]
fn a_command_names_the_session_only_when_its_final_completion_succeeds() {
    use contract::hook::{AfterToolOutcome, HookFailure};
    for (after, status, names) in [
        (
            AfterToolOutcome::Failed(HookFailure {
                extension: "x".into(),
                message: "m".into(),
            }),
            CallStatus::Failed,
            0,
        ),
        (AfterToolOutcome::Unchanged, CallStatus::Completed, 1),
    ] {
        let tool = Arc::new(FakeTool::new("inner", |_, _, _, _| Output {
            control: Some(contract::events::Control {
                name: Some("named".into()),
                ..Default::default()
            }),
            ..text("raw secret")
        }));
        let mut session = Session::with_tools(vec![], None, vec![tool]);
        let hooks = Arc::new(FakeHooks {
            after,
            ..Default::default()
        });
        session.looped = Some(session.looped.take().unwrap().hooks(hooks));
        let request = command_request("inner", json!({}));
        session
            .inbox
            .send(contract::inbox::Delivery::InnerCall(request.request))
            .unwrap();
        let (done, _) = start_turn(&mut session);
        let completed = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&request.answer, "command completion");
        assert_eq!(completed.status, status);
        let lines = session.events_until("command completion", |line| {
            line.kind == "tool_call_completed"
        });
        let id = inner_id(&lines);
        let named: Vec<_> = lines
            .iter()
            .filter(|line| line.kind == "session_named")
            .collect();
        assert_eq!(named.len(), names);
        if let Some(named) = named.first() {
            assert_eq!(named.payload["name"], "named");
            assert_eq!(named.action_id.as_ref(), Some(&id));
            assert!(named.turn_id.is_none());
        }
        if status == CallStatus::Failed {
            assert_eq!(
                completed.error.as_ref().unwrap().code,
                contract::ErrorCode::HookFailed
            );
        }
        let mut expected = vec![
            "session_started",
            "tool_call_requested",
            "tool_call_started",
        ];
        expected.extend(std::iter::repeat_n("session_named", names));
        expected.push("tool_call_completed");
        assert_eq!(support::kinds(&lines), expected);
        assert!(lines.iter().all(|line| line.turn_id.is_none()));
        session
            .inbox
            .send(contract::inbox::Delivery::Close(support::ignore()))
            .unwrap();
        let (looped, outcome) =
            fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&done, "idle loop closed");
        session.looped = Some(looped);
        assert_eq!(outcome, None);
    }
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
        let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
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
    let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
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
            .await_thread_parked(id, None, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
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
    let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
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
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "inner delta waits on its clock interval"
    );
    session.clock.advance_when_parked(
        Some(session.clock.now() + interval),
        Some(thread),
        interval,
        &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
    );
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
    let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
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
        !serde_json::to_string(&session.requests()[1].conversation[..])
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
/// arrives within the must-succeed bound. It never fails inside its worker, so a missed
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
            || fakes::Deadline::after(MUST_SUCCEED_WITHIN)
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
        fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(
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
        fakes::Deadline::after(MUST_SUCCEED_WITHIN * 2).recv_or_fail(&seen, "deep cancel report"),
        "the anchor's return cancels the nested call"
    );
}

/// The anchor of a nested inner call is cancelled by its own anchor's
/// return while it is still running, so the nested call is cancelled too.
#[test]
fn a_nested_inner_call_is_cancelled_when_its_anchor_is_cancelled() {
    let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
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
        fakes::Deadline::after(MUST_SUCCEED_WITHIN * 2).recv_or_fail(&seen, "deep cancel report"),
        "the cancelled anchor cancels the nested call"
    );
}

#[test]
fn an_inner_call_settle_is_written_at_the_inner_finish_under_its_action() {
    let job = "j_5555555555555555";
    let runs = Arc::new(Mutex::new(0usize));
    let settle_runs = Arc::clone(&runs);
    let inner = Arc::new(FakeTool::new("inner", move |_, _, _, _| {
        let taken = contract::jobs::Taken {
            record: contract::jobs::JobRecord::Completed(contract::events::JobCompleted {
                job_id: contract::JobId(job.into()),
                status: contract::events::Outcome::Completed,
                error: None,
                process: None,
                output_tail: None,

                notice: None,
                changed_by: None,
            }),
            action: None,
        };
        let runs = Arc::clone(&settle_runs);
        Output {
            content: vec![contract::shapes::ContentPart::Text {
                text: "inner answer".into(),
            }],
            settle: Some(contract::tool::Settle(Arc::new(move || {
                *runs.lock().unwrap() += 1;
                vec![taken.clone()]
            }))),
            ..Default::default()
        }
    }));
    let (outer, _) = outer("inner", json!({}));
    let mut session = session(vec![outer, inner]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(*runs.lock().unwrap(), 1, "the settle ran once");
    let lines = session.lines();
    let id = inner_id(&lines);
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
        "tool_call_requested",
        "tool_call_started",
        "job_completed",
        "tool_call_completed",
    ];
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
    assert_eq!(support::kinds(&lines), expected);
    assert_eq!(
        action_kinds(&lines, &id),
        [
            "tool_call_requested",
            "tool_call_started",
            "job_completed",
            "tool_call_completed"
        ]
    );
    let completed = lines
        .iter()
        .find(|line| line.kind == "job_completed")
        .expect("a settled job_completed");
    assert_eq!(completed.payload["job_id"], job);
    assert_eq!(completed.action_id.as_ref(), Some(&id));
}

/// An inner call of an always-reviewed tool carries the key on its start,
/// while the outer call's start omits it.
#[test]
fn an_inner_calls_start_carries_always_reviewed() {
    let (outer, _) = outer("inner", json!({}));
    let mut inner_tool = FakeTool::new("inner", |_, _, _, _| text("inner answer"));
    inner_tool.effects.always_reviewed = true;
    let mut session = session(vec![outer, Arc::new(inner_tool)]);
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_turn_events(
        &lines,
        &[
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
        ],
    );
    let id = inner_id(&lines);
    let started = lines
        .iter()
        .find(|l| l.action_id.as_ref() == Some(&id) && l.kind == "tool_call_started")
        .unwrap();
    assert_eq!(started.payload["always_reviewed"], true);
    let outer_started = lines
        .iter()
        .find(|l| l.kind == "tool_call_started" && l.action_id.as_ref() != Some(&id))
        .unwrap();
    assert_eq!(outer_started.payload.get("always_reviewed"), None);
}
