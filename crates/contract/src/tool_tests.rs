use std::sync::{Arc, Weak};

use serde_json::{Map, Value};

use super::{Answered, Ask, Asking, Cancel, Effects, EffectsError, Output, Settle, Tool};
use crate::ActionId;
use crate::clock::Wake;
use crate::emit::Emit;
use crate::events::Event;
use crate::provider::ToolDefinition;
use crate::shapes::{ContentPart, DeclaredEffects};

/// `Settle` prints opaque and compares by identity, so the loop's logs
/// never carry a queue's contents and two calls' settles never compare
/// equal.
#[test]
fn settle_debug_and_equality_are_by_identity() {
    let first = Settle(Arc::new(Vec::<crate::jobs::Taken>::new));
    let same = first.clone();
    assert_eq!(format!("{:?}", first), "Settle(..)");
    assert_eq!(first, same);
    let other = Settle(Arc::new(Vec::<crate::jobs::Taken>::new));
    assert_ne!(first, other);
}

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

struct EchoTool;

impl Tool for EchoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "echo".into(),
            description: "echo".into(),
            input_schema: Value::Null,
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        Output {
            content: vec![ContentPart::Text { text: "ran".into() }],
            ..Default::default()
        }
    }
}

/// The default `run_asking` runs `run`: a stub whose `run` returns a
/// non-default output sees exactly that output back.
#[test]
fn default_run_asking_forwards_to_run() {
    let tool = EchoTool;
    let arguments = Map::new();
    let cancel = NeverCancelled;
    let emit = NoEmit;
    let ask = NoAsk;
    let expected = tool.run(&arguments, &cancel, &emit);
    assert!(
        !expected.content.is_empty(),
        "the stub's run must return a non-default output"
    );
    assert_eq!(tool.run_asking(&arguments, &cancel, &emit, &ask), expected);
}
