//! `host.config` (`docs/extensions.md`, "Host calls"): the extension's own
//! settings. `get` reads the merged value across layers; `set` writes the
//! machine or the project file under Fiber home's lock.

use std::cell::RefCell;
use std::rc::Rc;

use config::{Config, ConfigError, Scope};
use mlua::{Lua, Table, Value as LuaValue};

use super::{failure, to_json, to_lua};

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
/// there is no configuration to read and no file to write. `failure` is
/// the `{ code, message }` constructor the Lua halves raise.
pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    extension: &str,
    session: Option<super::Session>,
    failure: &mlua::Function,
) -> mlua::Result<()> {
    let session = session.map(|session| Settings {
        extension: extension.to_owned(),
        repo_settings: session.repo_settings,
        config: Rc::new(RefCell::new(session.config)),
    });
    let table = lua.create_table()?;
    // A coded I/O failure returns `(nil, code, message)` for the Lua half
    // to raise as the table; a wrong key, scope or value returns
    // `(nil, message)` for it to raise as the string.
    {
        let session = session.clone();
        let raw = lua.create_function(move |lua, key: LuaValue| {
            let Some(session) = session.as_ref() else {
                return failure::raw_string(lua, no_session("get"));
            };
            let key = match key_text("get", &key) {
                Ok(key) => key,
                Err(message) => return failure::raw_string(lua, message),
            };
            let repo: Vec<&str> = session.repo_settings.iter().map(String::as_str).collect();
            match session
                .config
                .borrow()
                .extensions()
                .get(&session.extension, &repo, &key)
            {
                Ok(Some(value)) => Ok(mlua::MultiValue::from_vec(vec![to_lua(lua, &value)?])),
                Ok(None) => Ok(mlua::MultiValue::from_vec(vec![LuaValue::Nil])),
                Err(err) => match coded("get", &key, err) {
                    Ok(message) => failure::raw_string(lua, message),
                    Err((code, message)) => failure::raw_failure(lua, &code, message),
                },
            }
        })?;
        table.set("get", failure::wrap(lua, raw, failure)?)?;
    }
    {
        let session = session.clone();
        let raw = lua.create_function(
            move |lua, (key, value, scope): (LuaValue, LuaValue, LuaValue)| {
                let Some(session) = session.as_ref() else {
                    return failure::raw_string(lua, no_session("set"));
                };
                if matches!(value, LuaValue::Nil) {
                    return failure::raw_string(
                        lua,
                        "host.config.set: value must not be nil".to_owned(),
                    );
                }
                let scope = if let LuaValue::String(s) = &scope {
                    s.as_bytes()
                } else {
                    return failure::raw_string(
                        lua,
                        "host.config.set: scope must be \"machine\" or \"project\"".to_owned(),
                    );
                };
                let scope = match scope.as_ref() {
                    b"machine" => Scope::Machine,
                    b"project" => Scope::Project,
                    _ => {
                        return failure::raw_string(
                            lua,
                            "host.config.set: scope must be \"machine\" or \"project\"".to_owned(),
                        );
                    }
                };
                let key = match key_text("set", &key) {
                    Ok(key) => key,
                    Err(message) => return failure::raw_string(lua, message),
                };
                let json = match to_json(&value) {
                    Ok(json) => json,
                    Err(err) => {
                        return failure::raw_string(
                            lua,
                            format!(
                                "host.config.set: {}",
                                err.to_string().lines().next().unwrap_or_default()
                            ),
                        );
                    }
                };
                match session.config.borrow_mut().extensions_mut().set(
                    &session.extension,
                    scope,
                    &key,
                    json,
                ) {
                    Ok(()) => Ok(mlua::MultiValue::from_vec(vec![])),
                    Err(err) => match coded("set", &key, err) {
                        Ok(message) => failure::raw_string(lua, message),
                        Err((code, message)) => failure::raw_failure(lua, &code, message),
                    },
                }
            },
        )?;
        table.set("set", failure::wrap(lua, raw, failure)?)?;
    }
    host.set("config", table)?;
    Ok(())
}

fn no_session(op: &str) -> String {
    format!("host.config.{op}: this extension has no session")
}

/// A dotted key as text. Anything else is the key error, as the message to
/// raise as a string.
fn key_text(op: &str, key: &LuaValue) -> Result<String, String> {
    let LuaValue::String(key) = key else {
        return Err(format!("host.config.{op}: key must be a string"));
    };
    std::str::from_utf8(&key.as_bytes())
        .map(str::to_owned)
        .map_err(|_| {
            format!(
                "host.config.{op}: `{}` is not a dotted key",
                String::from_utf8_lossy(&key.as_bytes())
            )
        })
}

/// A failure of `extension_setting` or `set_extension_setting`: an
/// unparseable key, or one the layer may not take, is an error in the
/// calling code; anything the files did is the coded failure the call's Lua
/// half raises, under the registry's own code.
fn coded(op: &str, key: &str, err: ConfigError) -> Result<String, (contract::ErrorCode, String)> {
    if matches!(err, ConfigError::Override { .. }) {
        Ok(format!("host.config.{op}: `{key}` is not a dotted key"))
    } else if matches!(err, ConfigError::Refused { .. }) {
        Ok(format!("host.config.{op}: {err}"))
    } else {
        let code = err.code();
        Err((code, format!("host.config.{op}: {err}")))
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
