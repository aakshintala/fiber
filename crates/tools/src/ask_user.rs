//! The `ask_user` tool (`docs/tools.md`, "Asking the person"): the model
//! asks whoever drives the session one to four questions.

use contract::ErrorCode;
use contract::emit::Emit;
use contract::events::Control;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Question};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use crate::files::failed;

/// The result of a call whose questions went to a program driver
/// (`docs/tools.md`, "When a program drives the session").
const SENT: &str = "The questions went to the driver. The answers arrive as the next prompt.";

/// Asks the driver questions. It declares no effects, so it is never
/// reviewed. Its limits are in the schema, which the loop checks before the
/// call runs, so the tool is sent with `strict: false`.
pub struct AskUser;

impl Tool for AskUser {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "ask_user".to_owned(),
            description: "Asks whoever drives this session one to four questions. Ask with \
                 this tool rather than listing choices in your reply. Put the option you \
                 recommend first, with \"(Recommended)\" at the end of its label. Leave out \
                 options for a free-text question. The answers come back as this call's \
                 result, or as the next prompt when a program drives the session."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 4,
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": {
                                    "type": "string",
                                    "description": "The full question, with its context."
                                },
                                "header": {
                                    "type": "string",
                                    "maxLength": 12,
                                    "description": "A short label for the question."
                                },
                                "options": {
                                    "type": "array",
                                    "minItems": 2,
                                    "maxItems": 4,
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": {"type": "string"},
                                            "description": {"type": "string"}
                                        },
                                        "required": ["label"],
                                        "additionalProperties": false
                                    }
                                },
                                "multiSelect": {
                                    "type": "boolean",
                                    "description": "Whether several options may be chosen."
                                }
                            },
                            "required": ["question", "header"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["questions"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: Vec::new(),
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
        })
    }

    fn run(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        match questions(arguments) {
            Ok(questions) => to_driver(questions),
            Err(message) => failed(ErrorCode::InvalidArguments, message),
        }
    }
}

/// The call's questions, read from the arguments the loop already checked.
fn questions(arguments: &Map<String, Value>) -> Result<Vec<Question>, String> {
    let value = arguments.get("questions").cloned().unwrap_or(Value::Null);
    serde_json::from_value(value).map_err(|e| format!("`questions` cannot be read: {e}"))
}

/// The result that hands the questions to a program driver: the loop ends
/// the turn with them (`docs/tools.md`, "What a result carries").
fn to_driver(questions: Vec<Question>) -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: SENT.to_owned(),
        }],
        control: Some(Control {
            handoff: None,
            questions: Some(questions),
        }),
        ..Output::default()
    }
}

#[cfg(test)]
#[path = "ask_user_tests.rs"]
mod tests;
