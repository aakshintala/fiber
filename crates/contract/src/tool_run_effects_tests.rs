//! [`Tool::run_with_effects`]: the default forwards to [`Tool::run_asking`]
//! unchanged, so a tool that needs nothing beyond its arguments keeps its
//! behavior when the loop passes prepared effects.

use std::sync::Weak;

use serde_json::{Map, Value};

use super::{Answered, Ask, Asking, Cancel, Effects, EffectsError, Output, Tool};
use crate::ActionId;
use crate::clock::Wake;
use crate::emit::Emit;
use crate::events::Event;
use crate::provider::ToolDefinition;
use crate::shapes::{ContentPart, DeclaredEffects};

struct NeverCancelled;

impl Cancel for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

struct NoEmit;

impl Emit for NoEmit {
    fn emit(&self, _event: &Event) {}
}

struct NoAsk;

impl Ask for NoAsk {
    fn action(&self) -> ActionId {
        ActionId("a_1".into())
    }

    fn answerable(&self) -> bool {
        false
    }

    fn ask(&self, _asking: Asking) -> Answered {
        Answered::NoAnswer
    }
}

fn effects() -> Effects {
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

/// A tool that answers only through `run_asking`: the default
/// `run_with_effects` must reach it.
struct AskingOnly;

impl Tool for AskingOnly {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "asking-only".into(),
            description: "asking-only".into(),
            input_schema: Value::Null,
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(effects())
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        unreachable!("run_with_effects forwards to run_asking, never run")
    }

    fn run_asking(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
        _ask: &dyn Ask,
    ) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "asked".into(),
            }],
            ..Output::default()
        }
    }
}

#[test]
fn the_default_run_with_effects_forwards_to_run_asking() {
    let tool = AskingOnly;
    let output = tool.run_with_effects(&Map::new(), &effects(), &NeverCancelled, &NoEmit, &NoAsk);
    assert_eq!(
        output.content,
        vec![ContentPart::Text {
            text: "asked".into()
        }]
    );
}
