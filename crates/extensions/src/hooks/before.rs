//! The `before_tool` and `before_message` chains (`docs/extensions.md`,
//! "The hook points", "When several hooks share a point" and "When a hook
//! fails"): what each hook is shown, and what its answer decides.

use contract::ErrorCode;
use contract::events::Notice;
use contract::hook::{
    BeforeMessage, BeforeMessageAnswer, BeforeMessageOutcome, BeforeToolAnswer, BeforeToolCall,
    BeforeToolOutcome, CheckAnswer, CheckOutcome, Context, HookFailure, Refusal, Replaced,
};
use contract::shapes::ContentPart;
use serde_json::{Map, Value};

use super::chain::{BEFORE_MESSAGE, BEFORE_TOOL};
use super::{SessionExtensions, kind, name_once, notice};
use crate::Error;

#[cfg(test)]
#[path = "before_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "keys_tests.rs"]
mod keys_tests;

/// Refuses only the own hook reached while the loop serves a command's chain.
pub(super) fn own_command_failure(
    extension: &crate::LuaExtension,
    point: &str,
) -> Option<HookFailure> {
    extension.runs_own_command_call().then(|| HookFailure {
        extension: extension.name().to_owned(),
        message: format!("The `{point}` hook would wait behind its extension's command"),
    })
}

/// Runs the `sanitize` and `transform` `before_tool` hooks on `call`, in
/// order: a `check` hook judges what will actually run, so it runs later,
/// in [`check_tool`].
pub(super) fn before_tool(
    session: &SessionExtensions,
    call: &BeforeToolCall<'_>,
) -> BeforeToolAnswer {
    let mut arguments = call.arguments.clone();
    let mut pending = Vec::new();
    let mut changed_by = Vec::new();
    let mut notices = Vec::new();
    for entry in session.before_tool.iter().filter(|entry| !entry.check) {
        let Some(extension) = session.lua.get(entry.extension) else {
            continue;
        };
        if let Some(failure) = own_command_failure(extension, BEFORE_TOOL) {
            return BeforeToolAnswer {
                outcome: BeforeToolOutcome::Failed(failure),
                notices,
            };
        }
        let name = extension.name();
        let arg = shown(call, &arguments);
        let returned = extension
            .hook_with(BEFORE_TOOL, entry.index, arg, |value| {
                let change = decode_tool(name, value)?;
                let changed = matches!(&change, ToolReturn::Rewrite(_));
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
            Ok(ToolReturn::Nothing) => {}
            Ok(ToolReturn::Rewrite(rewritten)) => {
                arguments = rewritten;
                name_once(&mut changed_by, name);
            }
            Ok(ToolReturn::Refuse(reason)) => {
                return BeforeToolAnswer {
                    outcome: BeforeToolOutcome::Refused(Refusal {
                        extension: name.to_owned(),
                        reason,
                    }),
                    notices,
                };
            }
            Err(error) if entry.blocking => {
                return BeforeToolAnswer {
                    outcome: BeforeToolOutcome::Failed(HookFailure {
                        extension: name.to_owned(),
                        message: error.to_string(),
                    }),
                    notices,
                };
            }
            Err(error) => notices.push(failed_notice(BEFORE_TOOL, name, &error)),
        }
    }
    BeforeToolAnswer {
        outcome: if changed_by.is_empty() {
            BeforeToolOutcome::Unchanged
        } else {
            session.set_change_lines(pending);
            BeforeToolOutcome::Rewritten {
                arguments,
                changed_by,
            }
        },
        notices,
    }
}

/// Runs the `check` `before_tool` hooks on `call`, in order.
pub(super) fn check_tool(session: &SessionExtensions, call: &BeforeToolCall<'_>) -> CheckAnswer {
    let mut notices = Vec::new();
    for entry in session.before_tool.iter().filter(|entry| entry.check) {
        let Some(extension) = session.lua.get(entry.extension) else {
            continue;
        };
        if let Some(failure) = own_command_failure(extension, BEFORE_TOOL) {
            return CheckAnswer {
                outcome: CheckOutcome::Failed(failure),
                notices,
            };
        }
        let name = extension.name();
        let arg = shown(call, call.arguments);
        let returned = extension
            .hook_with(BEFORE_TOOL, entry.index, arg, |value| {
                decode_check(name, value).map(|change| (change, false))
            })
            .map(|(change, lines, _)| {
                session.state.queue(lines);
                change
            });
        match returned {
            Ok(None) => {}
            Ok(Some(reason)) => {
                return CheckAnswer {
                    outcome: CheckOutcome::Refused(Refusal {
                        extension: name.to_owned(),
                        reason,
                    }),
                    notices,
                };
            }
            Err(error) if entry.blocking => {
                return CheckAnswer {
                    outcome: CheckOutcome::Failed(HookFailure {
                        extension: name.to_owned(),
                        message: error.to_string(),
                    }),
                    notices,
                };
            }
            Err(error) => notices.push(failed_notice(BEFORE_TOOL, name, &error)),
        }
    }
    CheckAnswer {
        outcome: CheckOutcome::Passed,
        notices,
    }
}

/// Runs every `before_message` hook on `message`, in order.
pub(super) fn before_message(
    session: &SessionExtensions,
    message: &BeforeMessage<'_>,
) -> BeforeMessageAnswer {
    let mut content: Option<Vec<ContentPart>> = None;
    let mut pending = Vec::new();
    let mut changed_by = Vec::new();
    let mut context = Vec::new();
    let mut notices = Vec::new();
    for entry in &session.before_message {
        let Some(extension) = session.lua.get(entry.extension) else {
            continue;
        };
        let name = extension.name();
        let current = content.as_deref().unwrap_or(message.content);
        let arg = shown_message(current, message);
        let returned = extension
            .hook_with(BEFORE_MESSAGE, entry.index, arg, |value| {
                let change = decode_message(name, current, value, entry.check)?;
                let changed = matches!(&change, MessageReturn::Change { .. });
                Ok((change, changed))
            })
            .map(|(change, lines, changed)| {
                if changed {
                    (change, lines)
                } else {
                    session.state.queue(lines);
                    (change, Vec::new())
                }
            });
        match returned {
            Ok((MessageReturn::Nothing, _)) => {}
            Ok((MessageReturn::Change { replace, text }, lines)) => {
                if let Some(parts) = replace {
                    content = Some(parts);
                    name_once(&mut changed_by, name);
                }
                if let Some(text) = text {
                    context.push(Context {
                        extension: name.to_owned(),
                        text,
                        state: lines,
                    });
                } else {
                    pending.extend(lines);
                }
            }
            Ok((MessageReturn::Refuse(reason), _)) => {
                return BeforeMessageAnswer {
                    outcome: BeforeMessageOutcome::Refused(Refusal {
                        extension: name.to_owned(),
                        reason,
                    }),
                    notices,
                };
            }
            Err(error) if entry.blocking => {
                return BeforeMessageAnswer {
                    outcome: BeforeMessageOutcome::Failed(HookFailure {
                        extension: name.to_owned(),
                        message: error.to_string(),
                    }),
                    notices,
                };
            }
            Err(error) => notices.push(failed_notice(BEFORE_MESSAGE, name, &error)),
        }
    }
    if content.is_some() || !context.is_empty() {
        session.set_change_lines(pending);
    }
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: content.map(|content| Replaced {
                content,
                changed_by,
            }),
            context,
        },
        notices,
    }
}

