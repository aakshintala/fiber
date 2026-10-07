//! Tests for `Tool::guidelines`'s default.

use serde_json::{Map, Value};

use super::{Answered, Ask, Asking, Cancel, Effects, EffectsError, Output, Tool};
use crate::emit::Emit;
use crate::provider::ToolDefinition;

struct NoGuidelines;

impl Tool for NoGuidelines {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "stub".to_owned(),
            description: "A stub.".to_owned(),
            input_schema: serde_json::json!({}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Arguments("stub".to_owned()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        Output::default()
    }
}

#[test]
fn a_tool_without_guidelines_returns_none() {
    assert_eq!(NoGuidelines.guidelines(), None);
}

#[test]
fn a_tool_that_does_not_cut_its_own_output_leaves_the_cap_to_the_loop() {
    assert!(NoGuidelines.with_cap(100).is_none());
}

#[test]
fn an_effects_error_reads_as_its_message() {
    assert_eq!(
        EffectsError::Arguments("no such path".into()).to_string(),
        "no such path"
    );
    assert_eq!(
        EffectsError::Tool("lua: boom".into()).to_string(),
        "lua: boom"
    );
}

/// A tool that implements only `run`, returning its arguments as details.
struct Echo;

impl Tool for Echo {
    fn definition(&self) -> ToolDefinition {
        NoGuidelines.definition()
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Arguments("echo".to_owned()))
    }

    fn run(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        Output {
            details: Some(Value::Object(arguments.clone())),
            ..Output::default()
        }
    }
}

/// An asker no test should reach.
struct Unasked;

impl Ask for Unasked {
    fn action(&self) -> crate::ActionId {
        unreachable!("the default run_asking never reads the asker")
    }

    fn ask(&self, _asking: Asking) -> Answered {
        unreachable!("the default run_asking never asks")
    }
}

struct NeverCancelled;

impl Cancel for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn crate::clock::Wake>) {}
}

struct Silent;

impl Emit for Silent {
    fn emit(&self, _event: &crate::events::Event) {}
}

#[test]
fn a_tool_that_never_asks_runs_through_run_asking_unchanged() {
    let arguments = serde_json::json!({"path": "a.txt"})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let asked = Echo.run_asking(&arguments, &NeverCancelled, &Silent, &Unasked);
    assert_eq!(asked, Echo.run(&arguments, &NeverCancelled, &Silent));
    assert_eq!(asked.details, Some(Value::Object(arguments)));
}
