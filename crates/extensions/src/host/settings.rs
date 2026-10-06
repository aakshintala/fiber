//! `host.config` (`docs/extensions.md`, "Host calls"): the extension's own
//! settings. `get` reads the merged value across layers; `set` writes the
//! machine or the project file under Fiber home's lock.

use std::cell::RefCell;
use std::rc::Rc;

use config::{Config, ConfigError, Scope};
use mlua::{Lua, LuaString, Table, Value as LuaValue};

use super::{to_json, to_lua};

/// What `host.config` reads and writes: the extension's name, the settings
/// keys a repository may set, and its own clone of the session's
/// configuration, so a `set` is visible to its own later `get` at once.
#[derive(Clone)]
pub(crate) struct Settings {
    pub extension: String,
    pub repo_settings: Vec<String>,
    pub config: Rc<RefCell<Config>>,
}

/// Installs `host.config` on `host`. Without a session both calls raise:
/// there is no configuration to read and no file to write.
pub(crate) fn install(lua: &Lua, host: &Table, session: Option<Settings>) -> mlua::Result<()> {
    let table = lua.create_table()?;
    {
        let session = session.clone();
        table.set(
            "get",
            lua.create_function(move |lua, key: LuaString| {
                let Some(session) = session.as_ref() else {
                    return Err(no_session("get"));
                };
                let key = key_text("get", &key)?;
                let repo: Vec<&str> = session.repo_settings.iter().map(String::as_str).collect();
                let value = session
                    .config
                    .borrow()
                    .extension_setting(&session.extension, &repo, &key)
                    .map_err(|err| key_error("get", &key, err))?;
                match value {
                    Some(value) => to_lua(lua, &value),
                    None => Ok(LuaValue::Nil),
                }
            })?,
        )?;
    }
    {
        let session = session.clone();
        table.set(
            "set",
            lua.create_function(
                move |_, (key, value, scope): (LuaString, LuaValue, LuaValue)| {
                    let Some(session) = session.as_ref() else {
                        return Err(no_session("set"));
                    };
                    if matches!(value, LuaValue::Nil) {
                        return Err(runtime("host.config.set: value must not be nil".into()));
                    }
                    let scope = match lua_scope(&scope)?.as_slice() {
                        b"machine" => Scope::Machine,
                        b"project" => Scope::Project,
                        _ => {
                            return Err(runtime(
                                "host.config.set: scope must be \"machine\" or \"project\"".into(),
                            ));
                        }
                    };
                    let key = key_text("set", &key)?;
                    let json = to_json(&value)
                        .map_err(|err| runtime(format!("host.config.set: {}", message(&err))))?;
                    session
                        .config
                        .borrow_mut()
                        .set_extension_setting(&session.extension, scope, &key, json)
                        .map_err(|err| key_error("set", &key, err))?;
                    Ok(())
                },
            )?,
        )?;
    }
    host.set("config", table)?;
    Ok(())
}

fn runtime(message: String) -> mlua::Error {
    mlua::Error::RuntimeError(message)
}

fn no_session(op: &str) -> mlua::Error {
    runtime(format!("host.config.{op}: this extension has no session"))
}

/// A dotted key as text. Anything else is the key error.
fn key_text(op: &str, key: &LuaString) -> Result<String, mlua::Error> {
    std::str::from_utf8(&key.as_bytes())
        .map(str::to_owned)
        .map_err(|_| {
            runtime(format!(
                "host.config.{op}: `{}` is not a dotted key",
                String::from_utf8_lossy(&key.as_bytes())
            ))
        })
}

/// A scope argument as bytes; anything else is the scope error.
fn lua_scope(scope: &LuaValue) -> mlua::Result<Vec<u8>> {
    if let LuaValue::String(s) = scope {
        Ok(s.as_bytes().to_vec())
    } else {
        Err(runtime(
            "host.config.set: scope must be \"machine\" or \"project\"".into(),
        ))
    }
}

/// A failure of `extension_setting` or `set_extension_setting`: an
/// unparseable key names the key, anything else names its file.
fn key_error(op: &str, key: &str, err: ConfigError) -> mlua::Error {
    if matches!(err, ConfigError::Override { .. }) {
        runtime(format!("host.config.{op}: `{key}` is not a dotted key"))
    } else {
        runtime(format!("host.config.{op}: {err}"))
    }
}

/// The message a person reads for a Lua error: its first line.
fn message(err: &mlua::Error) -> String {
    err.to_string()
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