/// What a `before_message` hook is shown: the message's content parts as
/// logged, without a part this build does not know, plus the sender's id
/// and its parent's id for a session message, each absent otherwise.
fn shown_message(content: &[ContentPart], message: &BeforeMessage<'_>) -> Value {
    let mut shown = Map::new();
    shown.insert(
        "content".into(),
        Value::Array(
            content
                .iter()
                .filter(|part| !matches!(part, ContentPart::Unknown))
                .filter_map(|part| serde_json::to_value(part).ok())
                .collect(),
        ),
    );
    if let Some(sender) = message.sender {
        shown.insert(
            "from_session_id".into(),
            Value::String(sender.from_session_id.0.clone()),
        );
        if let Some(parent) = sender.parent_session_id {
            shown.insert("parent_session_id".into(), Value::String(parent.0.clone()));
        }
    }
    Value::Object(shown)
}

/// What a `before_message` hook returned.
enum MessageReturn {
    /// It changed nothing.
    Nothing,
    /// Its replacement content and its added context, each absent when it
    /// returned neither.
    Change {
        /// The replacement content.
        replace: Option<Vec<ContentPart>>,
        /// The context to add.
        text: Option<String>,
    },
    /// Its refusal reason.
    Refuse(String),
}

/// A `check` hook that returned a change has failed.
const CHECK_CHANGE: &str = "returned a change, which a `check` hook cannot make";

