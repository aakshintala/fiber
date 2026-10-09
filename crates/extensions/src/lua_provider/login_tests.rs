#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::fs;
use std::sync::Arc;

use fakes::clock::FakeClock;
use serde_json::json;

use super::{LoggedIn, LoginMethod};
use crate::{Error, LuaExtension, LuaProvider};

/// A login fixture whose `credential()` echoes its login argument in the
/// stored value's `seen` field, fills the slot through `refresh`, and
/// returns the `email` the `want` table names.
const LOGIN: &str = r#"
fiber.provider("p", {
  credential = { timeout = 60000, run = function(arg)
    local fresh = host.oauth.refresh(function(stored)
      if stored ~= nil then error("stored was not nil") end
      return { token = "tok", expires_at = 4102444800, refresh_token = "rt",
               seen = { login = arg.login, label = arg.label } }
    end)
    local returned = { token = fresh.token, expires_at = fresh.expires_at }
    if WANT_EMAIL ~= nil then returned.email = WANT_EMAIL end
    return returned
  end }
})
"#;

/// A login fixture that returns without filling the slot.
const NO_STORE: &str = r#"
fiber.provider("p", {
  credential = { timeout = 60000, run = function(arg)
    return { token = "tok", expires_at = 4102444800 }
  end }
})
"#;

/// A login fixture whose refresh function fails as `mode` says.
const FAILING: &str = r#"
fiber.provider("p", {
  credential = { timeout = 60000, run = function(arg)
    if MODE == "string" then
      host.oauth.refresh(function(stored) error("boom") end)
    elseif MODE == "expired" then
      host.oauth.refresh(function(stored)
        return { token = "t", expires_at = 1 }
      end)
    elseif MODE == "unattended" then
      host.oauth.open("https://auth.example/")
    end
    return { token = "tok", expires_at = 4102444800 }
  end }
})
"#;

fn provider(name: &str, init: &str, prefix: &str) -> (fakes::TempDir, Arc<LuaProvider>) {
    let root = fakes::TempDir::new(name);
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let init = init.replace("WANT_EMAIL", prefix).replace("MODE", prefix);
    fs::write(dir.join("init.lua"), init).unwrap();
    let extension = Arc::new(LuaExtension::new("ext", dir, home, FakeClock::new()));
    (root, LuaProvider::new(extension, "p"))
}

/// The `BadReturn` reason inside a login's `Credential` wrapper: a login
/// keeps the refresh mapping, so every shape error arrives wrapped.
fn bad_return_why(error: &Error) -> &str {
    let Error::Credential(inner) = error else {
        panic!("not a credential failure: {error:?}");
    };
    let Error::BadReturn { why, .. } = inner.as_ref() else {
        panic!("not a bad return: {error:?}");
    };
    why
}

fn logged_in(provider: &Arc<LuaProvider>, label: Option<&str>, method: LoginMethod) -> LoggedIn {
    provider.login("p", label, method).unwrap()
}

#[test]
fn a_login_returns_what_the_slot_holds_and_the_email() {
    let (_root, provider) = provider("fiber-provider-login-ok", LOGIN, "\"alice@example.com\"");
    let logged = logged_in(&provider, None, LoginMethod::Browser);
    assert_eq!(
        logged.stored.as_value(),
        &json!({
            "token": "tok",
            "expires_at": 4102444800u64,
            "refresh_token": "rt",
            "seen": { "login": "browser" },
        })
    );
    assert_eq!(logged.email.as_deref(), Some("alice@example.com"));
}

#[test]
fn a_login_passes_the_label_and_the_device_method() {
    let (_root, provider) = provider("fiber-provider-login-args", LOGIN, "nil");
    let logged = logged_in(&provider, Some("work"), LoginMethod::Device);
    assert_eq!(
        logged.stored.as_value()["seen"],
        json!({ "login": "device", "label": "work" })
    );
    assert_eq!(logged.email, None);
}

#[test]
fn a_login_that_stored_nothing_is_a_bad_return() {
    let (_root, provider) = provider("fiber-provider-login-nostore", NO_STORE, "");
    let error = provider.login("p", None, LoginMethod::Browser).unwrap_err();
    assert!(
        bad_return_why(&error).contains("stored nothing"),
        "{error:?}"
    );
    assert_eq!(error.code(), contract::ErrorCode::CredentialFailed);
}

#[test]
fn an_expired_stored_value_is_credential_failed() {
    let root = fakes::TempDir::new("fiber-provider-login-expired");
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("init.lua"),
        r#"fiber.provider("p", {
          credential = { timeout = 60000, run = function(arg)
            local fresh = host.oauth.refresh(function(stored)
              return { token = "t", expires_at = 1, refresh_token = "rt" }
            end)
            return { token = fresh.token, expires_at = fresh.expires_at }
          end }
        })"#,
    )
    .unwrap();
    let extension = Arc::new(LuaExtension::new("ext", dir, home, FakeClock::new()));
    let provider = LuaProvider::new(extension, "p");
    let error = provider.login("p", None, LoginMethod::Browser).unwrap_err();
    assert_eq!(
        error.code(),
        contract::ErrorCode::CredentialFailed,
        "{error}"
    );
}

