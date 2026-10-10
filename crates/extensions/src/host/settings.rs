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
    // A coded I/O failure is raised as the table; a wrong key, scope or
    // value as the string.
    {
        let session = session.clone();
        failure::register(
            lua,
            &table,
            "get",
            failure,
            None,
            move |lua, key: LuaValue| {
                let Some(session) = session.as_ref() else {
                    return Err(failure::Raise::Arg(no_session("get")));
                };
                let key = key_text("get", &key)?;
                let repo: Vec<&str> = session.repo_settings.iter().map(String::as_str).collect();
                match session
                    .config
                    .borrow()
                    .extensions()
                    .get(&session.extension, &repo, &key)
                {
                    Ok(Some(value)) => Ok(mlua::MultiValue::from_vec(vec![
                        to_lua(lua, &value).map_err(|e| failure::Raise::Arg(e.to_string()))?,
                    ])),
                    Ok(None) => Ok(mlua::MultiValue::from_vec(vec![LuaValue::Nil])),
                    Err(err) => Err(coded("get", &key, err)),
                }
            },
        )?;
    }
    {
        let session = session.clone();
        failure::register(
            lua,
            &table,
            "set",
            failure,
            None,
            move |_lua, (key, value, scope): (LuaValue, LuaValue, LuaValue)| {
                let Some(session) = session.as_ref() else {
                    return Err(failure::Raise::Arg(no_session("set")));
                };
                if matches!(value, LuaValue::Nil) {
                    return Err(failure::Raise::Arg(
                        "host.config.set: value must not be nil".to_owned(),
                    ));
                }
                let scope = if let LuaValue::String(s) = &scope {
                    s.as_bytes()
                } else {
                    return Err(failure::Raise::Arg(
                        "host.config.set: scope must be \"machine\" or \"project\"".to_owned(),
                    ));
                };
                let scope = match scope.as_ref() {
                    b"machine" => Scope::Machine,
                    b"project" => Scope::Project,
                    _ => {
                        return Err(failure::Raise::Arg(
                            "host.config.set: scope must be \"machine\" or \"project\"".to_owned(),
                        ));
                    }
                };
                let key = key_text("set", &key)?;
                let json = match to_json(&value) {
                    Ok(json) => json,
                    Err(err) => {
                        return Err(failure::Raise::Arg(format!(
                            "host.config.set: {}",
                            err.to_string().lines().next().unwrap_or_default()
                        )));
                    }
                };
                match session.config.borrow_mut().extensions_mut().set(
                    &session.extension,
                    scope,
                    &key,
                    json,
                ) {
                    Ok(()) => Ok(mlua::MultiValue::from_vec(vec![])),
                    Err(err) => Err(coded("set", &key, err)),
                }
            },
        )?;
    }
    host.set("config", table)?;
    Ok(())
}

fn no_session(op: &str) -> String {
    format!("host.config.{op}: this extension has no session")
}

/// A dotted key as text. Anything else is the key error, raised as a
/// string.
fn key_text(op: &str, key: &LuaValue) -> Result<String, failure::Raise> {
    let LuaValue::String(key) = key else {
        return Err(failure::Raise::Arg(format!(
            "host.config.{op}: key must be a string"
        )));
    };
    std::str::from_utf8(&key.as_bytes())
        .map(str::to_owned)
        .map_err(|_| {
            failure::Raise::Arg(format!(
                "host.config.{op}: `{}` is not a dotted key",
                String::from_utf8_lossy(&key.as_bytes())
            ))
        })
}

/// A failure of `extension_setting` or `set_extension_setting`: an
/// unparseable key, or one the layer may not take, is an error in the
/// calling code; anything the files did is the coded failure the call's Lua
/// half raises, under the registry's own code.
fn coded(op: &str, key: &str, err: ConfigError) -> failure::Raise {
    if matches!(err, ConfigError::Override { .. }) {
        failure::Raise::Arg(format!("host.config.{op}: `{key}` is not a dotted key"))
    } else if matches!(err, ConfigError::Refused { .. }) {
        failure::Raise::Arg(format!("host.config.{op}: {err}"))
    } else {
        let code = err.code();
        failure::Raise::Failed(code, format!("host.config.{op}: {err}"))
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
