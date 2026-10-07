//! `host.log` (`docs/extensions.md`, "Host calls"): a diagnostic line an
//! extension writes. It never suspends the caller, so it works in the
//! entry script too: the hub buffers the line until `deliver_to`, in call
//! order with the extension's other deliveries.

use std::sync::Arc;

use contract::events::ExtensionLog;
use contract::inbox::Delivery;
use mlua::{Lua, LuaString, Table};

use crate::lua::Hub;

/// `host.log` checks its argument in Lua, so a number is coerced exactly
/// as Lua's string coercion does it. The Rust half records the bytes with
/// lossy UTF-8, so bytes that are not UTF-8 arrive without panic.
const LOG: &str = r#"
local host, impl = ...
function host.log(msg)
  if type(msg) == "number" then msg = tostring(msg) end
  if type(msg) ~= "string" then
    error("host.log: the message must be a string", 2)
  end
  impl(msg)
end
"#;

/// Sets `host.log` on `host`: one live `extension_log` delivery per call,
/// carrying `extension`'s name with the message as written.
pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    hub: &Arc<Hub>,
    extension: &str,
) -> mlua::Result<()> {
    let hub = Arc::clone(hub);
    let extension = extension.to_owned();
    let send = lua.create_function(move |_, msg: LuaString| {
        let message = String::from_utf8_lossy(&msg.as_bytes()).into_owned();
        hub.send(Delivery::ExtensionLog(ExtensionLog {
            extension: extension.clone(),
            message,
        }));
        Ok(())
    })?;
    lua.load(LOG)
        .set_name("=host.log")
        .call::<()>((host.clone(), send))
}

#[cfg(test)]
#[path = "log_tests.rs"]
mod tests;
