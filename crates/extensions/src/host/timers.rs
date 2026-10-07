//! `host.after` and `host.every` (`docs/extensions.md`, "Host calls"): timers
//! that fire in the gaps of the session's stream, on the extension's
//! injected clock. The Lua half validates and registers; the scheduler
//! (`lua/schedule.rs`) starts a due timer only when no queued call can
//! start, under its own timeout from its firing's start.

use std::sync::Arc;
use std::time::Duration;

use mlua::{Lua, MultiValue, Table, Value as LuaValue};

use crate::lua::Hub;

/// Installs `host.after` and `host.every` on `host`. Returns the table of
/// timer functions by id, which each firing runs. `hub` carries the
/// extension's clock and the timer list the scheduler reads; both the due
/// check and `:cancel()` run on the extension's thread, so a cancel and a
/// due check never interleave.
pub(crate) fn install(lua: &Lua, host: &Table, hub: &Arc<Hub>) -> mlua::Result<Table> {
    let funcs = lua.create_table()?;
    for (short, call, every) in [
        ("after", "host.after", false),
        ("every", "host.every", true),
    ] {
        let hub = Arc::clone(hub);
        let funcs = funcs.clone();
        host.set(
            short,
            lua.create_function(move |lua, args: MultiValue| {
                register(lua, &hub, &funcs, call, every, args)
            })?,
        )?;
    }
    Ok(funcs)
}

/// Registers one timer: `ms` whole milliseconds at or above 0, `func` its
/// callback, and a required `timeout` whole milliseconds above 0. Due at set
/// time plus `ms`, on the extension's clock. Returns a handle table whose
/// `:cancel()` stops it; cancelling twice, or a fired `after`, is a no-op.
fn register(
    lua: &Lua,
    hub: &Arc<Hub>,
    funcs: &Table,
    call: &'static str,
    every: bool,
    args: MultiValue,
) -> mlua::Result<Table> {
    let fail = |why: String| mlua::Error::RuntimeError(format!("{call}: {why}"));
    // A missing argument is `None` below, which each check refuses.
    let args = args.into_vec();
    let Some(LuaValue::Integer(ms)) = args.first() else {
        return Err(fail(
            "`ms` must be a whole number of milliseconds, 0 or above".into(),
        ));
    };
    if *ms < 0 {
        return Err(fail(
            "`ms` must be a whole number of milliseconds, 0 or above".into(),
        ));
    }
    let Some(LuaValue::Function(func)) = args.get(1) else {
        return Err(fail("`fn` must be a function".into()));
    };
    let timeout = match args.get(2) {
        Some(LuaValue::Table(opts)) => {
            let LuaValue::Integer(ms) = opts.get::<LuaValue>("timeout")? else {
                return Err(fail(
                    "`timeout` must be a whole number of milliseconds above 0".into(),
                ));
            };
            if ms <= 0 {
                return Err(fail(
                    "`timeout` must be a whole number of milliseconds above 0".into(),
                ));
            }
            Duration::from_millis(u64::try_from(ms).unwrap_or(u64::MAX))
        }
        Some(_) | None => {
            return Err(fail(
                "`timeout` must be a whole number of milliseconds above 0".into(),
            ));
        }
    };
    let delay = Duration::from_millis(u64::try_from(*ms).unwrap_or(u64::MAX));
    let now = hub.clock().now();
    let due = now.checked_add(delay).unwrap_or(now);
    let id = hub.add_timer(every.then_some(delay), due, timeout);
    funcs.set(i64::try_from(id).unwrap_or(i64::MAX), func.clone())?;
    let handle = lua.create_table()?;
    let hub = Arc::clone(hub);
    let funcs = funcs.clone();
    handle.set(
        "cancel",
        lua.create_function(move |_, _: MultiValue| {
            hub.cancel_timer(id);
            funcs.set(i64::try_from(id).unwrap_or(i64::MAX), LuaValue::Nil)?;
            Ok(())
        })?,
    )?;
    Ok(handle)
}

#[cfg(test)]
#[path = "timers_tests.rs"]
mod tests;
