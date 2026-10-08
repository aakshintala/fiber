//! `fiber.tool` (`docs/extensions.md`, "Registering"): a tool an extension
//! registers while its entry script runs, and the calls Fiber makes to it.
//! A tool's definition and a static `effects` table are read once, when it
//! registers, so no later Lua call changes what the model is sent
//! (`docs/prompt-cache.md`, "Tools").

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant};

use contract::clock::Wake;
use contract::inbox::Delivery;
use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::Cancel;
use mlua::{Function, Lua, Table, Value as LuaValue};
use serde_json::Value;

use crate::extension_tools::LuaTool;
use crate::{Error, host};

use super::hub::{Hub, Progress};
use super::{LuaExtension, Next, Phase, Shared, Target};

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
    /// it returned, under the tool's timeout, or `None` once `cancel`
    /// stopped it (`docs/tools.md`, "Cancellation"). It runs in the gaps of
    /// the extension's ordered stream, as a provider function does. A
    /// cancelled call that has not started never runs; one that suspends on
    /// a host call starts no host work and ends there; one parked on a host
    /// call is dropped, and one parked on `host.exec` returns only once the
    /// run has stopped; one running Lua is stopped by the deadline hook, or
    /// abandoned with the VM past its grace.
    pub(crate) fn tool_run(
        &self,
        name: &str,
        args: Value,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, Error> {
        let target = Target::Tool(name.to_owned());
        // When Fiber asks, before it waits for the hub (`docs/extensions.md`).
        let asked = self.hub.clock().now();
        // Subscribed before anything is queued, and the signal is read
        // after, so no cancel is missed.
        let wake: Arc<dyn Wake> = Arc::clone(&self.hub) as Arc<dyn Wake>;
        cancel.subscribe(Arc::downgrade(&wake));
        let mut shared = self.hub.lock();
        self.start(&mut shared)?;
        let id = shared.push(target.clone(), args, asked);
        self.hub.notify();
        let mut cancelling = false;
        loop {
            if !cancelling && cancel.is_cancelled() {
                cancelling = true;
                shared.cancel_call(id);
                self.hub.notify();
            }
            let now = self.hub.clock().now();
            let (judged, unsent) = shared.judge_tool(&self.name, id, &target, asked, now);
            let returned = match judged {
                Some(Next::Sleep(until)) => {
                    drop(unsent);
                    shared = self.hub.wait(shared, until);
                    continue;
                }
                Some(Next::Return(result)) => result.map(Some),
                None => Ok(None),
            };
            self.hub.notify();
            drop(shared);
            drop(unsent);
            return returned;
        }
    }
}

/// An admitted `host.exec` run: its stop sender, held until cancel, stop
/// or completion, so the stop reaches the exec thread without the Lua thread.
pub(super) struct ExecAdmit {
    stop: Option<std::sync::mpsc::Sender<()>>,
}

impl ExecAdmit {
    /// Drops the stop sender, so the stop reaches the exec thread without
    /// the Lua thread running.
    pub(super) fn stop_take(&mut self) {
        self.stop.take();
    }
}

impl Hub {
    /// Admits the host work a suspended callback of the call `id` asks for,
    /// and returns the lock the work starts under, so no stop or cancel lands
    /// between the check and the start. A stopped extension admits nothing,
    /// and `judge` ends its calls. A cancelled call admits nothing and ends
    /// here, its waiter woken: the thread drops the callback, so nothing
    /// else would end it.
    pub(super) fn admit(&self, id: u64) -> Option<MutexGuard<'_, Shared>> {
        let mut shared = self.lock();
        if !matches!(shared.phase, Phase::Ready(_)) {
            return None;
        }
        if shared.end_cancelled(id) {
            self.notify();
            return None;
        }
        Some(shared)
    }
}

/// What a tool call's caller does next: as [`Shared::judge`] says, or
/// `None` once the call was cancelled.
type Judged = (Option<Next>, Vec<Delivery>);

