//! Tests for `Tool::guidelines`'s default.

use serde_json::{Map, Value};

use super::{Cancel, Effects, EffectsError, Output, Tool};
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
