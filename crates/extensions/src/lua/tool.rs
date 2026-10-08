//! `fiber.tool` (`docs/extensions.md`, "Registering"): a tool an extension
//! registers while its entry script runs, and the calls Fiber makes to it.
//! A tool's definition and a static `effects` table are read once, when it
//! registers, so no later Lua call changes what the model is sent
//! (`docs/prompt-cache.md`, "Tools").

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use contract::shapes::{DeclaredEffects, Effect};
use mlua::{Function, Lua, Table, Value as LuaValue};
use serde_json::Value;

use crate::extension_tools::LuaTool;
use crate::{Error, host};

use super::{LuaExtension, Target};

/// The longest tool name a provider accepts (`docs/mcp.md`, "Tools and
/// their names").
const NAME_MAX: usize = 128;

/// One tool the entry script registered.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeclaredTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_schema: Value,
    /// The declared effects, or `None` when a function of the call's
    /// arguments returns them.
    pub(crate) effects: Option<DeclaredEffects>,
    pub(crate) timeout: Duration,
}

/// What `fiber.tool` registered: each tool's definition, and its `run` and
/// `effects` functions in a Lua table by name.
pub(super) struct Registry {
    functions: Table,
    declared: Rc<RefCell<BTreeMap<String, DeclaredTool>>>,
}

impl Registry {
    /// Every tool registered, by name.
    pub(super) fn declared(&self) -> BTreeMap<String, DeclaredTool> {
        self.declared.borrow().clone()
    }

    /// The tool `name`'s function `key` (`run` or `effects`), if it has one.
    pub(super) fn function(&self, name: &str, key: &str) -> mlua::Result<Option<Function>> {
        self.functions
            .get::<Option<Table>>(name)?
            .map_or(Ok(None), |table| table.get::<Option<Function>>(key))
    }
}

/// `fiber.tool` raises at the caller's line once the entry script has
/// returned, and registers otherwise.
const GUARD: &str = r#"
local register, in_entry = ...
fiber.tool = function(name, spec)
  if not in_entry() then
    error("fiber.tool: a tool registers only while `init.lua` runs", 2)
  end
  register(name, spec)
end
"#;

/// Sets `fiber.tool`. A refused registration leaves its reason in
/// `problems`, for Fiber's notice, and the entry script goes on. A call once
/// `entry` is false raises in the calling code: the session's tool set is
/// fixed by then.
pub(super) fn install(lua: &Lua, problems: Table, entry: Rc<Cell<bool>>) -> mlua::Result<Registry> {
    let functions = lua.create_table()?;
    let declared = Rc::new(RefCell::new(BTreeMap::new()));
    let registry = Registry {
        functions: functions.clone(),
        declared: Rc::clone(&declared),
    };
    let register = lua.create_function(move |lua, (name, spec): (LuaValue, LuaValue)| {
        match read(&name, &spec) {
            Ok((tool, run, effects)) => {
                let slot = lua.create_table()?;
                slot.set("run", run)?;
                slot.set("effects", effects)?;
                functions.set(tool.name.as_str(), slot)?;
                declared.borrow_mut().insert(tool.name.clone(), tool);
            }
            Err(why) => {
                let shown = name
                    .to_string()
                    .unwrap_or_else(|_| name.type_name().to_owned());
                problems.raw_push(format!("`{shown}` tool not registered: {why}"))?;
            }
        }
        Ok(())
    })?;
    let in_entry = lua.create_function(move |_, ()| Ok(entry.get()))?;
    lua.load(GUARD)
        .set_name("=fiber.tool")
        .call::<()>((register, in_entry))?;
    Ok(registry)
}

/// A registered tool: its definition, its `run`, and its `effects` when
/// that is a function.
type Functions = (DeclaredTool, Function, Option<Function>);

