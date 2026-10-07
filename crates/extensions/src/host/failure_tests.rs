//! The `{ code, message }` constructor, the `pcall` converter and the
//! prelude's yield-forwarding `pcall`/`xpcall`
//! (`docs/extensions.md`, "How an extension runs").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use contract::ErrorCode;
use mlua::{Lua, LuaSerdeExt, Value as LuaValue};

use super::{as_failure, code_name, fail, install};
use crate::lua::Deadline;

fn lua() -> Lua {
    Lua::new()
}

fn lib(lua: &Lua) -> super::FailureLib {
    install(lua).unwrap()
}

fn converted(lua: &Lua, err: LuaValue) -> LuaValue {
    let lib = lib(lua);
    lib.convert.call(err).unwrap()
}

fn external(err: impl std::error::Error + Send + Sync + 'static) -> LuaValue {
    LuaValue::Error(Box::new(mlua::Error::external(err)))
}

#[test]
fn failure_builds_code_and_message_with_a_tostring_of_the_message() {
    let lua = lua();
    let lib = lib(&lua);
    let failed: mlua::Table = lib
        .failure
        .call(("connection_failed", "host.http: nope"))
        .unwrap();
    assert_eq!(failed.get::<String>("code").unwrap(), "connection_failed");
    assert_eq!(failed.get::<String>("message").unwrap(), "host.http: nope");
    // __tostring returns the message, so an uncaught table fails the
    // callback with the message as its text.
    lua.globals().set("failed", failed).unwrap();
    let shown: String = lua.load("return tostring(failed)").eval().unwrap();
    assert_eq!(shown, "host.http: nope");
}

#[test]
fn the_converter_maps_each_rust_failure_to_its_table() {
    let lua = lua();
    // A host failure keeps its code and message.
    let table = converted(
        &lua,
        external(super::Failure {
            code: ErrorCode::ConnectionFailed,
            message: "host.http: nope".into(),
        }),
    );
    let (code, message) = as_failure(&table).unwrap();
    assert_eq!(code, "connection_failed");
    assert_eq!(message, "host.http: nope");
    // An unattended login is `authentication_failed`.
    let table = converted(
        &lua,
        external(crate::oauth::Unattended {
            call: "open".into(),
        }),
    );
    let (code, message) = as_failure(&table).unwrap();
    assert_eq!(code, "authentication_failed");
    assert!(message.contains("host.oauth.open"), "{message}");
    // A failure table passes through with its own code; a string the
    // refresh function raised itself is the credential failing.
    let table = converted(
        &lua,
        external(crate::oauth::RefreshFailed {
            reached: false,
            message: "host.http: nope".into(),
            table_code: Some("connection_failed".into()),
        }),
    );
    let (code, _) = as_failure(&table).unwrap();
    assert_eq!(code, "connection_failed");
    let table = converted(
        &lua,
        external(crate::oauth::RefreshFailed {
            reached: true,
            message: "boom".into(),
            table_code: None,
        }),
    );
    let (code, message) = as_failure(&table).unwrap();
    assert_eq!(code, "credential_failed");
    assert_eq!(message, "boom");
}

#[test]
fn the_converter_leaves_anything_else_alone() {
    let lua = lua();
    for err in [
        LuaValue::String(lua.create_string("boom").unwrap()),
        LuaValue::Nil,
        LuaValue::Integer(3),
    ] {
        assert!(matches!(converted(&lua, err), LuaValue::Nil));
    }
    // A table, even one shaped like a failure, is not a Rust failure.
    let plain = lua.create_table().unwrap();
    plain.set("code", "connection_failed").unwrap();
    assert!(matches!(
        converted(&lua, LuaValue::Table(plain)),
        LuaValue::Nil
    ));
    // Another Rust error is not a host failure either.
    assert!(matches!(
        converted(&lua, external(std::io::Error::other("disk gone"))),
        LuaValue::Nil
    ));
}

