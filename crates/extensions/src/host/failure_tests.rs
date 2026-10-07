//! The `{ code, message }` constructor and the prelude's yield-forwarding
//! `pcall`/`xpcall` (`docs/extensions.md`, "How an extension runs").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use contract::ErrorCode;
use mlua::{Lua, LuaSerdeExt, Value as LuaValue};

use super::{Boundary, FailureLib, code_name, install};
use crate::lua::Deadline;

fn lua() -> Lua {
    Lua::new()
}

fn lib(lua: &Lua) -> FailureLib {
    install(lua).unwrap()
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
    lua.globals().set("failed", failed).unwrap();
    let shown: String = lua.load("return tostring(failed)").eval().unwrap();
    assert_eq!(shown, "host.http: nope");
}

#[test]
fn an_escaping_failure_is_matched_through_mlua_s_traceback() {
    let lua = lua();
    let lib = lib(&lua);
    lua.globals().set("failure", lib.failure.clone()).unwrap();
    let message = "host.oauth.poll needs a person to log in, and nobody is attached";
    let raise =
        format!("error(failure('authentication_failed', '{message}', 'unattended:poll'), 0)");
    let thread = lua
        .create_thread(lua.load(&raise).into_function().unwrap())
        .unwrap();
    let error = thread.resume::<()>(()).unwrap_err();
    // mlua reports a resumed thread's error as its tostring and a traceback.
    assert!(
        matches!(&error, mlua::Error::RuntimeError(text) if text.starts_with(&format!("{message}\nstack traceback:"))),
        "{error:?}"
    );
    let pending = lib.state.take_matching(&error).unwrap();
    assert_eq!(pending.message, message);
    assert_eq!(
        pending.boundary,
        Boundary::Unattended {
            call: "poll".into()
        }
    );
    // Taken once.
    assert!(lib.state.take_matching(&error).is_none());

    let error = lua.load(&raise).exec().unwrap_err();
    assert!(lib.state.take_matching(&error).is_some(), "{error:?}");
    // Raised through a Rust function, mlua wraps it in `CallbackError`.
    let raise_fn = lua.load(&raise).into_function().unwrap();
    let through_rust = lua
        .create_function(move |_, ()| raise_fn.call::<()>(()))
        .unwrap();
    let wrapped = through_rust.call::<()>(()).unwrap_err();
    assert!(
        matches!(wrapped, mlua::Error::CallbackError { .. }),
        "{wrapped:?}"
    );
    assert!(lib.state.take_matching(&wrapped).is_some(), "{wrapped:?}");

    // Another error, or one that merely starts with the same text, is not it.
    lua.load(&raise).exec().unwrap_err();
    for other in [
        "a different error".to_owned(),
        format!("{message} and more"),
    ] {
        assert!(
            lib.state
                .take_matching(&mlua::Error::RuntimeError(other))
                .is_none()
        );
    }
    // A failure with no boundary records nothing: a fresh state, since
    // the run above leaves its own record behind.
    let clean = Lua::new();
    let clean_lib = install(&clean).unwrap();
    clean
        .globals()
        .set("failure", clean_lib.failure.clone())
        .unwrap();
    clean
        .load(format!(
            "error(failure('authentication_failed', '{message}'), 0)"
        ))
        .exec()
        .unwrap_err();
    assert!(
        clean_lib
            .state
            .take_matching(&mlua::Error::RuntimeError(message.to_owned()))
            .is_none()
    );
}

#[test]
fn code_names_are_the_registry_snake_case() {
    assert_eq!(code_name(&ErrorCode::ConnectionFailed), "connection_failed");
    assert_eq!(code_name(&ErrorCode::TooLarge), "too_large");
    assert_eq!(code_name(&ErrorCode::UnreadableReply), "unreadable_reply");
    assert_eq!(code_name(&ErrorCode::Other("custom".into())), "custom");
}

/// Lua with the prelude installed: the yield-forwarding `pcall`/`xpcall`.
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
    assert_eq!(
        json(&lua, "return table.pack(pcall(error, 'boom', 0))"),
        serde_json::json!([false, "boom"])
    );
    let same: bool = lua
        .load(
            "local original = { code = 'custom', message = 'mine', extra = 7 }
             local ok, caught = pcall(error, original, 0)
             return not ok and rawequal(caught, original) and caught.extra == 7",
        )
        .eval()
        .unwrap();
    assert!(same);
}

/// The failure a raw `coroutine.resume` of `code` catches: its code and
/// message. The value is the table at its source, not userdata.
fn resumed(lua: &Lua, code: &str) -> (String, String) {
    let (ok, err): (bool, LuaValue) = lua
        .load(format!(
            "return coroutine.resume(coroutine.create(function() {code} end))"
        ))
        .eval()
        .unwrap();
    assert!(!ok, "{code} unexpectedly succeeded");
    let LuaValue::Table(failed) = err else {
        panic!("{code} raised no failure table");
    };
    (failed.get("code").unwrap(), failed.get("message").unwrap())
}

