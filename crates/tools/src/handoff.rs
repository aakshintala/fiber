//! The `handoff` tool (`docs/handoff.md`, "A tool"): the model restarts its
//! context from a note it writes as the argument.

use contract::ErrorCode;
use contract::emit::Emit;
use contract::events::Control;
use contract::provider::ToolDefinition;
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use crate::files::string_argument;
use crate::tool_util::{failed, no_effects};

const MISSING: &str = "Give the handoff note as `note`.";

/// Restarts the model's context from `note`. It declares no effects, so it is
/// never reviewed, and its result has no content: it carries only
/// `control.handoff`, which the loop acts on.
pub struct Handoff;

impl Tool for Handoff {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "handoff".to_owned(),
            description: "Restarts your context from the note you give. Write the note so a \
                 fresh agent can continue the work: refer to specs, issues, commits and files \
                 by path or URL instead of copying them, and leave out secrets. The next \
                 request misses the prompt cache, because it starts from the note."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "note": {
                        "type": "string",
                        "description": "The handoff note: what the next context needs to continue."
                    }
                },
                "required": ["note"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        string_argument(arguments, "note", MISSING).map_err(EffectsError::Arguments)?;
        Ok(no_effects())
    }

    fn run(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        match string_argument(arguments, "note", MISSING) {
            Ok(note) => Output {
                control: Some(Control {
                    handoff: Some(note),
                    questions: None,
                    skill: None,
                }),
                ..Output::default()
            },
            Err(message) => failed(ErrorCode::InvalidArguments, message),
        }
    }

    fn guidelines(&self) -> Option<String> {
        crate::guidelines::of("handoff")
    }
}

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;
