//! Every failed host call raises `{ code, message }`
//! (`docs/extensions.md`, "How an extension runs"): one Lua constructor
//! builds them all, and one Rust error carries them out of host functions.
//!
//! A Rust host function never raises a failure as a Rust error directly:
//! mlua would wrap it as userdata, and `pcall` would see that, not a table.
//! Instead each failing call has a Lua half, as `host.http` and `host.exec`
//! already have, that receives `(nil, code, message)` from Rust or from
//! `coroutine.yield` and raises `error(failure(code, message), 0)`. Calls
//! implemented wholly in Rust (`host.secret`, `host.fs`, `host.config`,
//! `host.oauth.pkce` and the held credential's methods) return `(nil,
//! code, message)` from Rust for a coded failure, and the Lua half [`wrap`]
//! raises the same table at the call site; uncaught, mlua stringifies the
//! failure text is `message` either way. A wrong argument stays a string error, "an
//! error in the calling code", as do the entry-script refusals.

use contract::ErrorCode;
use mlua::{Function, Lua, MultiValue, Table, Value as LuaValue};

/// A host call's failure: its stable code and what a person reads. The
/// prelude's `pcall` converts a caught one to the same table [`install`]
/// builds; direct host calls instead return `(nil, code, message)` for
/// [`wrap`] to raise, so the value is the table at its source.
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

/// Wraps a raw host function that returns its value on success and
/// `(nil, code, message)` on a coded failure: the wrapper raises
/// `error(failure(code, message), 0)` at the call site, so the value is
/// the table at its source even for a caller catching it with
/// `coroutine.resume`. A wrong argument stays a string error: the raw
/// function raises it, and the wrapper never sees it.
pub(crate) fn wrap(lua: &Lua, raw: Function, failure: &Function) -> mlua::Result<Function> {
    lua.load(
        r#"
local raw, failure = ...
local pack, unpack = table.pack, table.unpack
return function(...)
  local r = pack(raw(...))
  if r[2] ~= nil then error(failure(r[2], r[3]), 0) end
  return unpack(r, 1, r.n)
end
"#,
    )
    .set_name("=host failure wrap")
    .call((raw, failure.clone()))
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
