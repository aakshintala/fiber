//! Every failed host call raises `{ code, message }`
//! (`docs/extensions.md`, "How an extension runs"): one Lua constructor
//! builds them all, and one Rust error carries them out of host functions.
//!
//! A Rust host function never raises a failure as a Rust error directly:
//! mlua would wrap it as userdata, and `pcall` would see that, not a table.
//! Instead each failing call has a Lua half, as `host.http` and `host.exec`
//! already have, that receives `(nil, code, message)` from Rust or from
//! `coroutine.yield` and raises `error(failure(code, message), 0)`. Calls
//! implemented wholly in Rust (`host.fs`, `host.secret`, `host.config`,
//! `host.data_dir`, `host.oauth.open`) raise [`Failure`] as an external
//! error, which the prelude's `pcall` converts to the same table with
//! [`convert`]; uncaught, mlua stringifies the table through `__tostring`
//! (or the external through its `Display`), so the callback's failure text
//! is `message` either way. A wrong argument stays a string error, "an
//! error in the calling code", as do the entry-script refusals.

use contract::ErrorCode;
use mlua::{Function, Lua, Table, Value as LuaValue};

/// A host call's failure: its stable code and what a person reads. Raised
/// from Rust host functions; the prelude's `pcall` converts it to the same
/// table [`install`] builds, and uncaught it fails the callback as any Lua
/// error does, with [`message`](Failure::message) as its text.
#[derive(Debug)]
pub(crate) struct Failure {
    /// The stable label a caller switches on, never parsing the message.
    pub code: ErrorCode,
    /// What a person reads; single-line, naming the call.
    pub message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

/// Raises the host call's failure: its code and message reach Lua as
/// `(nil, code, message)` for the call's Lua half, or as a table through
/// the prelude's `pcall`.
pub(crate) fn fail(code: ErrorCode, message: String) -> mlua::Error {
    mlua::Error::external(Failure { code, message })
}

/// The code's name as Lua sees it: the registry's snake_case label.
pub(crate) fn code_name(code: &ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "extension_failed".to_owned())
}

/// What `install` builds: the `failure(code, message)` constructor the host
/// halves raise, and the `pcall` converter that maps a caught Rust failure
/// to the same table.
pub(crate) struct FailureLib {
    /// Builds `{ code, message }` with the shared `__tostring` metatable.
    pub failure: Function,
    /// Maps a caught external host failure to its table, else nil.
    pub convert: Function,
}

/// Builds [`FailureLib`]: one shared `__tostring` metatable behind both
/// functions, so a table and its converted twin stringify alike.
pub(crate) fn install(lua: &Lua) -> mlua::Result<FailureLib> {
    let metatable = lua.create_table()?;
    metatable.set(
        "__tostring",
        lua.create_function(|_, failed: Table| {
            let message: String = failed.get("message")?;
            Ok(message)
        })?,
    )?;
    let failure_mt = metatable.clone();
    let failure = lua.create_function(move |lua, (code, message): (String, String)| {
        raised(lua, &failure_mt, &code, &message)
    })?;
    let convert_mt = metatable.clone();
    let convert = lua.create_function(move |lua, err: LuaValue| {
        converted(lua, &convert_mt, &err).map(|table| table.map_or(LuaValue::Nil, LuaValue::Table))
    })?;
    Ok(FailureLib { failure, convert })
}

/// The `{ code, message }` table with the shared metatable.
fn raised(lua: &Lua, metatable: &Table, code: &str, message: &str) -> mlua::Result<Table> {
    let failed = lua.create_table()?;
    failed.set("code", code)?;
    failed.set("message", message)?;
    failed.set_metatable(Some(metatable.clone()))?;
    Ok(failed)
}

/// The table a caught `err` converts to: a Rust host failure's code and
/// message, an unattended login's `authentication_failed`, or a failed
/// refresh's pass-through. Anything else is not a host failure.
fn converted(lua: &Lua, metatable: &Table, err: &LuaValue) -> mlua::Result<Option<Table>> {
    let LuaValue::Error(e) = err else {
        return Ok(None);
    };
    if let Some(failed) = e.downcast_ref::<Failure>() {
        return raised(lua, metatable, &code_name(&failed.code), &failed.message).map(Some);
    }
    if let Some(unattended) = e.downcast_ref::<crate::oauth::Unattended>() {
        return raised(
            lua,
            metatable,
            &code_name(&ErrorCode::AuthenticationFailed),
            &unattended.to_string(),
        )
        .map(Some);
    }
    if let Some(refresh) = e.downcast_ref::<crate::oauth::RefreshFailed>() {
        // A failure table passes through with its own code; a string the
        // refresh function raised itself is the credential failing.
        let code = refresh.table_code.as_deref().unwrap_or("credential_failed");
        return raised(lua, metatable, code, &refresh.message).map(Some);
    }
    Ok(None)
}

/// The `(code, message)` a Lua value carries when it is a failure table:
/// two strings and nothing else is required, so a table `pcall` caught
/// passes back through unchanged.
pub(crate) fn as_failure(value: &LuaValue) -> Option<(String, String)> {
    let LuaValue::Table(failed) = value else {
        return None;
    };
    let (code, message): (LuaValue, LuaValue) =
        (failed.get("code").ok()?, failed.get("message").ok()?);
    match (code, message) {
        (LuaValue::String(code), LuaValue::String(message)) => {
            let (Ok(code), Ok(message)) = (code.to_str(), message.to_str()) else {
                return None;
            };
            Some((code.to_owned(), message.to_owned()))
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "failure_tests.rs"]
mod tests;
