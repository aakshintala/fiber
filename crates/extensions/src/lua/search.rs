//! `fiber.search_backend` (`docs/extensions.md`, "Registering"): a search
//! backend an extension registers while its entry script runs, and the
//! calls Fiber's own `web_search` makes to it. A backend's timeout is read
//! once, when it registers, so no later Lua call changes it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use contract::tool::Cancel;
use mlua::{Function, Lua, Table, Value as LuaValue};
use serde_json::{Value, json};

use super::{LuaExtension, Target};
use crate::Error;

/// One search backend the entry script registered: its timeout.
pub(super) type DeclaredBackend = std::time::Duration;

/// What `fiber.search_backend` registered: each backend's `run` function in
/// a Lua table by name, and its timeout.
pub(super) struct Registry {
    functions: Table,
    declared: Rc<RefCell<BTreeMap<String, DeclaredBackend>>>,
}

impl Registry {
    /// Every search backend registered, by name.
    pub(super) fn declared(&self) -> BTreeMap<String, DeclaredBackend> {
        self.declared.borrow().clone()
    }

    /// The backend `name`'s `run` function, if it has one.
    pub(super) fn function(&self, name: &str) -> mlua::Result<Option<Function>> {
        self.functions.get::<Option<Function>>(name)
    }
}

/// `fiber.search_backend` raises at the caller's line once the entry script
/// has returned, and registers otherwise.
const GUARD: &str = r#"
local register, in_entry = ...
fiber.search_backend = function(name, spec)
  if not in_entry() then
    error("fiber.search_backend: a backend registers only while `init.lua` runs", 2)
  end
  register(name, spec)
end
"#;

/// Sets `fiber.search_backend`. A refused registration leaves its reason in
/// `problems`, for Fiber's notice, and the entry script goes on.
pub(super) fn install(lua: &Lua, problems: Table, entry: Rc<Cell<bool>>) -> mlua::Result<Registry> {
    let functions = lua.create_table()?;
    let declared = Rc::new(RefCell::new(BTreeMap::new()));
    let registry = Registry {
        functions: functions.clone(),
        declared: Rc::clone(&declared),
    };
    let register = lua.create_function(move |_lua, (name, spec): (LuaValue, LuaValue)| {
        match read(&name, &spec) {
            Ok((name, timeout, run)) => {
                functions.set(name.as_str(), run)?;
                declared.borrow_mut().insert(name, timeout);
            }
            Err(why) => {
                let shown = name
                    .to_string()
                    .unwrap_or_else(|_| name.type_name().to_owned());
                problems.raw_push(format!("`{shown}` search backend not registered: {why}"))?;
            }
        }
        Ok(())
    })?;
    let in_entry = lua.create_function(move |_, ()| Ok(entry.get()))?;
    lua.load(GUARD)
        .set_name("=fiber.search_backend")
        .call::<()>((register, in_entry))?;
    Ok(registry)
}

/// Reads one `fiber.search_backend(name, spec)`, or says why it is refused.
fn read(
    name: &LuaValue,
    spec: &LuaValue,
) -> Result<(String, std::time::Duration, Function), String> {
    let LuaValue::String(name) = name else {
        return Err("the name must be a string".to_owned());
    };
    let name = name.to_str().map_err(|e| e.to_string())?.to_owned();
    if name.is_empty() {
        return Err("the name must not be empty".to_owned());
    }
    let LuaValue::Table(spec) = spec else {
        return Err("it takes a table of `timeout` and `run`".to_owned());
    };
    let field = |key: &str| spec.get::<LuaValue>(key).map_err(|e| e.to_string());
    let timeout = field("timeout")?;
    if timeout.is_nil() {
        return Err("missing `timeout`".to_owned());
    }
    let timeout = timeout
        .as_integer()
        .and_then(|ms| u64::try_from(ms).ok())
        .filter(|ms| *ms > 0)
        .map(std::time::Duration::from_millis)
        .ok_or_else(|| "`timeout` must be a whole number of milliseconds above 0".to_owned())?;
    let LuaValue::Function(run) = field("run")? else {
        return Err("`run` must be a function".to_owned());
    };
    Ok((name, timeout, run))
}

