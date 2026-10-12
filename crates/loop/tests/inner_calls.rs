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
