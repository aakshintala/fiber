//! Host failures raised in Lua are tables (`docs/extensions.md`, "How an
//! extension runs"). A failure whose uncaught code is not `extension_failed`
//! (OAuth's unattended calls and refresh) is also recorded in a per-VM slot,
//! which the callback boundary reads when the same error escapes.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use contract::ErrorCode;
use mlua::{Function, Lua, MultiValue, Table, Value as LuaValue};

/// How a recorded failure maps if it escapes its callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Boundary {
    /// No person was attached for an interactive OAuth call.
    Unattended { call: String },
    /// A refresh function failed, classified by whether its last request got
    /// a response from the token endpoint.
    Refresh { reached: bool },
}

/// The last recorded failure raised in this VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingFailure {
    /// The raised value's `tostring`: what mlua reports when it escapes.
    pub(crate) text: String,
    /// What the callback's error reports: the table's own `message`.
    pub(crate) message: String,
    pub(crate) boundary: Boundary,
}

/// How many caught failures the per-VM list keeps. Two unattended calls
/// caught before a rethrow must both keep their classification; past the
/// cap the oldest is dropped.
const MAX_PENDING: usize = 32;

/// Per-VM state shared by the Lua constructor and its callback boundary.
#[derive(Clone, Default)]
pub(crate) struct FailureState(Arc<Mutex<Vec<PendingFailure>>>);

impl FailureState {
    /// Take the pending failure when `error` is it escaping the callback.
    ///
    /// mlua hands an uncaught error value over as its `tostring`, which for a
    /// failure table is its message, followed by Lua's traceback when it left
    /// a resumed thread; an error that crossed a Rust function is wrapped in
    /// `CallbackError`.
    pub(crate) fn take_matching(&self, error: &mlua::Error) -> Option<PendingFailure> {
        if let mlua::Error::CallbackError { cause, .. } = error {
            return self.take_matching(cause);
        }
        let mlua::Error::RuntimeError(text) = error else {
            return None;
        };
        let mut pending = lock(&self.0);
        let at = pending.iter().rposition(|failure| {
            text.strip_prefix(failure.text.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with("\nstack traceback:"))
        });
        at.map(|at| pending.remove(at))
    }