/// Reads one `fiber.tool(name, spec)`, or says why it is refused.
fn read(name: &LuaValue, spec: &LuaValue) -> Result<Functions, String> {
    let LuaValue::String(name) = name else {
        return Err("the name must be a string".to_owned());
    };
    let name = name.to_str().map_err(|e| e.to_string())?.to_owned();
    if !is_tool_name(&name) {
        return Err(format!(
            "a name is 1 to {NAME_MAX} of `A-Z`, `a-z`, `0-9`, `_` and `-`"
        ));
    }
    let LuaValue::Table(spec) = spec else {
        return Err(
            "it takes a table of `description`, `input_schema`, `effects`, `timeout` and `run`"
                .to_owned(),
        );
    };
    let field = |key: &str| spec.get::<LuaValue>(key).map_err(|e| e.to_string());
    let description = field("description")?
        .as_string()
        .and_then(|text| text.to_str().ok().map(|text| text.to_owned()))
        .filter(|text| !text.is_empty())
        .ok_or_else(|| "`description` must be a non-empty string".to_owned())?;
    let input_schema = schema(&field("input_schema")?)?;
    let effects = field("effects")?;
    if effects.is_nil() {
        return Err("missing `effects`".to_owned());
    }
    let (effects, effects_fn) = if let LuaValue::Function(f) = effects {
        (None, Some(f))
    } else if effects.is_table() {
        let value = host::to_json(&effects).map_err(|e| format!("`effects`: {e}"))?;
        let declared = effects_from(&value).map_err(|why| format!("`effects`: {why}"))?;
        (Some(declared), None)
    } else {
        return Err("`effects` must be a table or a function".to_owned());
    };
    let timeout = field("timeout")?;
    if timeout.is_nil() {
        return Err("missing `timeout`".to_owned());
    }
    let timeout = timeout
        .as_integer()
        .and_then(|ms| u64::try_from(ms).ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .ok_or_else(|| "`timeout` must be a whole number of milliseconds above 0".to_owned())?;
    let LuaValue::Function(run) = field("run")? else {
        return Err("`run` must be a function".to_owned());
    };
    Ok((
        DeclaredTool {
            name,
            description,
            input_schema,
            effects,
            timeout,
        },
        run,
        effects_fn,
    ))
}

/// Whether `name` is 1 to 128 of `A-Z a-z 0-9 _ -`.
fn is_tool_name(name: &str) -> bool {
    (1..=NAME_MAX).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A tool's `input_schema`: a table that reads as a JSON object whose `type`
/// is `object`.
fn schema(value: &LuaValue) -> Result<Value, String> {
    let schema = host::to_json(value).map_err(|e| format!("`input_schema`: {e}"))?;
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err("`input_schema` must be a JSON object whose `type` is `object`".to_owned());
    }
    Ok(schema)
}

/// A call's effects as `docs/permissions.md`, "Effects", declares them:
/// `effects`, a list of `reads`, `writes`, `executes` and `network`, kept in
/// the order each first appears; `reversible`; and `paths`, absent or a
/// list of strings. Any other key, a wrong type or an unknown effect is
/// refused.
pub(crate) fn effects_from(value: &Value) -> Result<DeclaredEffects, String> {
    let Value::Object(map) = value else {
        return Err("it must be a table of `effects`, `paths` and `reversible`".to_owned());
    };
    if let Some(key) = map
        .keys()
        .find(|key| !matches!(key.as_str(), "effects" | "paths" | "reversible"))
    {
        return Err(format!("`{key}` is not `effects`, `paths` or `reversible`"));
    }
    let list = match map.get("effects") {
        None => return Err("missing `effects`".to_owned()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("`effects` must be a list".to_owned()),
    };
    let mut effects = Vec::new();
    for item in list {
        let effect = match item.as_str() {
            Some("reads") => Effect::Reads,
            Some("writes") => Effect::Writes,
            Some("executes") => Effect::Executes,
            Some("network") => Effect::Network,
            _ => {
                return Err(format!(
                    "{item} is not `reads`, `writes`, `executes` or `network`"
                ));
            }
        };
        if !effects.contains(&effect) {
            effects.push(effect);
        }
    }
    let reversible = match map.get("reversible") {
        None => return Err("missing `reversible`".to_owned()),
        Some(Value::Bool(reversible)) => *reversible,
        Some(_) => return Err("`reversible` must be true or false".to_owned()),
    };
    let paths = match map.get("paths") {
        None => None,
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| "`paths` must be a list of strings".to_owned())?,
        ),
        Some(_) => return Err("`paths` must be a list of strings".to_owned()),
    };
    Ok(DeclaredEffects {
        effects,
        reversible,
        paths,
    })
}

impl LuaExtension {
    /// Every tool the entry script registered, by name. Starts the
    /// extension.
    pub fn tools(self: &Arc<Self>) -> Result<Vec<LuaTool>, Error> {
        let declared = self.registered(|timeouts| timeouts.tools.clone())?;
        Ok(declared
            .into_values()
            .map(|tool| LuaTool::new(Arc::clone(self), tool))
            .collect())
    }

    /// Runs the `effects` function of the tool `name` on `args` and returns
    /// what it returned, under the tool's timeout.
    pub(crate) fn tool_effects(&self, name: &str, args: Value) -> Result<Value, Error> {
        self.call(Target::Effects(name.to_owned()), args)
    }

    /// Runs the `run` function of the tool `name` on `args` and returns what
    /// it returned, under the tool's timeout. It runs in the gaps of the
    /// extension's ordered stream, as a provider function does.
    pub(crate) fn tool_run(&self, name: &str, args: Value) -> Result<Value, Error> {
        self.call(Target::Tool(name.to_owned()), args)
    }
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
