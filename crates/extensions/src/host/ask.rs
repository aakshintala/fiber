//! `host.ask` (`docs/extensions.md`, "Commands and screens"): raises one of
//! the closed interactions and waits for its answer. It suspends the
//! callback like `host.http`: the loop writes the question at its drain
//! and resumes the callback with the answer. With nobody to answer, as in
//! a session started by `fiber ask`, it returns `{ declined = true }` at
//! once. A bad `kind` or `spec` is an error in the calling code and raises
//! a string naming the key or label, which `pcall` catches. Refused in the
//! entry script before it yields, like `host.exec`.

use std::cell::Cell;
use std::rc::Rc;

use mlua::{Lua, Table, Value as LuaValue};

/// `host.ask` yields this tag, `"ask"` and the call's kind with its spec.
/// The extension's thread raises the interaction and resumes the coroutine
/// with the answer (`docs/extensions.md`, "A host call suspends the code
/// that made it"). Refused in the entry script before it yields, like
/// `host.exec`.
const ASK: &str = r#"
local host, tag, in_entry, check = ...
function host.ask(kind, spec)
  if in_entry() then
    error("host.ask: not available while init.lua runs", 0)
  end
  local ok, message = check(kind, spec)
  if not ok then error(message, 2) end
  return coroutine.yield(tag, "ask", { kind = kind, spec = spec })
end
"#;

/// Sets `host.ask` on `host`. `entry` is true while the entry script runs.
pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    tag: &Table,
    entry: Rc<Cell<bool>>,
) -> mlua::Result<()> {
    let in_entry = crate::host::failure::in_entry(lua, &entry)?;
    let check = lua.create_function(|_, (kind, spec): (LuaValue, LuaValue)| {
        // `true`, or `false` with the string the Lua half raises, so
        // `pcall` catches a string, never a table.
        let checked: (bool, Option<String>) = check_spec(&kind, &spec);
        Ok(checked)
    })?;
    lua.load(ASK)
        .set_name("=host.ask")
        .call::<()>((host.clone(), tag.clone(), in_entry, check))
}

/// Checks `kind` with its `spec`: `true`, or `false` with the string the
/// Lua half raises. Built on [`interaction`](crate::lua::asks::interaction).
fn check_spec(kind: &LuaValue, spec: &LuaValue) -> (bool, Option<String>) {
    let fail = |message: String| (false, Some(message));
    let LuaValue::String(kind) = kind else {
        return fail("host.ask: kind must be a string".to_owned());
    };
    let kind = match kind.to_str() {
        Ok(kind) => kind.to_owned(),
        Err(_) => return fail("host.ask: kind must be a string".to_owned()),
    };
    // An empty table is an empty spec: `to_json` would read it as an
    // empty list, so it is an empty object here and `interaction`
    // names the missing key.
    let empty = matches!(spec, LuaValue::Table(table) if table.pairs::<LuaValue, LuaValue>().next().is_none());
    let spec = match (empty, super::to_json(spec)) {
        (true, _) => serde_json::Value::Object(serde_json::Map::new()),
        (false, Ok(spec)) => spec,
        (false, Err(_)) => return fail("host.ask: spec must be a table".to_owned()),
    };
    match crate::lua::asks::interaction(&kind, &spec) {
        Ok(_) => (true, None),
        Err(message) => fail(message),
    }
}

#[cfg(test)]
#[path = "ask_tests.rs"]
mod tests;