#[test]
fn a_stored_value_expiring_this_instant_is_credential_failed() {
    // Equal to the wall is already expired: the write refuses it before
    // anything is stored, so the slot stays empty.
    let wall = 1_700_000_000u64;
    let root = fakes::TempDir::new("fiber-provider-login-boundary");
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("init.lua"),
        format!(
            r#"fiber.provider("p", {{
              credential = {{ timeout = 60000, run = function(arg)
                local fresh = host.oauth.refresh(function(stored)
                  return {{ token = "t", expires_at = {wall}, refresh_token = "rt" }}
                end)
                return {{ token = fresh.token, expires_at = fresh.expires_at }}
              end }}
            }})"#
        ),
    )
    .unwrap();
    let extension = Arc::new(LuaExtension::new("ext", dir, home, FakeClock::new()));
    let provider = LuaProvider::new(extension, "p");
    let error = provider.login("p", None, LoginMethod::Browser).unwrap_err();
    assert_eq!(
        error.code(),
        contract::ErrorCode::CredentialFailed,
        "{error:?}"
    );
}

#[test]
fn an_expired_write_inside_a_login_is_credential_failed() {
    let (_root, provider) = provider("fiber-provider-login-expired-write", FAILING, "\"expired\"");
    let error = provider.login("p", None, LoginMethod::Browser).unwrap_err();
    assert!(matches!(&error, Error::Credential(_)), "{error:?}");
    assert_eq!(error.code(), contract::ErrorCode::CredentialFailed);
}

#[test]
fn a_string_raised_inside_a_login_is_credential_failed_not_a_rejected_refresh() {
    let (_root, provider) = provider("fiber-provider-login-string", FAILING, "\"string\"");
    let error = provider.login("p", None, LoginMethod::Browser).unwrap_err();
    assert!(matches!(&error, Error::Credential(_)), "{error:?}");
    assert_eq!(error.code(), contract::ErrorCode::CredentialFailed);
}

#[test]
fn an_unattended_call_inside_a_login_keeps_authentication_failed() {
    let (_root, provider) = provider("fiber-provider-login-unattended", FAILING, "\"unattended\"");
    let error = provider.login("p", None, LoginMethod::Browser).unwrap_err();
    assert!(matches!(&error, Error::Unattended { .. }), "{error:?}");
    assert_eq!(error.code(), contract::ErrorCode::AuthenticationFailed);
}

#[test]
fn a_non_string_email_is_a_bad_return_and_an_empty_one_is_no_email() {
    let root = fakes::TempDir::new("fiber-provider-login-email");
    for (n, email_lua, expected) in [("absent", "nil", None), ("empty", "\"\"", None)] {
        let home = root.path().join(format!("home-{n}"));
        let dir = root.path().join(format!("ext-{n}"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("init.lua"),
            format!(
                r#"fiber.provider("p", {{
                  credential = {{ timeout = 60000, run = function(arg)
                    local fresh = host.oauth.refresh(function(stored)
                      return {{ token = "tok", expires_at = 4102444800, refresh_token = "rt" }}
                    end)
                    local returned = {{ token = fresh.token, expires_at = fresh.expires_at }}
                    if {email_lua} ~= nil then returned.email = {email_lua} end
                    return returned
                  end }}
                }})"#
            ),
        )
        .unwrap();
        let extension = Arc::new(LuaExtension::new("ext", dir, home, FakeClock::new()));
        let provider = LuaProvider::new(extension, "p");
        let logged = provider.login("p", None, LoginMethod::Browser).unwrap();
        assert_eq!(logged.email.as_deref(), expected, "{n}");
    }
    for email_lua in ["5", "{ account = 1 }"] {
        let n = if email_lua == "5" { "number" } else { "table" };
        let home = root.path().join(format!("home-{n}"));
        let dir = root.path().join(format!("ext-{n}"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("init.lua"),
            format!(
                r#"fiber.provider("p", {{
                  credential = {{ timeout = 60000, run = function(arg)
                    local fresh = host.oauth.refresh(function(stored)
                      return {{ token = "tok", expires_at = 4102444800, refresh_token = "rt" }}
                    end)
                    return {{ token = fresh.token, expires_at = fresh.expires_at, email = {email_lua} }}
                  end }}
                }})"#
            ),
        )
        .unwrap();
        let extension = Arc::new(LuaExtension::new("ext", dir, home, FakeClock::new()));
        let provider = LuaProvider::new(extension, "p");
        let error = provider.login("p", None, LoginMethod::Browser).unwrap_err();
        assert!(bad_return_why(&error).contains("email"), "{n}: {error:?}");
    }
}

#[test]
fn a_stored_login_prints_redacted() {
    let (_root, provider) = provider("fiber-provider-login-ok", LOGIN, "\"alice@example.com\"");
    let logged = logged_in(&provider, None, LoginMethod::Browser);
    assert_eq!(format!("{:?}", logged.stored), "StoredLogin(..)");
    assert!(!format!("{:?}", logged).contains("rt"), "{:?}", logged);
}