/// Reads a hook's return: `nil`, a table holding `content` (a non-empty
/// list of parts) or `context` (a non-empty string) or both, or `refuse`
/// (a non-empty string) alone. Anything else is the hook's failure. A
/// `check` hook returns no change: `arguments`, `content` or `context`
/// from one has failed.
fn decode_message(
    extension: &str,
    current: &[ContentPart],
    value: Value,
    check: bool,
) -> Result<MessageReturn, Error> {
    let bad = |why: String| Error::BadReturn {
        extension: extension.to_owned(),
        callback: BEFORE_MESSAGE.to_owned(),
        why,
    };
    let map = match value {
        Value::Null => return Ok(MessageReturn::Nothing),
        // An empty Lua table reads as an empty array.
        Value::Array(items) if items.is_empty() => return Ok(MessageReturn::Nothing),
        Value::Object(map) => map,
        other @ (Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_)) => {
            return Err(bad(format!("{}, not a table", kind(&other))));
        }
    };
    let mut replace = None;
    let mut text = None;
    let mut refuse = None;
    for (key, value) in map {
        if check && matches!(key.as_str(), "arguments" | "content" | "context") {
            return Err(bad(CHECK_CHANGE.to_owned()));
        }
        match (key.as_str(), value) {
            ("content", Value::Array(items)) => {
                replace = Some(decode_parts(extension, current, items)?);
            }
            ("content", other) => {
                return Err(bad(format!(
                    "`content` as {}, not a list of parts",
                    kind(&other)
                )));
            }
            ("context", Value::String(text_value)) if !text_value.is_empty() => {
                text = Some(text_value);
            }
            ("context", Value::String(_)) => {
                return Err(bad(
                    "`context` as an empty string, not a non-empty string".to_owned()
                ));
            }
            ("context", other) => {
                return Err(bad(format!(
                    "`context` as {}, not a non-empty string",
                    kind(&other)
                )));
            }
            ("refuse", Value::String(reason)) if !reason.is_empty() => refuse = Some(reason),
            ("refuse", Value::String(_)) => {
                return Err(bad(
                    "`refuse` as an empty string, not a non-empty string".to_owned()
                ));
            }
            ("refuse", other) => {
                return Err(bad(format!(
                    "`refuse` as {}, not a non-empty string",
                    kind(&other)
                )));
            }
            (_, _) => {
                return Err(bad(format!(
                    "`{key}`, which is not `content`, `context` or `refuse`"
                )));
            }
        }
    }
    match (replace, text, refuse) {
        (None, None, None) => Ok(MessageReturn::Nothing),
        (replace, text, None) => Ok(MessageReturn::Change { replace, text }),
        (None, None, Some(reason)) => Ok(MessageReturn::Refuse(reason)),
        (Some(_), _, Some(_)) | (None, Some(_), Some(_)) => Err(bad(
            "`refuse` beside another key: a hook either changes the message or refuses it"
                .to_owned(),
        )),
    }
}

/// Reads a replacement: a non-empty list of text parts and image or pdf
/// parts the message holds. A hook can drop one, never add one: any other
/// part is the hook's failure.
fn decode_parts(
    extension: &str,
    current: &[ContentPart],
    items: Vec<Value>,
) -> Result<Vec<ContentPart>, Error> {
    let bad = |why: String| Error::BadReturn {
        extension: extension.to_owned(),
        callback: BEFORE_MESSAGE.to_owned(),
        why,
    };
    if items.is_empty() {
        return Err(bad(
            "`content` as an empty list: a replacement holds at least one part".to_owned(),
        ));
    }
    let mut parts = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let place = index + 1;
        let part: ContentPart = serde_json::from_value(item).map_err(|_| {
            bad(format!(
                "`content` part {place} is not a text, image or pdf part"
            ))
        })?;
        if matches!(part, ContentPart::Unknown) {
            return Err(bad(format!(
                "`content` part {place} is not a text, image or pdf part"
            )));
        }
        if !matches!(part, ContentPart::Text { .. }) && !current.contains(&part) {
            return Err(bad(format!(
                "`content` part {place} is not one of the message's own parts: \
                 a hook can drop an image or pdf part, never add one"
            )));
        }
        parts.push(part);
    }
    Ok(parts)
}

/// What a `before_tool` hook is shown: the call, with `arguments` as the
/// hook before it left them.
fn shown(call: &BeforeToolCall<'_>, arguments: &Map<String, Value>) -> Value {
    let mut shown = Map::new();
    shown.insert("tool".into(), Value::String(call.tool.to_owned()));
    shown.insert("arguments".into(), Value::Object(arguments.clone()));
    shown.insert(
        "effects".into(),
        Value::Array(
            call.declared
                .effects
                .iter()
                .map(|effect| serde_json::to_value(effect).unwrap_or(Value::Null))
                .collect(),
        ),
    );
    if let Some(paths) = &call.declared.paths {
        shown.insert(
            "paths".into(),
            Value::Array(
                paths
                    .iter()
                    .map(|path| Value::String(path.clone()))
                    .collect(),
            ),
        );
    }
    shown.insert("reversible".into(), Value::Bool(call.declared.reversible));
    Value::Object(shown)
}

