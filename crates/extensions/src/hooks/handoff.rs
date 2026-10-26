//! The `before_handoff` chain (`docs/extensions.md`, "The hook points",
//! "When several hooks share a point" and "When a hook fails"): what each
//! hook is shown, and what its answer decides.

use contract::ErrorCode;
use contract::events::Notice;
use contract::hook::{BeforeHandoff, BeforeHandoffAnswer, BeforeHandoffOutcome, HookFailure};
use contract::provider::Input;
use serde_json::{Map, Value};

use super::chain::BEFORE_HANDOFF;
use super::{SessionExtensions, kind, notice};
use crate::Error;

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;

/// Runs every `before_handoff` hook on the handoff, in order: each is shown
/// the note the hook before it returned, and the last note wins. A `nil`
/// return, a note that is empty after trimming, and a `non-blocking`
/// failure leave the note as it stood; a `blocking` failure drops it and
/// ends the chain. A cancel that landed between two hooks runs no later
/// hook, and the answer holds no note.
pub(super) fn before_handoff(
    session: &SessionExtensions,
    handoff: &BeforeHandoff<'_>,
) -> BeforeHandoffAnswer {
    let chain = &session.lifecycle.before_handoff;
    if chain.is_empty() {
        return BeforeHandoffAnswer {
            outcome: BeforeHandoffOutcome::NoNote,
            notices: Vec::new(),
        };
    }
    let mut note: Option<(String, String)> = None;
    let mut pending = Vec::new();
    let mut notices = Vec::new();
    let messages = render(handoff.conversation);
    for (at, entry) in chain.iter().enumerate() {
        if at > 0 && handoff.cancel.is_cancelled() {
            return BeforeHandoffAnswer {
                outcome: BeforeHandoffOutcome::NoNote,
                notices,
            };
        }
        let Some(name) = session.runtime.owner_name(&entry.hook) else {
            continue;
        };
        let arg = shown(
            &messages,
            handoff,
            note.as_ref().map(|(_, text)| text.as_str()),
        );
        let returned = session
            .runtime
            .hook_with(&entry.hook, arg, |value| {
                let change = decode(&name, value)?;
                let changed = change.is_some();
                Ok((change, changed))
            })
            .map(|(change, lines, changed)| {
                if changed {
                    pending.extend(lines);
                } else {
                    session.state.queue(lines);
                }
                change
            });
        match returned {
            Ok(None) => {}
            Ok(Some(text)) => note = Some((name.to_owned(), text)),
            Err(error) if entry.blocking => {
                return BeforeHandoffAnswer {
                    outcome: BeforeHandoffOutcome::Failed(HookFailure {
                        extension: name.to_owned(),
                        message: error.to_string(),
                    }),
                    notices,
                };
            }
            Err(error) => notices.push(failed_notice(&name, &error)),
        }
    }
    if note.is_some() {
        session.set_change_lines(pending);
    }
    BeforeHandoffAnswer {
        outcome: match note {
            Some((extension, text)) => BeforeHandoffOutcome::Note { text, extension },
            None => BeforeHandoffOutcome::NoNote,
        },
        notices,
    }
}

/// What a `before_handoff` hook is shown: what started the handoff, the
/// person's instructions when there were any, the conversation as Fiber's
/// neutral messages, and the note a hook before it returned, when one did.
fn shown(messages: &Value, handoff: &BeforeHandoff<'_>, note: Option<&str>) -> Value {
    let trigger = match handoff.trigger {
        contract::events::HandoffTrigger::Auto => "auto",
        contract::events::HandoffTrigger::Person => "person",
        contract::events::HandoffTrigger::Overflow => "overflow",
    };
    let mut shown = Map::new();
    shown.insert("trigger".into(), Value::String(trigger.to_owned()));
    if let Some(instructions) = handoff.instructions {
        shown.insert(
            "instructions".into(),
            Value::String(instructions.to_owned()),
        );
    }
    shown.insert("messages".into(), messages.clone());
    if let Some(note) = note {
        shown.insert("note".into(), Value::String(note.to_owned()));
    }
    Value::Object(shown)
}

/// The conversation as Fiber's neutral messages, in order, one message per
/// input: a person's message, a model's text, a tool call, or a tool's
/// result. Reasoning and a model's provider-only form never cross the seam;
/// an empty model text is left out, and nothing is merged.
pub(super) fn render(conversation: &[Input]) -> Value {
    Value::Array(
        conversation
            .iter()
            .filter_map(|input| message(conversation, input))
            .collect(),
    )
}