#[test]
fn pcall_and_coroutine_resume_receive_failure_tables_at_their_source() {
    let lua = prelude();
    let lib = install(&lua).unwrap();
    lua.globals().set("failure", lib.failure.clone()).unwrap();
    lua.load(
        "host_failure = function()
           error(failure('connection_failed', 'host.http: nope'), 0)
         end",
    )
    .exec()
    .unwrap();
    assert_eq!(
        json(
            &lua,
            "local ok, err = pcall(host_failure)
             return { ok, err.code, err.message }",
        ),
        serde_json::json!([false, "connection_failed", "host.http: nope"])
    );
    assert_eq!(
        resumed(&lua, "host_failure()"),
        ("connection_failed".into(), "host.http: nope".into())
    );
}

#[test]
fn wrap_raises_the_table_at_its_source() {
    let lua = lua();
    let lib = lib(&lua);
    let raw = lua
        .create_function(|lua, ()| {
            Ok(mlua::MultiValue::from_vec(vec![
                LuaValue::Nil,
                LuaValue::String(lua.create_string("connection_failed")?),
                LuaValue::String(lua.create_string("host.http: nope")?),
            ]))
        })
        .unwrap();
    let wrapped = super::wrap(&lua, raw, &lib.failure).unwrap();
    lua.globals().set("wrapped", wrapped).unwrap();
    assert_eq!(
        resumed(&lua, "return wrapped()"),
        ("connection_failed".into(), "host.http: nope".into())
    );
    let raw = lua
        .create_function(|_, ()| Ok(mlua::MultiValue::from_vec(vec![LuaValue::Nil])))
        .unwrap();
    let wrapped = super::wrap(&lua, raw, &lib.failure).unwrap();
    lua.globals().set("missing", wrapped).unwrap();
    let value: LuaValue = lua.load("return missing()").eval().unwrap();
    assert_eq!(value, LuaValue::Nil);
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
}

#[test]
fn xpcall_forwards_extra_arguments_to_the_function() {
    let lua = prelude();
    assert_eq!(
        json(
            &lua,
            "return table.pack(xpcall(function(a, b) return a + b, a - b end, tostring, 3, 1))"
        ),
        serde_json::json!([true, 4, 2])
    );
}

#[test]
fn xpcall_runs_a_failing_handler_protected() {
    let lua = prelude();
    assert_eq!(
        json(
            &lua,
            "return table.pack(xpcall(function() error('boom', 0) end, function(e) error('handler:' .. e, 0) end))"
        ),
        serde_json::json!([false, "handler:boom"])
    );
}

#[test]
fn a_noted_failure_matches_its_text_and_reports_its_message() {
    let lua = lua();
    let lib = lib(&lua);
    for (context, expected) in [
        ("refresh:reached", Boundary::Refresh { reached: true }),
        ("refresh:unreached", Boundary::Refresh { reached: false }),
        (
            "unattended:open",
            Boundary::Unattended {
                call: "open".into(),
            },
        ),
        (
            "unattended:callback",
            Boundary::Unattended {
                call: "callback".into(),
            },
        ),
        (
            "unattended:poll",
            Boundary::Unattended {
                call: "poll".into(),
            },
        ),
    ] {
        lib.note_failure
            .call::<()>(("table: 0x1", "offline", context))
            .unwrap();
        let error =
            mlua::Error::RuntimeError("table: 0x1\nstack traceback:\n\t[C]: in ?".to_owned());
        let pending = lib.state.take_matching(&error).unwrap();
        assert_eq!(pending.message, "offline");
        assert_eq!(pending.boundary, expected);
    }
    assert!(
        lib.note_failure
            .call::<()>(("t", "m", "elsewhere"))
            .is_err()
    );
}

#[test]
fn two_caught_failures_both_keep_their_classification() {
    let lua = lua();
    let lib = lib(&lua);
    let open = "host.oauth.open needs a person to log in, and nobody is attached";
    let poll = "host.oauth.poll needs a person to log in, and nobody is attached";
    let _: mlua::Table = lib
        .failure
        .call(("authentication_failed", open, "unattended:open"))
        .unwrap();
    let _: mlua::Table = lib
        .failure
        .call(("authentication_failed", poll, "unattended:poll"))
        .unwrap();
    // Rethrowing the first table still finds its own record, not the
    // second catch's: a single slot would have lost it.
    let first = lib
        .state
        .take_matching(&mlua::Error::RuntimeError(open.to_owned()))
        .unwrap();
    assert_eq!(
        first.boundary,
        Boundary::Unattended {
            call: "open".into()
        }
    );
    let second = lib
        .state
        .take_matching(&mlua::Error::RuntimeError(poll.to_owned()))
        .unwrap();
    assert_eq!(
        second.boundary,
        Boundary::Unattended {
            call: "poll".into()
        }
    );
    assert!(
        lib.state
            .take_matching(&mlua::Error::RuntimeError(open.to_owned()))
            .is_none()
    );
}