    fn record(&self, text: String, message: String, boundary: &str) -> mlua::Result<()> {
        let boundary = match boundary {
            "unattended:open" => Boundary::Unattended {
                call: "open".into(),
            },
            "unattended:callback" => Boundary::Unattended {
                call: "callback".into(),
            },
            "unattended:poll" => Boundary::Unattended {
                call: "poll".into(),
            },
            "refresh:reached" => Boundary::Refresh { reached: true },
            "refresh:unreached" => Boundary::Refresh { reached: false },
            other => {
                return Err(mlua::Error::runtime(format!(
                    "unknown failure boundary `{other}`"
                )));
            }
        };
        let mut pending = lock(&self.0);
        // An unattended call that fails inside a refresh function keeps its
        // own mapping when the refresh forwards the same table.
        if matches!(boundary, Boundary::Refresh { .. })
            && pending.iter().any(|failure| {
                failure.text == text && matches!(failure.boundary, Boundary::Unattended { .. })
            })
        {
            return Ok(());
        }
        if pending.len() >= MAX_PENDING {
            pending.remove(0);
        }
        pending.push(PendingFailure {
            text,
            message,
            boundary,
        });
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The failure table constructor and per-VM callback-boundary state.
pub(crate) struct FailureLib {
    /// `failure(code, message, boundary?)` builds `{ code, message }` with
    /// the shared `__tostring` metatable, recording it when given a boundary.
    pub(crate) failure: Function,
    /// `note_failure(text, message, boundary)` records a value raised
    /// elsewhere, which then passes through unchanged.
    pub(crate) note_failure: Function,
    /// Lets mlua re-raise a Rust panic instead of exposing it as a Lua error.
    pub(crate) rethrow_panic: Function,
    /// State read by `Vm::error`; records stay across callbacks so a
    /// suspended callback's caught failure still classifies its rethrow.
    pub(crate) state: FailureState,
}

/// Builds the shared failure constructor and its pending-error recorder.
pub(crate) fn install(lua: &Lua) -> mlua::Result<FailureLib> {
    let metatable = lua.create_table()?;
    metatable.set(
        "__tostring",
        lua.create_function(|_, failed: Table| {
            let message: String = failed.get("message")?;
            Ok(message)
        })?,
    )?;
    let state = FailureState::default();
    let failure_state = state.clone();
    let failure_mt = metatable.clone();
    let failure = lua.create_function(
        move |lua, (code, message, boundary): (String, String, Option<String>)| {
            if let Some(boundary) = boundary {
                failure_state.record(message.clone(), message.clone(), &boundary)?;
            }
            raised(lua, &failure_mt, &code, &message)
        },
    )?;
    let note_state = state.clone();
    let note_failure = lua.create_function(
        move |_, (text, message, boundary): (String, String, String)| {
            note_state.record(text, message, &boundary)
        },
    )?;
    let rethrow_panic = lua.create_function(|_, _: LuaValue| Ok(()))?;
    Ok(FailureLib {
        failure,
        note_failure,
        rethrow_panic,
        state,
    })
}

/// Wraps a raw host function that returns its value on success,
/// `(nil, code, message)` on a coded failure, and `(nil, message)` with no
/// third value on an error in the calling code. The table, or the string,
/// is raised at the call site, so even `coroutine.resume` catches it there.
pub(crate) fn wrap(lua: &Lua, raw: Function, failure: &Function) -> mlua::Result<Function> {
    wrap_with_boundary(lua, raw, failure, None)
}

/// Wraps an OAuth call whose uncaught failure has a special callback mapping.
pub(crate) fn wrap_with_boundary(
    lua: &Lua,
    raw: Function,
    failure: &Function,
    boundary: Option<&str>,
) -> mlua::Result<Function> {
    lua.load(
        r#"
local raw, failure, boundary = ...
local pack, unpack = table.pack, table.unpack
return function(...)
  local r = pack(raw(...))
  if r[2] ~= nil and r[3] == nil then error(r[2], 0) end
  if r[2] ~= nil then error(failure(r[2], r[3], boundary), 0) end
  return unpack(r, 1, r.n)
end
"#,
    )
    .set_name("=host failure wrap")
    .call((raw, failure.clone(), boundary))
}

/// The `(nil, message)` a raw host function returns for an error in the
/// calling code, so [`wrap`] raises it as the string. A Rust host function
/// never raises as a Rust error: mlua wraps that as userdata, and `pcall`
/// would see that, not a string or a table.
pub(crate) fn raw_string(lua: &Lua, message: String) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_vec(vec![
        LuaValue::Nil,
        LuaValue::String(lua.create_string(message)?),
    ]))
}

/// The `(nil, code, message)` a raw host function returns for a coded
/// failure, so [`wrap`] raises it as the table.
pub(crate) fn raw_failure(
    lua: &Lua,
    code: &ErrorCode,
    message: String,
) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_vec(vec![
        LuaValue::Nil,
        LuaValue::String(lua.create_string(code_name(code))?),
        LuaValue::String(lua.create_string(message)?),
    ]))
}

/// The code's name as Lua sees it: the registry's snake_case label.
pub(crate) fn code_name(code: &ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "extension_failed".to_owned())
}

/// The `{ code, message }` table with the shared metatable.
fn raised(lua: &Lua, metatable: &Table, code: &str, message: &str) -> mlua::Result<Table> {
    let failed = lua.create_table()?;
    failed.set("code", code)?;
    failed.set("message", message)?;
    failed.set_metatable(Some(metatable.clone()))?;
    Ok(failed)
}

#[cfg(test)]
#[path = "failure_tests.rs"]
mod tests;
