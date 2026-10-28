//! The `session_start`, `turn_start`, `before_model_call` and `turn_end`
//! chains (`docs/extensions.md`, "The hook points", "When several hooks
//! share a point" and "When a hook fails"): what each hook is shown, and
//! what its answer decides.

use contract::ErrorCode;
use contract::events::Notice;
use contract::hook::{
    BeforeModelCall, CheckAnswer, CheckOutcome, Context, ContextAnswer, ContextOutcome,
    HookFailure, Refusal, SessionStart, StartReason, TurnEnd, TurnStart,
};
use contract::tool::Cancel;
use serde_json::{Map, Value};

use super::chain::{BEFORE_MODEL_CALL, Entry, SESSION_START, TURN_END, TURN_START};
use super::{SessionExtensions, kind, notice};
use crate::Error;

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;

/// Runs every `session_start` hook, in order: each is shown the same input
/// and every return is kept, in run order. Before the chain runs, the
/// mapped latest reason is recorded on the session and every live process
/// owner, so a restart after handoff resends the handoff reason.
pub(super) fn session_start(
    session: &SessionExtensions,
    start: &SessionStart<'_>,
) -> ContextAnswer {
    let reason = match start.reason {
        StartReason::Start => contract::events::ExtensionReason::New,
        StartReason::Resume => contract::events::ExtensionReason::Resume,
        StartReason::Fork => contract::events::ExtensionReason::Fork,
        StartReason::Rewind => contract::events::ExtensionReason::Rewind,
        StartReason::Handoff => contract::events::ExtensionReason::Handoff,
    };
    session.record_start_reason(reason);
    run_additive(
        session,
        &session.lifecycle.session_start,
        SESSION_START,
        "context",
        shown_start(start),
        start.cancel,
    )
}

/// Runs every `turn_start` hook on the turn's input, in order: each is
/// shown the same input and every return is kept, in run order.
pub(super) fn turn_start(session: &SessionExtensions, turn: &TurnStart<'_>) -> ContextAnswer {
    run_additive(
        session,
        &session.lifecycle.turn_start,
        TURN_START,
        "context",
        shown_turn(turn),
        turn.cancel,
    )
}

/// Runs every `turn_end` hook on the model's final reply, in order: each is
/// shown the same input and every return is kept, in run order.
pub(super) fn turn_end(session: &SessionExtensions, end: &TurnEnd<'_>) -> ContextAnswer {
    run_additive(
        session,
        &session.lifecycle.turn_end,
        TURN_END,
        "message",
        shown_end(end),
        end.cancel,
    )
}