/// Checks a backend's `run` return on the Lua value, before any JSON
/// conversion, and returns the validated list: a list of `{ title, url,
/// snippet }` tables, every field a string and no other key. An empty Lua
/// table reads as the empty list.
pub(super) fn results(value: &LuaValue) -> Result<Value, String> {
    let LuaValue::Table(list) = value else {
        return Err("it must be a list of `{ title, url, snippet }`".to_owned());
    };
    let mut pairs: Vec<(LuaValue, LuaValue)> = Vec::new();
    for pair in list.pairs::<LuaValue, LuaValue>() {
        pairs.push(pair.map_err(|e| e.to_string())?);
    }
    let n = pairs.len();
    if n == 0 {
        return Ok(json!([]));
    }
    let index = |key: &LuaValue| {
        key.as_integer()
            .and_then(|i| usize::try_from(i).ok())
            .filter(|i| (1..=n).contains(i))
    };
    if !pairs.iter().all(|(key, _)| index(key).is_some()) {
        return Err("it must be a list of `{ title, url, snippet }`".to_owned());
    }
    let mut ordered: Vec<Option<LuaValue>> = vec![None; n];
    for (key, item) in pairs {
        if let Some(i) = index(&key)
            && let Some(slot) = ordered.get_mut(i - 1)
        {
            *slot = Some(item);
        }
    }
    let mut out = Vec::with_capacity(n);
    for (at, item) in ordered.into_iter().enumerate() {
        let position = at + 1;
        let Some(LuaValue::Table(entry)) = item else {
            return Err(format!("result {position} is not a table"));
        };
        out.push(entry_at(&entry, position)?);
    }
    Ok(Value::Array(out))
}

/// One validated result entry as JSON.
fn entry_at(entry: &Table, position: usize) -> Result<Value, String> {
    let mut title: Option<String> = None;
    let mut url: Option<String> = None;
    let mut snippet: Option<String> = None;
    for pair in entry.pairs::<LuaValue, LuaValue>() {
        let (key, item) = pair.map_err(|e| e.to_string())?;
        let LuaValue::String(key) = &key else {
            return Err(format!(
                "result {position} has a key that is not `title`, `url` or `snippet`"
            ));
        };
        let Ok(key) = key.to_str() else {
            return Err(format!(
                "result {position} has a key that is not `title`, `url` or `snippet`"
            ));
        };
        match key.as_ref() {
            "title" | "url" | "snippet" => {
                let LuaValue::String(text) = &item else {
                    return Err(format!("result {position} `{key}` is not a string"));
                };
                let text = text
                    .to_str()
                    .map_err(|_| format!("result {position} `{key}` is not a string"))?
                    .to_owned();
                match key.as_ref() {
                    "title" => title = Some(text),
                    "url" => url = Some(text),
                    _ => snippet = Some(text),
                }
            }
            _ => {
                return Err(format!(
                    "result {position} has `{key}`, which is not `title`, `url` or `snippet`"
                ));
            }
        }
    }
    let Some(title) = title else {
        return Err(format!("result {position} with no `title`"));
    };
    let Some(url) = url else {
        return Err(format!("result {position} with no `url`"));
    };
    let Some(snippet) = snippet else {
        return Err(format!("result {position} with no `snippet`"));
    };
    Ok(json!({"title": title, "url": url, "snippet": snippet}))
}

impl LuaExtension {
    /// Every search backend the entry script registered, sorted. Starts the
    /// extension (`timeouts.search` is a `BTreeMap`, so its keys read sorted).
    pub(crate) fn search_backends(&self) -> Result<Vec<String>, Error> {
        self.registered(|timeouts| timeouts.search.keys().cloned().collect())
    }

    /// Runs the `run` function of the backend `name` on `args` and returns
    /// what it returned, under the backend's timeout, or `None` once
    /// `cancel` stopped it. It runs in the gaps of the extension's ordered
    /// stream, as a tool does.
    pub(crate) fn search_run(
        &self,
        name: &str,
        args: Value,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, Error> {
        self.run_cancellable(Target::Search(name.to_owned()), args, cancel)
    }
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
