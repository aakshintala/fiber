//! The `name_session` tool (`docs/tools.md`, "Naming the session"): the model
//! names the session, so the session list and the terminal show what the
//! work is about.

use contract::emit::Emit;
use contract::events::Control;
use contract::provider::ToolDefinition;
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::tool_util::{errored, no_effects};

/// The call's arguments, checked against the schema before the call runs.
#[derive(Debug, Deserialize)]
struct Args {
    name: String,
}

/// Names the session. It declares no effects, so it is never reviewed,
/// and its result has no content: it carries only `control.name`, which
/// the loop writes as `session_named`.
pub struct NameSession;

impl Tool for NameSession {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "name_session".to_owned(),
            description: "Names this session, so the session list and the terminal show what \
                 the work is about. Name the session after the first prompt, and rename it \
                 when the work's topic changes, a handoff included."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 60,
                        "description": "The session's name, 1 to 60 characters: a few words saying what the work is about."
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let _args: Args =
            crate::tool_util::arguments(arguments).map_err(EffectsError::Arguments)?;
        Ok(no_effects())
    }

    fn run(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        match crate::tool_util::parsed::<Args>(arguments) {
            Ok(Args { name }) => Output {
                control: Some(Control {
                    handoff: None,
                    questions: None,
                    skill: None,
                    name: Some(name),
                }),
                ..Output::default()
            },
            Err(error) => errored(error),
        }
    }
}

#[cfg(test)]
#[path = "name_session_tests.rs"]
mod tests;
