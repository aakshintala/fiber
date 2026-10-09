#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use super::{credential_headers_are_strings, pending_error};
use crate::host::failure::{Boundary, PendingFailure};
use crate::{Error, ErrorCode};

fn pending(message: &str, boundary: Boundary) -> PendingFailure {
    PendingFailure {
        text: message.to_owned(),
        message: message.to_owned(),
        boundary,
    }
}

#[test]
fn unattended_oauth_keeps_authentication_failed_when_uncaught() {
    let error = pending_error(
        "acme",
        pending(
            "host.oauth.poll needs a person to log in, and nobody is attached",
            Boundary::Unattended {
                call: "poll".to_owned(),
            },
        ),
    );
    assert!(matches!(&error, Error::Unattended { call, .. } if call == "poll"));
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
}

#[test]
fn a_reached_refresh_keeps_authentication_failed_when_uncaught() {
    let error = pending_error(
        "acme",
        pending("offline", Boundary::Refresh { reached: true }),
    );
    assert!(matches!(&error, Error::RefreshRejected { message, .. } if message == "offline"));
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
}

#[test]
fn an_unreached_refresh_keeps_connection_failed_when_uncaught() {
    let error = pending_error(
        "acme",
        pending("host.http: offline", Boundary::Refresh { reached: false }),
    );
    assert!(matches!(&error, Error::RefreshUnreachable { .. }));
    assert_eq!(error.code(), ErrorCode::ConnectionFailed);
}

#[test]
fn credential_headers_must_be_a_table_of_strings_when_present() {
    let lua = mlua::Lua::new();
    // Each row: a `credential()` return value, and whether its `headers`
    // passes the pre-conversion check (`docs/extensions.md`, "What writing
    // a provider looks like"). A numeric key would not survive `to_json`,
    // which turns `{[42] = "v"}` into `{"42": "v"}` and `{[1] = "v"}` into
    // an array, so it fails here.
    let cases: &[(&str, bool)] = &[
        ("return { token = \"t\", expires_at = 1 }", true),
        (
            "return { token = \"t\", expires_at = 1, headers = {} }",
            true,
        ),
        (
            "return { token = \"t\", expires_at = 1, headers = { a = \"v\" } }",
            true,
        ),
        (
            "return { token = \"t\", expires_at = 1, headers = { [42] = \"v\" } }",
            false,
        ),
        (
            "return { token = \"t\", expires_at = 1, headers = { [1] = \"v\" } }",
            false,
        ),
        (
            "return { token = \"t\", expires_at = 1, headers = { a = 1 } }",
            false,
        ),
        (
            "return { token = \"t\", expires_at = 1, headers = { a = true } }",
            false,
        ),
        (
            "return { token = \"t\", expires_at = 1, headers = \"x\" }",
            false,
        ),
        (
            "return { token = \"t\", expires_at = 1, headers = 1 }",
            false,
        ),
        ("return nil", true),
        ("return \"x\"", true),
    ];
    for (script, expected) in cases {
        let value: mlua::Value = lua
            .load(*script)
            .eval()
            .unwrap_or_else(|_| panic!("the test's own script fails: {script}"));
        assert_eq!(
            credential_headers_are_strings(&value),
            *expected,
            "{script}"
        );
    }
}

#[test]
fn credential_headers_treats_null_like_absent_and_a_failed_lookup_like_a_clash() {
    let lua = mlua::Lua::new();
    // A null carried over from a host call is no headers, as for `sign()`.
    let table = lua.create_table().unwrap();
    table.set("headers", mlua::Value::NULL).unwrap();
    assert!(credential_headers_are_strings(&mlua::Value::Table(table)));
    // A `headers` lookup that raises (here through `__index`) is not a
    // table of strings.
    let value: mlua::Value = lua
        .load(
            "local t = {}\n\
             setmetatable(t, { __index = function() error(\"boom\") end })\n\
             return t",
        )
        .eval()
        .unwrap();
    assert!(!credential_headers_are_strings(&value));
}
