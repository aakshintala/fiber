//! `host.delegate` (`docs/extensions.md`, "Host calls"): start a delegate job
//! the session owns. It suspends the callback like `host.http`: the worker
//! runs the service's start and resumes the coroutine with the started job's
//! id. A wrong argument is an error in the calling code and raises a string,
//! which `pcall` catches. Refused in the entry script before it yields, like
//! `host.exec`.

use std::cell::Cell;
use std::rc::Rc;

use contract::JobId;
use mlua::{Lua, MultiValue, Table, Value as LuaValue};
use serde_json::{Map, Value};

/// `host.delegate` yields this tag, `"delegate"` and the call's spec, handle
/// and per-call timeout. The extension's thread runs the delegate start off
/// to the side and resumes the coroutine with the started job's id
/// (`docs/extensions.md`, "A host call suspends the code that made it").
/// Refused in the entry script before it yields, like `host.exec`. The
/// `{ timeout, run }` pair is kept in the chunk-local `handles` table under
/// a handle numbered from 1, stored before the yield so a start that
/// completes after its callback timed out still has its `run`; a failed
/// reply clears it, as the end's teardown does.
const DELEGATE: &str = r#"
local host, tag, in_entry, failure, snapshot, handles = ...
function host.delegate(spec, opts)
  if in_entry() then
    error(failure("closing", "host.delegate: not available while init.lua runs"), 0)
  end
  if type(spec) ~= "table" then
    error("host.delegate: `spec` must be a table", 2)
  end
  if type(opts) ~= "table" or math.type(opts.timeout) ~= "integer" or opts.timeout <= 0 then
    error("host.delegate: `timeout` must be a whole number of milliseconds above 0", 2)
  end
  if type(opts.run) ~= "function" then
    error("host.delegate: `run` must be a function", 2)
  end
  local text, problem = snapshot(spec)
  if problem then
    error(problem, 0)
  end
  local handle = #handles + 1
  handles[handle] = { timeout = opts.timeout, run = opts.run }
  local id, code, message = coroutine.yield(tag, "delegate", { spec = text, handle = handle, timeout = opts.timeout })
  if id == nil then
    handles[handle] = nil
    error(failure(code, message), 0)
  end
  return id
end
"#;

/// Sets `host.delegate` on `host`. `entry` is true while the entry script runs.
/// Returns the chunk-local `handles` table, so the VM can reach the stored
/// `{ timeout, run }` pair when the delegate ends.
pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    tag: &Table,
    entry: Rc<Cell<bool>>,
    failure: mlua::Function,
) -> mlua::Result<Table> {
    let in_entry = super::failure::in_entry(lua, &entry)?;
    // Reports its problem as a value, not a raise: a Rust raise would
    // reach the caller decorated with a stack traceback, and the chunk
    // raises the bare message instead, like its other string errors.
    let snapshot = lua.create_function(|_, spec: Table| match snapshot(&spec) {
        Ok(text) => Ok((Some(text), None)),
        Err(mlua::Error::RuntimeError(message)) => Ok((None, Some(message))),
        Err(other) => Err(other),
    })?;
    let handles = lua.create_table()?;
    lua.load(DELEGATE).set_name("=host.delegate").call::<()>((
        host.clone(),
        tag.clone(),
        in_entry,
        failure,
        snapshot,
        handles.clone(),
    ))?;
    Ok(handles)
}

/// The spec as the service sees it: its JSON object text. An empty table
/// becomes `{}`, which the service refuses as missing `description` and
/// `prompt`. A non-empty list, or one `to_json` refuses, fails here,
/// before the yield, so `pcall` catches it and no handle is stored.
fn snapshot(spec: &Table) -> mlua::Result<String> {
    let json = |why: &str| mlua::Error::RuntimeError(format!("host.delegate: `spec` json: {why}"));
    let value = super::to_json(&LuaValue::Table(spec.clone())).map_err(|e| {
        let why = e.to_string();
        // `to_json` fails as a Rust error, whose text carries mlua's
        // `runtime error: ` prefix (as in `host_tests`); strip it and the
        // shared `json: ` prefix so the message names only the problem.
        let why = why.strip_prefix("runtime error: ").unwrap_or(&why);
        json(why.strip_prefix("json: ").unwrap_or(why))
    })?;
    match value {
        Value::Object(map) => serde_json::to_string(&map).map_err(|e| json(&e.to_string())),
        // An empty table reads as `[]`; the service refuses its `{}` as
        // missing `description` and `prompt` (`invalid_arguments`).
        Value::Array(list) if list.is_empty() => Ok("{}".to_owned()),
        // A non-empty list names no arguments.
        Value::Array(_) => Err(mlua::Error::RuntimeError(
            "host.delegate: `spec` must be a table of named arguments".to_owned(),
        )),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            Err(json("a table converts to an array or an object"))
        }
    }
}

/// Reads a `host.delegate` yield: the snapshotted spec's text back into its
/// argument map, the `run` handle and the per-call timeout. It cannot fail
/// after the Lua half's snapshot passed; an `Err` is an error in the
/// calling code, raised as the string. The text came from `serde_json`, so
/// the parse cannot fail; a failure would be a bug and is reported as an
/// error in the calling code.
pub(crate) fn request(spec: &Table) -> mlua::Result<super::Request> {
    let text: String = spec.get("spec")?;
    let handle: u64 = spec.get("handle")?;
    let timeout_ms: u64 = spec.get("timeout")?;
    let arguments: Map<String, Value> = serde_json::from_str(&text)
        .map_err(|e| mlua::Error::RuntimeError(format!("host.delegate: {e}")))?;
    Ok(super::Request::Delegate {
        arguments,
        handle,
        timeout: std::time::Duration::from_millis(timeout_ms),
    })
}

/// The reply as Lua sees it: the started job's id, or `nil, code, message`
/// for the Lua half to raise as the table.
pub(crate) fn resume(
    lua: &Lua,
    reply: Result<JobId, (contract::ErrorCode, String)>,
) -> mlua::Result<MultiValue> {
    match reply {
        Ok(id) => Ok(MultiValue::from_vec(vec![LuaValue::String(
            lua.create_string(&id.0)?,
        )])),
        Err((code, message)) => Ok(MultiValue::from_vec(vec![
            LuaValue::Nil,
            LuaValue::String(lua.create_string(super::failure::code_name(&code))?),
            LuaValue::String(lua.create_string(message)?),
        ])),
    }
}

#[cfg(test)]
#[path = "delegate_tests.rs"]
mod tests;
