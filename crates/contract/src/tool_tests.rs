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
            hosts: None,
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

/// The default `guidelines` is `None`: a tool that declares none offers
/// no system-prompt lines.
#[test]
fn default_guidelines_is_none() {
    assert_eq!(EchoTool.guidelines(), None);
}

/// The default `review_note` is `None`: a tool with no per-call note adds
/// no line to the reviewer item.
#[test]
fn default_review_note_is_none() {
    assert_eq!(EchoTool.review_note(&Map::new()), None);
}

/// `Display` writes the held message for both variants.
#[test]
fn effects_error_display_writes_the_held_message() {
    assert_eq!(
        format!("{}", EffectsError::Arguments("bad path".into())),
        "bad path"
    );
    assert_eq!(format!("{}", EffectsError::Tool("boom".into())), "boom");
}
