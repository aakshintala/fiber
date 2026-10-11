//! The `ask_user` tool (`docs/tools.md`, "Asking the person"): the model
//! asks whoever drives the session one to four questions.

use contract::ErrorCode;
use contract::emit::Emit;
use contract::events::{Answer, Control, FormAnswer, Interaction};
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Question};
use contract::tool::{Answered, Ask, Asking, Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use crate::tool_util::{failed, no_effects, text_output};

/// The result of a call whose questions went to a program driver
/// (`docs/tools.md`, "When a program drives the session").
const SENT: &str = "The questions went to the driver. The answers arrive as the next prompt.";

/// The result of a form the person declined or a cancel ended
/// (`docs/tools.md`, "The result").
const DECLINED: &str = "declined";

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
        Ok(no_effects())
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

    /// Raises one `form` when a person can answer, and hands the questions
    /// to the driver otherwise (`docs/tools.md`, "Asking the person").
    fn run_asking(
        &self,
        arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        if !ask.answerable() {
            return self.run(arguments, cancel, emit);
        }
        let questions = match questions(arguments) {
            Ok(questions) => questions,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        let answer = ask.ask(Asking {
            interaction: Interaction::Form {
                fields: questions.clone(),
            },
            action_ids: Vec::new(),
            until: None,
            check: None,
            // It lives like a pending approval: the session may exit on it,
            // and resuming runs the call again (`docs/tools.md`, "When a
            // person can answer").
            suspends: true,
        });
        match answer {
            Answered::Reply(Answer::Form { answers, note }) => {
                text_output(answered(&questions, &answers, note.as_deref()))
            }
            Answered::Reply(Answer::Declined { .. }) => text_output(DECLINED.to_owned()),
            // A cancel resolved it; the loop completes the call `cancelled`
            // (`docs/tools.md`, "Cancellation").
            Answered::NoAnswer if cancel.is_cancelled() => text_output(DECLINED.to_owned()),
            // `close`, or nobody left to answer: the turn ends with the
            // questions (`docs/tools.md`, "When a program drives the
            // session").
            Answered::NoAnswer => to_driver(questions),
            Answered::Reply(
                Answer::Confirmed { .. } | Answer::Labels { .. } | Answer::Text { .. },
            ) => failed(
                ErrorCode::ToolError,
                "The answer does not fit the form.".to_owned(),
            ),
        }
    }
}

/// One line per question in field order, then the note (`docs/tools.md`,
/// "The result"). Typed text and the note are JSON strings; a header or
/// label is JSON-escaped without quotes, so each question stays on one line.
fn answered(questions: &[Question], answers: &[FormAnswer], note: Option<&str>) -> String {
    let mut lines: Vec<String> = questions
        .iter()
        .zip(answers)
        .map(|(question, answer)| {
            let said = match answer {
                FormAnswer::Skipped { .. } => "skipped".to_owned(),
                FormAnswer::Answered { labels, text } => {
                    let mut parts: Vec<String> =
                        labels.iter().map(|label| escaped(label)).collect();
                    parts.extend(text.as_deref().map(quoted));
                    parts.join(", ")
                }
            };
            format!("{}: {said}", escaped(&question.header))
        })
        .collect();
    lines.extend(note.map(|note| format!("note: {}", quoted(note))));
    lines.join("\n")
}

/// `text` as a JSON string, quotes included.
fn quoted(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

/// `text` with JSON's escapes but without the quotes around it.
fn escaped(text: &str) -> String {
    let quoted = quoted(text);
    quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(&quoted)
        .to_owned()
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
            skill: None,
        }),
        ..Output::default()
    }
}

#[cfg(test)]
#[path = "ask_user_tests.rs"]
mod tests;
