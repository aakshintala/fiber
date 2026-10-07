//! `host.drive` (`docs/extensions.md`, "Host calls"): sends one driver
//! command from inside the session, as the extension. It suspends the
//! callback like `host.http`: the door runs the command and resumes the
//! callback with its answer. `command_accepted` returns its `result` as a
//! Lua table, or `true` when it has none; `command_rejected` raises the
//! rejection's `code` and `message` as the failure table, which `pcall`
//! catches. A wrong argument is an error in the calling code and raises a
//! string; in the entry script it raises `closing`, since `drive_to` is set
//! only after the entry script runs.

use std::cell::Cell;
use std::rc::Rc;

use mlua::{Function, Lua, Table};

/// `host.drive` yields this tag, `"drive"` and the call's spec. The
/// extension's thread sends the command through the session's door and
/// resumes the coroutine with the reply (`docs/extensions.md`, "A host call
/// suspends the code that made it"). Refused in the entry script before it
/// yields, like `host.exec`.
const DRIVE: &str = r#"
local host, tag, in_entry, failure = ...
function host.drive(command, args)
  if in_entry() then
    error(failure("closing", "host.drive: not available while init.lua runs"), 0)
  end
  if type(command) ~= "string" then
    error("host.drive: `command` must be a string", 2)
  end
  if args == nil then args = {} end
  if type(args) ~= "table" then
    error("host.drive: `args` must be a table", 2)
  end
  local result, code, message = coroutine.yield(tag, "drive", { command = command, args = args })
  if result == nil then error(failure(code, message), 0) end
  return result
end
"#;

/// Sets `host.drive` on `host`. `entry` is true while the entry script runs.
pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    tag: &Table,
    entry: Rc<Cell<bool>>,
    failure: Function,
) -> mlua::Result<()> {
    let in_entry = lua.create_function(move |_, ()| Ok(entry.get()))?;
    lua.load(DRIVE).set_name("=host.drive").call::<()>((
        host.clone(),
        tag.clone(),
        in_entry,
        failure.clone(),
    ))
}

#[cfg(test)]
#[path = "drive_tests.rs"]
mod tests;