impl Shared {
    /// Cancels the call `id`: a queued call is dropped and ends cancelled; a
    /// started one is marked for the thread, and one running Lua is
    /// interrupted. One that ended keeps how it ended. Dropping an admitted
    /// exec's stop sender stops its run without the Lua thread.
    pub(super) fn cancel_call(&mut self, id: u64) {
        match self.calls.get(&id) {
            Some(Progress::Queued) => {
                self.queue.retain(|job| job.id != id);
                self.calls.insert(id, Progress::Cancelled);
            }
            Some(Progress::Started { parked, .. }) => {
                if !*parked {
                    self.interrupt.store(true, Ordering::SeqCst);
                }
                self.cancelled.insert(id);
                self.stop_exec(id);
            }
            Some(Progress::Done(_) | Progress::Cancelled) | None => {}
        }
    }

    /// Registers an exec run for the call `id`, admitted by [`Hub::admit`]
    /// under the same lock hold, and returns the stop receiver the run
    /// watches.
    pub(super) fn register_exec(&mut self, id: u64) -> std::sync::mpsc::Receiver<()> {
        let (stop, rx) = std::sync::mpsc::channel::<()>();
        self.execs.insert(id, ExecAdmit { stop: Some(stop) });
        rx
    }

    /// Stops the admitted exec run of the call `id`, if any: the sender's
    /// drop reaches the exec thread without the Lua thread running.
    pub(super) fn stop_exec(&mut self, id: u64) {
        if let Some(admit) = self.execs.get_mut(&id) {
            admit.stop.take();
        }
    }

    /// Ends the admitted exec run of the call `id`: its group is empty or
    /// killed and drained, so a cancelled call may return.
    pub(super) fn finish_exec(&mut self, id: u64) {
        self.execs.remove(&id);
    }

    /// Whether the call `id` has an admitted exec run still going: its
    /// group may still write, so a cancelled call must not return yet.
    pub(super) fn exec_pending(&self, id: u64) -> bool {
        self.execs.contains_key(&id)
    }

    /// Ends the call `id`, if it was cancelled, once its thread work has
    /// stopped. Returns whether it was.
    pub(super) fn end_cancelled(&mut self, id: u64) -> bool {
        if !self.cancelled.remove(&id) {
            return false;
        }
        if let Some(progress) = self.calls.get_mut(&id) {
            *progress = Progress::Cancelled;
        }
        true
    }

    /// Judges the tool call `id` as [`Shared::judge`] does, except that a
    /// cancelled call parked on a host call waits for the thread to stop
    /// it, past its deadline too; one parked on `host.exec` waits for the
    /// run's group to empty, even abandoned; and a cancelled call the
    /// extension's end fails ends cancelled once no admitted run is going.
    pub(super) fn judge_tool(
        &mut self,
        name: &str,
        id: u64,
        target: &Target,
        asked: Instant,
        now: Instant,
    ) -> Judged {
        // Before `judge`, which forgets a stopped call: an admitted run
        // still going may still write, so the call waits for its end.
        if self.exec_pending(id)
            && self.cancelled.contains(&id)
            && !matches!(self.phase, Phase::Ready(_))
        {
            return (Some(Next::Sleep(None)), Vec::new());
        }
        let cancelled = self.cancelled.contains(&id);
        match self.calls.get(&id) {
            Some(Progress::Cancelled) => {
                self.calls.remove(&id);
                return (None, Vec::new());
            }
            Some(Progress::Started { parked: true, .. })
                if cancelled && matches!(self.phase, Phase::Ready(_)) =>
            {
                return (Some(Next::Sleep(None)), Vec::new());
            }
            _ => {}
        }
        match self.judge(name, id, target, asked, now) {
            // The extension ended under a cancelled call: abandoned with
            // it, or stopped before the thread dropped it.
            (Next::Return(Err(_)), unsent)
                if cancelled && !matches!(self.phase, Phase::Ready(_)) =>
            {
                (None, unsent)
            }
            (next, unsent) => (Some(next), unsent),
        }
    }
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