/// One input as a neutral message: its role and content parts, with images
/// by path. A tool call reads as the model's words, and its result as the
/// tool's answer beside the call's name, found by action in the same
/// conversation.
fn message(conversation: &[Input], input: &Input) -> Option<Value> {
    let (role, parts) = match input {
        Input::User { text, images } => {
            let mut parts = Vec::new();
            if !text.is_empty() || images.is_empty() {
                parts.push(text_part(text.clone()));
            }
            for image in images {
                parts.push(image_part(image));
            }
            ("user", parts)
        }
        Input::Assistant { text, .. } => {
            if text.is_empty() {
                return None;
            }
            ("assistant", vec![text_part(text.clone())])
        }
        Input::Reasoning { .. } => return None,
        Input::ToolCall { call, .. } => (
            "assistant",
            vec![text_part(format!(
                "Called the tool {} with arguments {}",
                call.name, call.arguments
            ))],
        ),
        Input::ToolSearchResult { action_id, loaded } => {
            return message(
                conversation,
                &Input::ToolResult {
                    action_id: action_id.clone(),
                    text: contract::provider::loaded_text(loaded),
                    is_error: false,
                    images: Vec::new(),
                    pdfs: Vec::new(),
                },
            );
        }
        Input::ToolResult {
            action_id,
            text,
            is_error,
            images,
            pdfs,
        } => {
            // The result's call, by action in the same conversation: an
            // orphan reads as some tool's answer.
            let name = action_name(conversation, action_id);
            let head = match (name, is_error) {
                (Some(name), false) => format!("The tool {name} returned:\n{text}"),
                (Some(name), true) => format!("The tool {name} failed:\n{text}"),
                (None, false) => format!("A tool returned:\n{text}"),
                (None, true) => format!("A tool failed:\n{text}"),
            };
            let mut parts = vec![text_part(head)];
            for image in images {
                parts.push(image_part(image));
            }
            for pdf in pdfs {
                parts.push(pdf_part(pdf));
            }
            ("user", parts)
        }
    };
    let mut shown = Map::new();
    shown.insert("role".into(), Value::String(role.to_owned()));
    shown.insert("content".into(), Value::Array(parts));
    Some(Value::Object(shown))
}

/// The call's name for `action`, from the conversation being rendered.
fn action_name<'a>(conversation: &'a [Input], action: &contract::ActionId) -> Option<&'a str> {
    conversation.iter().find_map(|input| match input {
        Input::ToolCall {
            action_id, call, ..
        } if action_id == action => Some(call.name.as_str()),
        Input::User { .. }
        | Input::Assistant { .. }
        | Input::Reasoning { .. }
        | Input::ToolCall { .. }
        | Input::ToolResult { .. }
        | Input::ToolSearchResult { .. } => None,
    })
}

/// One text part.
fn text_part(text: String) -> Value {
    let mut part = Map::new();
    part.insert("type".into(), Value::String("text".into()));
    part.insert("text".into(), Value::String(text));
    Value::Object(part)
}

/// One image part: the processed file in the session's `artifacts/`, by
/// path, with its type and size in pixels.
fn image_part(image: &contract::provider::ImageRef) -> Value {
    let mut part = Map::new();
    part.insert("type".into(), Value::String("image".into()));
    part.insert("path".into(), Value::String(image.path.clone()));
    part.insert("mime_type".into(), Value::String(image.mime_type.clone()));
    part.insert("width".into(), Value::Number(image.width.into()));
    part.insert("height".into(), Value::Number(image.height.into()));
    Value::Object(part)
}

/// One PDF part: the file in the session's `artifacts/`, with the pages
/// rendered as image parts when they could be rendered.
fn pdf_part(pdf: &contract::provider::PdfRef) -> Value {
    let mut part = Map::new();
    part.insert("type".into(), Value::String("pdf".into()));
    part.insert("path".into(), Value::String(pdf.path.clone()));
    part.insert("page_count".into(), Value::Number(pdf.page_count.into()));
    if let Some(pages) = &pdf.pages {
        part.insert(
            "pages".into(),
            Value::Array(pages.iter().map(image_part).collect()),
        );
    }
    Value::Object(part)
}

/// Reads a hook's return: `nil`, an empty table, or a table whose only key
/// is `note` with the note's text. A note that is empty after trimming
/// changes nothing, so it reads as no return. Anything else is the hook's
/// failure.
fn decode(extension: &str, value: Value) -> Result<Option<String>, Error> {
    let bad = |why: String| Error::BadReturn {
        extension: extension.to_owned(),
        callback: BEFORE_HANDOFF.to_owned(),
        why,
    };
    let map = match value {
        Value::Null => return Ok(None),
        // An empty Lua table reads as an empty array.
        Value::Array(items) if items.is_empty() => return Ok(None),
        Value::Object(map) => map,
        other @ (Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_)) => {
            return Err(bad(format!("{}, not a table", kind(&other))));
        }
    };
    let mut note = None;
    for (key, value) in map {
        if key != "note" {
            return Err(bad(format!("`{key}`, which is not `note`")));
        }
        match value {
            Value::String(text) if text.trim().is_empty() => {}
            Value::String(text) => note = Some(text),
            other @ (Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_)) => {
                return Err(bad(format!("`note` as {}, not a string", kind(&other))));
            }
        }
    }
    Ok(note)
}

/// A `non-blocking` hook's failure: its change is dropped, the chain goes
/// on, and this notice names it (`docs/extensions.md`, "When a hook
/// fails").
fn failed_notice(extension: &str, error: &Error) -> Notice {
    notice(
        ErrorCode::HookFailed,
        format!("The `{BEFORE_HANDOFF}` hook failed, so its change was dropped: {error}"),
        Some(extension),
    )
}