/// A `non-blocking` hook's failure: its change is dropped, the chain goes
/// on, and this notice names it (`docs/extensions.md`, "When a hook
/// fails").
fn failed_notice(point: &str, extension: &str, error: &Error) -> Notice {
    notice(
        ErrorCode::HookFailed,
        format!("The `{point}` hook failed, so its change was dropped: {error}"),
        Some(extension),
    )
}

/// What a `before_tool` hook returned.
enum ToolReturn {
    /// It changed nothing.
    Nothing,
    /// Its replacement arguments.
    Rewrite(Map<String, Value>),
    /// Its refusal reason.
    Refuse(String),
}

/// Reads a hook's return: `nil`, or a table holding `arguments` (a table)
/// or `refuse` (a non-empty string) alone. Anything else is the hook's
/// failure. A top-level `arguments = {}` reads as an empty object.
fn decode_tool(extension: &str, value: Value) -> Result<ToolReturn, Error> {
    let bad = |why: String| Error::BadReturn {
        extension: extension.to_owned(),
        callback: BEFORE_TOOL.to_owned(),
        why,
    };
    let map = match value {
        Value::Null => return Ok(ToolReturn::Nothing),
        // An empty Lua table reads as an empty array.
        Value::Array(items) if items.is_empty() => return Ok(ToolReturn::Nothing),
        Value::Object(map) => map,
        other @ (Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_)) => {
            return Err(bad(format!("{}, not a table", kind(&other))));
        }
    };
    let mut rewrite = None;
    let mut refuse = None;
    for (key, value) in map {
        match (key.as_str(), value) {
            ("arguments", Value::Object(arguments)) => rewrite = Some(arguments),
            // Lua cannot tell an empty table from an empty list: a
            // top-level `arguments = {}` reads as an empty object.
            ("arguments", Value::Array(items)) if items.is_empty() => {
                rewrite = Some(Map::new());
            }
            ("arguments", other) => {
                return Err(bad(format!("`arguments` as {}, not a table", kind(&other))));
            }
            ("refuse", Value::String(reason)) if !reason.is_empty() => refuse = Some(reason),
            ("refuse", Value::String(_)) => {
                return Err(bad(
                    "`refuse` as an empty string, not a non-empty string".to_owned()
                ));
            }
            ("refuse", other) => {
                return Err(bad(format!(
                    "`refuse` as {}, not a non-empty string",
                    kind(&other)
                )));
            }
            (_, _) => {
                return Err(bad(format!(
                    "`{key}`, which is not `arguments` or `refuse`"
                )));
            }
        }
    }
    match (rewrite, refuse) {
        (Some(_), Some(_)) => Err(bad(
            "`refuse` beside `arguments`: a hook either rewrites a call or refuses it".to_owned(),
        )),
        (Some(arguments), None) => Ok(ToolReturn::Rewrite(arguments)),
        (None, Some(reason)) => Ok(ToolReturn::Refuse(reason)),
        (None, None) => Ok(ToolReturn::Nothing),
    }
}

/// Reads a `check` hook's return: `nil`, or a table holding `refuse` (a
/// non-empty string) alone. A returned change (`arguments`, `content` or
/// `context`) has failed, as has anything else outside that shape.
fn decode_check(extension: &str, value: Value) -> Result<Option<String>, Error> {
    let bad = |why: String| Error::BadReturn {
        extension: extension.to_owned(),
        callback: BEFORE_TOOL.to_owned(),
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
    let mut refuse = None;
    for (key, value) in map {
        match (key.as_str(), value) {
            ("arguments" | "content" | "context", _) => {
                return Err(bad(CHECK_CHANGE.to_owned()));
            }
            ("refuse", Value::String(reason)) if !reason.is_empty() => refuse = Some(reason),
            ("refuse", Value::String(_)) => {
                return Err(bad(
                    "`refuse` as an empty string, not a non-empty string".to_owned()
                ));
            }
            ("refuse", other) => {
                return Err(bad(format!(
                    "`refuse` as {}, not a non-empty string",
                    kind(&other)
                )));
            }
            (_, _) => {
                return Err(bad(format!("`{key}`, which is not `refuse`")));
            }
        }
    }
    // Any key besides `refuse` returns above, so a refusal that reaches
    // here stood alone.
    Ok(refuse)
}