#[test]
fn a_match_takes_the_most_recent_record() {
    let lua = lua();
    let lib = lib(&lua);
    lib.note_failure
        .call::<()>(("same", "first", "refresh:reached"))
        .unwrap();
    lib.note_failure
        .call::<()>(("same", "second", "refresh:unreached"))
        .unwrap();
    let error = mlua::Error::RuntimeError("same".to_owned());
    assert_eq!(lib.state.take_matching(&error).unwrap().message, "second");
    assert_eq!(lib.state.take_matching(&error).unwrap().message, "first");
    assert!(lib.state.take_matching(&error).is_none());
}

#[test]
fn the_list_keeps_the_cap_and_drops_the_oldest() {
    let lua = lua();
    let lib = lib(&lua);
    // The cap itself fits: every record is still there.
    for i in 0..super::MAX_PENDING {
        lib.note_failure
            .call::<()>((format!("text-{i}"), "m", "refresh:reached"))
            .unwrap();
    }
    assert!(
        lib.state
            .take_matching(&mlua::Error::RuntimeError("text-0".to_owned()))
            .is_some()
    );
    // A fresh state for the eviction phase: the run above leaves the rest
    // of its records behind.
    let clean = Lua::new();
    let clean_lib = install(&clean).unwrap();
    for i in 0..super::MAX_PENDING {
        clean_lib
            .note_failure
            .call::<()>((format!("text-{i}"), "m", "refresh:reached"))
            .unwrap();
    }
    // One past the cap evicts the oldest.
    clean_lib
        .note_failure
        .call::<()>((
            format!("text-{}", super::MAX_PENDING),
            "m",
            "refresh:reached",
        ))
        .unwrap();
    assert!(
        clean_lib
            .state
            .take_matching(&mlua::Error::RuntimeError("text-0".to_owned()))
            .is_none(),
        "the oldest record past the cap is dropped"
    );
    assert!(
        clean_lib
            .state
            .take_matching(&mlua::Error::RuntimeError("text-1".to_owned()))
            .is_some()
    );
    assert!(
        clean_lib
            .state
            .take_matching(&mlua::Error::RuntimeError(format!(
                "text-{}",
                super::MAX_PENDING
            )))
            .is_some()
    );
}

#[test]
fn a_refresh_without_a_matching_unattended_failure_records() {
    // Only the first conjunct holds (a refresh boundary, but no
    // unattended record with the same text): it must record. With `||`
    // the refresh alone would skip recording.
    let lua = lua();
    let lib = lib(&lua);
    lib.note_failure
        .call::<()>(("fresh", "m", "refresh:reached"))
        .unwrap();
    let pending = lib
        .state
        .take_matching(&mlua::Error::RuntimeError("fresh".to_owned()))
        .unwrap();
    assert_eq!(pending.boundary, Boundary::Refresh { reached: true });
    // Only the second conjunct holds (the text matches an unattended
    // record, but the new boundary is not a refresh): it must record
    // too. With `||` the matching text alone would skip recording.
    let message = "host.oauth.open needs a person to log in, and nobody is attached";
    let _: mlua::Table = lib
        .failure
        .call(("authentication_failed", message, "unattended:open"))
        .unwrap();
    lib.note_failure
        .call::<()>((message, message, "unattended:poll"))
        .unwrap();
    let error = mlua::Error::RuntimeError(message.to_owned());
    let recent = lib.state.take_matching(&error).unwrap();
    assert_eq!(
        recent.boundary,
        Boundary::Unattended {
            call: "poll".into()
        }
    );
    let earlier = lib.state.take_matching(&error).unwrap();
    assert_eq!(
        earlier.boundary,
        Boundary::Unattended {
            call: "open".into()
        }
    );
}

#[test]
fn a_refresh_forwarding_an_unattended_failure_keeps_its_mapping() {
    let lua = lua();
    let lib = lib(&lua);
    let message = "host.oauth.open needs a person to log in, and nobody is attached";
    let _: mlua::Table = lib
        .failure
        .call(("authentication_failed", message, "unattended:open"))
        .unwrap();
    lib.note_failure
        .call::<()>((message, message, "refresh:reached"))
        .unwrap();
    let pending = lib
        .state
        .take_matching(&mlua::Error::RuntimeError(message.to_owned()))
        .unwrap();
    assert_eq!(
        pending.boundary,
        Boundary::Unattended {
            call: "open".into()
        }
    );
}