#[test]
fn code_names_are_the_registry_snake_case() {
    assert_eq!(code_name(&ErrorCode::ConnectionFailed), "connection_failed");
    assert_eq!(code_name(&ErrorCode::TooLarge), "too_large");
    assert_eq!(code_name(&ErrorCode::UnreadableReply), "unreadable_reply");
    assert_eq!(code_name(&ErrorCode::Other("custom".into())), "custom");
}

#[test]
fn as_failure_reads_only_string_code_and_message_tables() {
    let lua = lua();
    let table = lua.create_table().unwrap();
    table.set("code", "connection_failed").unwrap();
    table.set("message", "host.http: nope").unwrap();
    assert_eq!(
        as_failure(&LuaValue::Table(table)),
        Some(("connection_failed".into(), "host.http: nope".into()))
    );
    for value in [
        LuaValue::Nil,
        LuaValue::String(lua.create_string("boom").unwrap()),
    ] {
        assert_eq!(as_failure(&value), None);
    }
    let table = lua.create_table().unwrap();
    table.set("code", 3).unwrap();
    table.set("message", "host.http: nope").unwrap();
    assert_eq!(as_failure(&LuaValue::Table(table)), None);
}

/// Lua with the prelude installed: the new global `pcall`/`xpcall`.
fn prelude() -> Lua {
    let lua = Lua::new();
    let clock = fakes::clock::FakeClock::new();
    let deadline = Deadline::new(clock);
    let dir = fakes::TempDir::new("fiber-failure-prelude");
    crate::lua::install_prelude(&lua, &deadline, dir.path().to_path_buf(), crate::MEMORY_CAP)
        .unwrap();
    lua
}

fn json(lua: &Lua, code: &str) -> serde_json::Value {
    let value: LuaValue = lua.load(code).eval().unwrap();
    lua.from_value(value).unwrap()
}

#[test]
fn pcall_returns_every_value_and_passes_errors_through_unchanged() {
    let lua = prelude();
    assert_eq!(
        json(
            &lua,
            "return table.pack(pcall(function(a, b) return a + b, a - b end, 3, 1))"
        ),
        serde_json::json!([true, 4, 2])
    );
    // A string error comes back as the same string.
    assert_eq!(
        json(&lua, "return table.pack(pcall(error, 'boom', 0))"),
        serde_json::json!([false, "boom"])
    );
    // A table error comes back as the same table.
    let same: bool = lua
        .load(
            "local err = { code = 'custom', message = 'mine' }
             local ok, got = pcall(error, err, 0)
             return ok == false and got == err",
        )
        .eval()
        .unwrap();
    assert!(same);
}

#[test]
fn pcall_converts_a_rust_host_failure_to_its_table() {
    let lua = prelude();
    lua.globals()
        .set(
            "boom",
            lua.create_function(|_, ()| {
                Err::<(), _>(fail(ErrorCode::ConnectionFailed, "host.http: nope".into()))
            })
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        json(
            &lua,
            "local ok, err = pcall(boom) return { ok, err.code, err.message }"
        ),
        serde_json::json!([false, "connection_failed", "host.http: nope"])
    );
}

#[test]
fn xpcall_calls_its_handler_with_the_error_and_keeps_every_return() {
    let lua = prelude();
    assert_eq!(
        json(
            &lua,
            "return table.pack(xpcall(function() return 1, 2 end, tostring))",
        ),
        serde_json::json!([true, 1, 2])
    );
    let seen: String = lua
        .load(
            "local seen
             local ok, val = xpcall(function() error('boom', 0) end, function(e) seen = e return 'handled' end)
             return tostring(ok) .. '|' .. tostring(val) .. '|' .. tostring(seen)",
        )
        .eval()
        .unwrap();
    assert_eq!(seen, "false|handled|boom");
    // The handler receives the converted table for a host failure.
    lua.globals()
        .set(
            "boom",
            lua.create_function(|_, ()| {
                Err::<(), _>(fail(ErrorCode::Timeout, "host.http: slow".into()))
            })
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        json(
            &lua,
            "local ok, code = xpcall(boom, function(e) return e.code end) return { ok, code }",
        ),
        serde_json::json!([false, "timeout"])
    );
}
