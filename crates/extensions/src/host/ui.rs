//! `host.status`, `host.widget` and `host.emit` (`docs/extensions.md`,
//! "Commands and screens"): one line of status, a named block of lines, and
//! a message to the extension's own TUI half. They are ephemeral and go
//! straight to the log through the late-bound emitter, so they show during a
//! model stream. None suspends, so all three work in `init.lua`: lines
//! emitted before `emit_to` are buffered in call order and flushed when it
//! is set. A wrong argument is an error in the calling code and raises a
//! string, as `host.log` does.

use std::sync::Arc;

use contract::events::{Event, ExtensionMessage, ExtensionUi, Ui};
use mlua::{Lua, Table, Value as LuaValue};

use crate::lua::Hub;

/// Sets `host.status`, `host.widget` and `host.emit` on `host`.
pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    hub: &Arc<Hub>,
    extension: &str,
) -> mlua::Result<()> {
    let hub_status = Arc::clone(hub);
    let ext_status = extension.to_owned();
    let status = lua.create_function(move |_, text: String| {
        hub_status.emit(Event::ExtensionUi(ExtensionUi {
            extension: ext_status.clone(),
            ui: Ui::Status { status: text },
        }));
        Ok(())
    })?;
    let hub_widget = Arc::clone(hub);
    let ext_widget = extension.to_owned();
    let widget = lua.create_function(move |_, (id, lines): (String, Vec<String>)| {
        hub_widget.emit(Event::ExtensionUi(ExtensionUi {
            extension: ext_widget.clone(),
            ui: Ui::Widget { widget: id, lines },
        }));
        Ok(())
    })?;
    let hub_emit = Arc::clone(hub);
    let ext_emit = extension.to_owned();
    let emit = lua.create_function(move |lua, value: LuaValue| {
        let data = crate::host::to_json(&value)
            .map_err(|e| mlua::Error::RuntimeError(format!("host.emit: {e}")))?;
        // `to_json` runs on the caller's values; drop the borrow before emit.
        let _ = lua;
        hub_emit.emit(Event::ExtensionMessage(ExtensionMessage {
            extension: ext_emit.clone(),
            data,
        }));
        Ok(())
    })?;
    lua.load(STATUS)
        .set_name("=host.status")
        .call::<()>((host.clone(), status))?;
    lua.load(WIDGET)
        .set_name("=host.widget")
        .call::<()>((host.clone(), widget))?;
    lua.load(EMIT)
        .set_name("=host.emit")
        .call::<()>((host.clone(), emit))
}

const STATUS: &str = r#"
local host, impl = ...
function host.status(text)
  if type(text) ~= "string" then
    error("host.status: `text` must be a string", 2)
  end
  impl(text)
end
"#;

const WIDGET: &str = r#"
local host, impl = ...
function host.widget(id, lines)
  if type(id) ~= "string" then
    error("host.widget: `id` must be a string", 2)
  end
  if type(lines) ~= "table" then
    error("host.widget: `lines` must be a list of strings", 2)
  end
  local checked = {}
  for i = 1, #lines do
    if type(lines[i]) ~= "string" then
      error("host.widget: `lines` must be a list of strings", 2)
    end
    checked[i] = lines[i]
  end
  impl(id, checked)
end
"#;

const EMIT: &str = r#"
local host, impl = ...
function host.emit(data)
  impl(data)
end
"#;

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
