//! The exact prepared [`Effects`] reach the run through outer, inner and
//! capped calls: a call the loop declares confined stays confined, and its
//! log shows `tool_call_started` with `confined: true` before the failed
//! end carrying the sandbox error. The failure result is injected by the
//! test tool, so the loop proves it carries a run's outcome through, not
//! that any sandbox exists here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::{Arc, Mutex, mpsc};

use contract::events::{CallStatus, ToolCallCompleted, ToolCallStarted};
use contract::shapes::{ContentPart, DeclaredEffects, Failure};
use contract::tool::{Ask, Cancel, Effects, EffectsError, Output, Tool};
use contract::{ActionId, ErrorCode, TurnId};
use serde_json::{Map, Value};

use super::before_tool::Prepared;
use super::calls::Decided;
use crate::{Loop, Model, Permissions};

/// The sandbox-apply failure the test tools inject: the loop must log it on
/// the failed end unchanged.
const SANDBOX_MESSAGE: &str = "The sandbox could not be applied: the sandbox is off.";

fn confined_effects() -> Effects {
    Effects {
        declared: DeclaredEffects {
            effects: Vec::new(),
            reversible: true,
            paths: None,
            confined: true,
        },
        subject: Some(String::new()),
        prefix: None,
        always_reviewed: false,
        review_when_confined: false,
        hosts: None,
    }
}

fn failed_output() -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: "nothing ran".into(),
        }],
        error: Some(Failure {
            code: ErrorCode::ToolError,
            message: SANDBOX_MESSAGE.into(),
            retry_after_ms: None,
            provider: None,
        }),
        ..Output::default()
    }
}

/// A tool recording every [`Effects`] its `run_with_effects` receives and
/// returning the injected output. `run` answers with a marker no assertion
/// accepts, so a call that misses `run_with_effects` fails loudly.
struct Recording {
    name: &'static str,
    declare: Effects,
    seen: Mutex<Vec<Effects>>,
    output: Output,
    /// When set, the run first makes this inner call and waits for it.
    inner: Option<&'static str>,
}

impl Recording {
    fn confined(name: &'static str) -> Self {
        Self {
            name,
            declare: confined_effects(),
            seen: Mutex::new(Vec::new()),
            output: failed_output(),
            inner: None,
        }
    }

    fn seen(&self) -> Vec<Effects> {
        self.seen.lock().unwrap().clone()
    }
}

impl Tool for Recording {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: self.name.into(),
            description: self.name.into(),
            input_schema: Value::Object(Map::new()),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(self.declare.clone())
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn contract::emit::Emit,
    ) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "wrong-seam".into(),
            }],
            ..Output::default()
        }
    }

    fn run_with_effects(
        &self,
        arguments: &Map<String, Value>,
        effects: &Effects,
        cancel: &dyn Cancel,
        emit: &dyn contract::emit::Emit,
        ask: &dyn Ask,
    ) -> Output {
        let _ = (arguments, cancel, emit);
        self.seen.lock().unwrap().push(effects.clone());
        if let Some(inner) = self.inner {
            ask.inner_calls()
                .expect("an outer run has an inner slot")
                .run("ext", inner, Value::Object(Map::new()))
                .expect("the inner call completes");
        }
        self.output.clone()
    }
}

/// A loop holding `tools` on a fresh log: the session is `s_test`. The log
/// is returned so the test can watch the lines the run writes.
fn start(tools: Vec<(String, Arc<dyn Tool>)>) -> (Loop, fakes::TempDir, Arc<log::Log>) {
    let home = fakes::TempDir::new("fiber-call-effects");
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log = Arc::new(
        log::Log::create(
            home.path(),
            contract::SessionId("s_test".into()),
            Arc::clone(&clock),
        )
        .unwrap(),
    );
    let (_inbox, rx) = mpsc::channel();
    let looped = Loop::start(
        crate::Session {
            log: Arc::clone(&log),
            provider: Arc::new(fakes::ScriptedProvider::new(Vec::new())),
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
            tools,
            permissions: Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules: Arc::new(fakes::NoRules),
            },
        },
        None,
    )
    .unwrap();
    (looped, home, log)
}

/// Runs one pre-decided call and returns its durable kinds in order.
fn run_decided(
    looped: &mut Loop,
    id: &str,
    tool: Arc<dyn Tool>,
    effects: Effects,
    watcher: &mut log::Watcher,
) -> Vec<(String, contract::Envelope)> {
    let prepared = Prepared {
        tool,
        arguments: Map::new(),
        effects,
        rewritten: None,
    };
    let request = contract::events::ToolCallRequested {
        name: "outer".into(),
        arguments: Value::Object(Map::new()),
        provider_id: None,
        repair: None,
        provider_item: None,
        ran_by: None,
    };
    let decision: Decided = Ok(prepared);
    let turn = TurnId("t_effects".into());
    looped
        .run_batch(
            vec![(ActionId(id.into()), request, Some(decision))],
            &turn,
            None,
            None,
        )
        .unwrap();
    let mut lines = Vec::new();
    while let Some(line) = watcher.try_recv().unwrap() {
        lines.push((line.kind.clone(), line));
    }
    lines
}