/// Runs every `before_model_call` hook on the built request, in order: a
/// `check` hook judges what will actually be sent, so it runs last.
pub(super) fn before_model_call(
    session: &SessionExtensions,
    call: &BeforeModelCall<'_>,
) -> CheckAnswer {
    let chain = &session.lifecycle.before_model_call;
    if chain.is_empty() {
        return CheckAnswer {
            outcome: CheckOutcome::Passed,
            notices: Vec::new(),
        };
    }
    let arg = shown_call(call);
    let mut notices = Vec::new();
    for (at, entry) in chain.iter().enumerate() {
        if at > 0 && call.cancel.is_cancelled() {
            return CheckAnswer {
                outcome: CheckOutcome::Passed,
                notices,
            };
        }
        let Some(name) = session.runtime.owner_name(&entry.hook) else {
            continue;
        };
        let returned = session
            .runtime
            .hook_with(&entry.hook, arg.clone(), |value| {
                decode_additive(&name, BEFORE_MODEL_CALL, "refuse", value)
                    .map(|change| (change, false))
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
            Err(error) => notices.push(failed_notice(BEFORE_MODEL_CALL, &name, &error)),
        }
    }
    CheckAnswer {
        outcome: CheckOutcome::Passed,
        notices,
    }
}

/// Runs one additive chain (`session_start`, `turn_start` or `turn_end`):
/// every hook is shown the same input and every return is kept, in run
/// order. A refusal is not possible here; a `blocking` failure drops the
/// contexts kept so far and keeps the notices.
fn run_additive(
    session: &SessionExtensions,
    chain: &[Entry],
    point: &str,
    key: &str,
    arg: Value,
    cancel: &dyn Cancel,
) -> ContextAnswer {
    if chain.is_empty() {
        return ContextAnswer {
            outcome: ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        };
    }
    let mut context = Vec::new();
    let mut notices = Vec::new();
    for (at, entry) in chain.iter().enumerate() {
        if at > 0 && cancel.is_cancelled() {
            return ContextAnswer {
                outcome: ContextOutcome::Added(Vec::new()),
                notices,
            };
        }
        let Some(name) = session.runtime.owner_name(&entry.hook) else {
            continue;
        };
        let returned = session
            .runtime
            .hook_with(&entry.hook, arg.clone(), |value| {
                let change = decode_additive(&name, point, key, value)?;
                let changed = change.is_some();
                Ok((change, changed))
            })
            .map(|(change, lines, _)| match change {
                Some(text) => Some(Context {
                    extension: name.to_owned(),
                    text,
                    state: lines,
                }),
                None => {
                    session.state.queue(lines);
                    None
                }
            });
        match returned {
            Ok(None) => {}
            Ok(Some(added)) => context.push(added),
            Err(error) if entry.blocking => {
                return ContextAnswer {
                    outcome: ContextOutcome::Failed(HookFailure {
                        extension: name.to_owned(),
                        message: error.to_string(),
                    }),
                    notices,
                };
            }
            Err(error) => notices.push(failed_notice(point, &name, &error)),
        }
    }
    ContextAnswer {
        outcome: ContextOutcome::Added(context),
        notices,
    }
}

/// What a `session_start` hook is shown: why the session started, its id
/// and its workspace root.
fn shown_start(start: &SessionStart<'_>) -> Value {
    let reason = match start.reason {
        StartReason::Start => "start",
        StartReason::Resume => "resume",
        StartReason::Fork => "fork",
        StartReason::Rewind => "rewind",
        StartReason::Handoff => "handoff",
    };
    let mut shown = Map::new();
    shown.insert("reason".into(), Value::String(reason.to_owned()));
    shown.insert(
        "session_id".into(),
        Value::String(start.session_id.0.clone()),
    );
    shown.insert(
        "workspace".into(),
        Value::String(start.workspace.to_owned()),
    );
    Value::Object(shown)
}

/// What a `turn_start` hook is shown: the turn's input as `turn_started`
/// logs it, without an item this build does not know.
fn shown_turn(turn: &TurnStart<'_>) -> Value {
    let mut shown = Map::new();
    shown.insert(
        "input".into(),
        Value::Array(
            turn.input
                .iter()
                .filter(|item| !matches!(item, contract::events::InputItem::Unknown))
                .filter_map(|item| serde_json::to_value(item).ok())
                .collect(),
        ),
    );
    Value::Object(shown)
}

/// What a `before_model_call` hook is shown: the model reference and the
/// session's usage so far. Lua has no null, so an unknown cost is absent.
fn shown_call(call: &BeforeModelCall<'_>) -> Value {
    let mut usage = serde_json::to_value(call.usage).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut usage
        && map.get("cost") == Some(&Value::Null)
    {
        map.remove("cost");
    }
    let mut shown = Map::new();
    shown.insert("model".into(), Value::String(call.model.to_owned()));
    shown.insert("usage".into(), usage);
    Value::Object(shown)
}

/// What a `turn_end` hook is shown: the model's final reply.
fn shown_end(end: &TurnEnd<'_>) -> Value {
    let mut shown = Map::new();
    shown.insert("reply".into(), Value::String(end.reply.to_owned()));
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

/// Reads an additive hook's return (`session_start`, `turn_start` or
/// `turn_end`): `nil`, an empty table, or a table whose only key is `key`
/// with a non-empty string. Anything else is the hook's failure.
fn decode_additive(
    extension: &str,
    point: &str,
    key: &str,
    value: Value,
) -> Result<Option<String>, Error> {
    let bad = |why: String| Error::BadReturn {
        extension: extension.to_owned(),
        callback: point.to_owned(),
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
    let mut text = None;
    for (name, value) in map {
        if name != key {
            return Err(bad(format!("`{name}`, which is not `{key}`")));
        }
        match value {
            Value::String(returned) if !returned.is_empty() => text = Some(returned),
            Value::String(_) => {
                return Err(bad(format!(
                    "`{key}` as an empty string, not a non-empty string"
                )));
            }
            other @ (Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_)) => {
                return Err(bad(format!(
                    "`{key}` as {}, not a non-empty string",
                    kind(&other)
                )));
            }
        }
    }
    Ok(text)
}
