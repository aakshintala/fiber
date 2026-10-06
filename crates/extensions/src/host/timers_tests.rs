//! The Lua half of `host.after` and `host.every` and the timer table the
//! scheduler reads (`docs/extensions.md`, "Host calls").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::Arc;
use std::time::Duration;

use contract::clock::Clock as _;
use fakes::clock::FakeClock;
use mlua::{Lua, StdLib, Value as LuaValue};

use super::install;
use crate::lua::Hub;

fn installed() -> (Lua, Arc<Hub>, Arc<FakeClock>) {
    let lua = Lua::new_with(
        StdLib::ALL_SAFE & !StdLib::IO & !StdLib::OS & !StdLib::PACKAGE,
        mlua::LuaOptions::new().catch_rust_panics(false),
    )
    .unwrap();
    let clock = FakeClock::new();
    let hub = Hub::new(clock.clone());
    let host = lua.create_table().unwrap();
    install(&lua, &host, &hub).unwrap();
    lua.globals().set("host", host).unwrap();
    (lua, hub, clock)
}

/// Calls `host.<call>` with `args` and returns the error it raises.
fn raised(lua: &Lua, call: &str, args: &str) -> String {
    let result: mlua::Result<LuaValue> = lua.load(format!("return host.{call}({args})")).call(());
    let err = result.expect_err("the call raises");
    err.to_string()
}

#[test]
fn after_needs_a_whole_ms_at_or_above_0() {
    let (lua, hub, _clock) = installed();
    for args in [
        "\"10\", function() end, {timeout = 100}",
        "-1, function() end, {timeout = 100}",
        "1.5, function() end, {timeout = 100}",
    ] {
        let message = raised(&lua, "after", args);
        assert!(
            message.contains("host.after: `ms` must be a whole number of milliseconds, 0 or above"),
            "{message}"
        );
    }
    assert!(hub.lock().timers.is_empty(), "nothing registered");
}

#[test]
fn after_needs_a_function() {
    let (lua, _, _) = installed();
    let message = raised(&lua, "after", "10, 42, {timeout = 100}");
    assert!(
        message.contains("host.after: `fn` must be a function"),
        "{message}"
    );
}

#[test]
fn after_needs_a_whole_timeout_above_0() {
    let (lua, hub, _clock) = installed();
    for (call, args) in [
        ("after", "10, function() end"),
        ("after", "10, function() end, {}"),
        ("after", "10, function() end, {timeout = 0}"),
        ("after", "10, function() end, {timeout = -5}"),
        ("after", "10, function() end, {timeout = 1.5}"),
        ("every", "10, function() end, {timeout = 0}"),
    ] {
        let message = raised(&lua, call, args);
        assert!(
            message.contains(&format!(
                "host.{call}: `timeout` must be a whole number of milliseconds above 0"
            )),
            "{message}"
        );
    }
    assert!(hub.lock().timers.is_empty(), "nothing registered");
}

#[test]
fn after_registers_due_at_set_plus_ms() {
    let (lua, hub, clock) = installed();
    lua.load("h = host.after(50, function() end, {timeout = 100})")
        .exec()
        .unwrap();
    let shared = hub.lock();
    assert_eq!(shared.timers.len(), 1);
    let timer = shared.timers.values().next().unwrap();
    assert_eq!(timer.every, None, "an `after` fires once");
    assert_eq!(timer.timeout, Duration::from_millis(100));
    assert_eq!(timer.due, clock.now() + Duration::from_millis(50));
    assert!(!timer.cancelled);
    assert!(!timer.firing);
}

#[test]
fn every_registers_its_period() {
    let (lua, hub, _clock) = installed();
    lua.load("h = host.every(25, function() end, {timeout = 100})")
        .exec()
        .unwrap();
    let timer = hub.lock().timers.values().next().unwrap().id;
    assert_eq!(
        hub.lock().timers.get(&timer).unwrap().every,
        Some(Duration::from_millis(25))
    );
}

#[test]
fn cancel_before_the_due_time_stops_it_and_twice_is_a_no_op() {
    let (lua, hub, _clock) = installed();
    lua.load("h = host.after(50, function() end, {timeout = 100})")
        .exec()
        .unwrap();
    lua.load("h:cancel()").exec().unwrap();
    assert!(hub.lock().timers.is_empty(), "the timer is gone");
    lua.load("h:cancel()").exec().unwrap();
}