fn kinds(lines: &[(String, contract::Envelope)]) -> Vec<&str> {
    lines.iter().map(|(kind, _)| kind.as_str()).collect()
}

fn line_for(
    lines: &[(String, contract::Envelope)],
    kind: &str,
    action: &str,
) -> contract::Envelope {
    lines
        .iter()
        .find(|(line_kind, line)| {
            line_kind == kind && line.action_id.as_ref().is_some_and(|id| id.0 == action)
        })
        .map(|(_, line)| line.clone())
        .unwrap_or_else(|| panic!("a {kind} line for {action}"))
}

fn started_confined(lines: &[(String, contract::Envelope)], action: &str) {
    let line = line_for(lines, "tool_call_started", action);
    let started: ToolCallStarted =
        serde_json::from_value(Value::Object(line.payload.clone())).expect("a started payload");
    assert!(started.declared.confined, "the start declares confined");
}

fn completed_failed(lines: &[(String, contract::Envelope)], action: &str) {
    let line = line_for(lines, "tool_call_completed", action);
    let completed: ToolCallCompleted =
        serde_json::from_value(Value::Object(line.payload.clone())).expect("a completed payload");
    assert_eq!(completed.status, CallStatus::Failed);
    assert_eq!(
        completed.error.as_ref().map(|error| error.message.as_str()),
        Some(SANDBOX_MESSAGE)
    );
}

#[test]
fn an_outer_call_receives_the_exact_prepared_effects() {
    let tool = Arc::new(Recording::confined("outer"));
    let (mut looped, _home, log) = start(vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)]);
    let mut watcher = log.watch();
    let lines = run_decided(
        &mut looped,
        "a_1",
        tool.clone() as Arc<dyn Tool>,
        confined_effects(),
        &mut watcher,
    );
    // No `tool_call_requested`: the call arrived pre-decided, past review.
    assert_eq!(kinds(&lines), ["tool_call_started", "tool_call_completed"]);
    started_confined(&lines, "a_1");
    completed_failed(&lines, "a_1");
    assert_eq!(tool.seen(), vec![confined_effects()]);
}

#[test]
fn an_inner_call_receives_the_exact_prepared_effects() {
    let inner = Arc::new(Recording::confined("inner"));
    let outer = Arc::new(Recording {
        inner: Some("inner"),
        ..Recording::confined("outer")
    });
    let (mut looped, _home, log) = start(vec![
        ("builtin".into(), outer.clone() as Arc<dyn Tool>),
        ("builtin".into(), inner.clone() as Arc<dyn Tool>),
    ]);
    let mut watcher = log.watch();
    let lines = run_decided(
        &mut looped,
        "a_1",
        outer.clone() as Arc<dyn Tool>,
        confined_effects(),
        &mut watcher,
    );
    assert_eq!(
        kinds(&lines),
        [
            "tool_call_started",
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
        ]
    );
    started_confined(&lines, "a_1");
    completed_failed(&lines, "a_1");
    assert_eq!(outer.seen(), vec![confined_effects()]);
    assert_eq!(inner.seen(), vec![confined_effects()]);
    // The inner call carries the same ordered start and failed end under
    // its own action.
    let inner_action = lines
        .iter()
        .find(|(kind, _)| kind == "tool_call_requested")
        .expect("an inner request")
        .1
        .action_id
        .clone()
        .expect("the inner action");
    let inner_lines: Vec<_> = lines
        .iter()
        .filter(|(_, line)| line.action_id.as_ref() == Some(&inner_action))
        .cloned()
        .collect();
    assert_eq!(
        kinds(&inner_lines),
        [
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed"
        ]
    );
    started_confined(&lines, &inner_action.0);
    completed_failed(&lines, &inner_action.0);
}

#[test]
fn a_capped_call_receives_the_exact_prepared_effects() {
    let tool = Arc::new(Recording::confined("outer"));
    let wrapped = super::caps::capped(
        vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)],
        &super::caps::ResultCaps::from([("outer".into(), 64)]),
    );
    let capped = wrapped
        .first()
        .expect("one tool stays registered")
        .1
        .clone();
    let (mut looped, _home, log) = start(wrapped);
    let mut watcher = log.watch();
    let lines = run_decided(&mut looped, "a_1", capped, confined_effects(), &mut watcher);
    assert_eq!(kinds(&lines), ["tool_call_started", "tool_call_completed"]);
    started_confined(&lines, "a_1");
    completed_failed(&lines, "a_1");
    assert_eq!(tool.seen(), vec![confined_effects()]);
}
