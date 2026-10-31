//! `host.tool` (`docs/extensions.md`, "Running a tool"): runs a tool as an
//! inner call of the running tool call, through the same path a model's
//! call takes. It suspends the callback like `host.http`: the runner runs
//! the call and resumes the coroutine with its outcome. An outcome that did
//! not complete is an ordinary return, not a Lua error, so the extension
//! decides what to do next. An error in the calling code raises a string,
//! which `pcall` catches. Refused in the entry script before it yields,
//! like `host.exec`.

use std::cell::Cell;
use std::rc::Rc;

use contract::events::{CallStatus, ToolCallCompleted};
use mlua::{Lua, Table, Value as LuaValue};
use serde_json::{Map, Value};

/// How many nested tables `args` may hold. `check` converts with `to_json`,
/// serialises, and parses that text back before it returns `true`, because
/// `request` parses the same text after the yield and serde_json's parser
/// refuses a 128th nested container. Checking with the parser itself makes
/// the cap and the later parse agree by construction.
const TOOL_DEPTH: usize = super::MAX_DEPTH;

/// `host.tool` yields this tag, `"tool"` and the call's spec. The
/// extension's thread runs the tool through its runner and resumes the
/// coroutine with the reply (`docs/extensions.md`, "A host call suspends the
/// code that made it"). The arguments travel as the JSON text `check`
/// returned, so a coroutine that yields `host.tool` through a parent which
/// then mutates the shared table cannot change what the host reads.
const TOOL: &str = r#"
local host, tag, in_entry, check = ...
function host.tool(name, args)
  if in_entry() then
    error("host.tool: not available while init.lua runs", 0)
  end
  local ok, text = check(name, args)
  if not ok then error(text, 2) end
  local result, message = coroutine.yield(tag, "tool", { name = name, args = text })
  if result == nil then error(message, 0) end
  return result
end
"#;

/// Sets `host.tool` on `host`. `entry` is true while the entry script runs.
pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    tag: &Table,
    entry: Rc<Cell<bool>>,
) -> mlua::Result<()> {
    let in_entry = super::failure::in_entry(lua, &entry)?;
    let check = lua.create_function(|_, (name, args): (LuaValue, LuaValue)| {
        // `true` with the arguments as a JSON text, or `false` with the
        // string the Lua half raises, so `pcall` catches a string, never a
        // table.
        let checked: (bool, Option<String>) = check_args(&name, &args);
        Ok(checked)
    })?;
    lua.load(TOOL)
        .set_name("=host.tool")
        .call::<()>((host.clone(), tag.clone(), in_entry, check))
}

/// Checks `name` with `args`: `true` with the arguments as a JSON text, or
/// `false` with the string the Lua half raises.
fn check_args(name: &LuaValue, args: &LuaValue) -> (bool, Option<String>) {
    let fail = |message: String| (false, Some(message));
    let LuaValue::String(name) = name else {
        return fail("host.tool: `name` must be a string".to_owned());
    };
    if name.to_str().is_err() {
        return fail("host.tool: `name` must be a string".to_owned());
    }
    // `nil` is no arguments; an empty table reads as an empty JSON array,
    // which `request` reads as no arguments.
    if args.is_nil() {
        return (true, Some("{}".to_owned()));
    }
    if !matches!(args, LuaValue::Table(_)) {
        return fail("host.tool: `args` must be a table".to_owned());
    }
    let json = match super::to_json(args) {
        Ok(json) => json,
        Err(mlua::Error::RuntimeError(inner)) if inner.contains("nests deeper than") => {
            return fail(format!("host.tool: `args` nests deeper than {TOOL_DEPTH}"));
        }
        Err(_) => {
            return fail("host.tool: `args` must be a table of JSON values".to_owned());
        }
    };
    let text = json.to_string();
    match serde_json::from_str::<Value>(&text) {
        Ok(_) => (true, Some(text)),
        Err(_) => fail(format!("host.tool: `args` nests deeper than {TOOL_DEPTH}")),
    }
}

/// One `host.tool` call, copied out of Lua so the runner can run it: the
/// tool's name and its arguments as JSON. The Lua half checked their
/// shapes; an empty table reads as no arguments, since an empty Lua table
/// is an empty JSON array. A non-empty array stays an array, and the loop's
/// existing check fails it `invalid_arguments` as an ordinary return.
pub(crate) struct ToolRequest {
    /// The tool to run.
    pub(crate) name: String,
    /// Its arguments.
    pub(crate) arguments: Value,
}

/// Reads a `host.tool` yield: the tool's name and the JSON text `check`
/// produced. It cannot fail after the Lua half's `check` passed; an `Err`
/// is an error in the calling code, raised as the string.
pub(crate) fn request(spec: &Table) -> mlua::Result<ToolRequest> {
    let name: String = spec.get("name")?;
    let args: String = spec.get("args")?;
    let arguments: Value = serde_json::from_str(&args)
        .map_err(|e| mlua::Error::RuntimeError(format!("host.tool: {e}")))?;
    let arguments = if matches!(&arguments, Value::Array(items) if items.is_empty()) {
        Value::Object(Map::new())
    } else {
        arguments
    };
    Ok(ToolRequest { name, arguments })
}

/// The inner call's outcome as the model would see it: `status`, `content`,
/// `artifact` when the loop cut the call, and `reason` or `error` when the
/// call did not complete. The script sees what the model sees: the artifact
/// path is part of that, while `details` and `changes` stay out. A field
/// `ToolCallCompleted` gains later never reaches the script unless this
/// names it.
pub(crate) fn outcome(completed: &ToolCallCompleted) -> Result<Value, String> {
    let fail = |e: serde_json::Error| format!("host.tool: {e}");
    let mut table = Map::new();
    let status = match completed.status {
        CallStatus::Completed => "completed",
        CallStatus::Failed => "failed",
        CallStatus::Denied => "denied",
        CallStatus::Cancelled => "cancelled",
    };
    table.insert("status".to_owned(), Value::String(status.to_owned()));
    table.insert(
        "content".to_owned(),
        serde_json::to_value(&completed.content).map_err(fail)?,
    );
    if let Some(artifact) = &completed.artifact {
        table.insert("artifact".to_owned(), Value::String(artifact.clone()));
    }
    match (&completed.reason, &completed.error) {
        (Some(reason), _) => {
            table.insert("reason".to_owned(), Value::String(reason.clone()));
        }
        (None, Some(error)) => {
            table.insert(
                "error".to_owned(),
                serde_json::to_value(error).map_err(fail)?,
            );
        }
        (None, None) => {}
    }
    Ok(Value::Object(table))
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
