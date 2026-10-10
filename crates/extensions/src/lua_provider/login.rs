//! Running one provider's `credential()` login (`docs/model-routing.md`,
//! "Logging in"): what `fiber login` runs for a provider whose data marks
//! it as a browser login, and nothing else.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::oauth::LoginSlot;
use crate::{Error, LuaProvider};

/// How the person logs in: a browser, or a device code they type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    /// A browser, opened at the vendor's authorize URL.
    Browser,
    /// A device code, shown for the person to enter.
    Device,
}

/// A finished login: what to store, and the login's email for the label.
#[derive(Debug)]
pub struct LoggedIn {
    /// The stored value: the token, its expiry, the refresh token and the
    /// account id, as JSON.
    pub stored: StoredLogin,
    /// The login's email, when it revealed one.
    pub email: Option<String>,
}

/// A login's stored value. `Debug` prints no field: it holds the refresh
/// token (`docs/code-quality.md`, "Errors").
pub struct StoredLogin(Value);

impl StoredLogin {
    /// The stored value, as JSON.
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

impl std::fmt::Debug for StoredLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoredLogin(..)")
    }
}

impl LuaProvider {
    /// Runs `credential()` as a login for the stored credential `credential`,
    /// passing the `--as` label when one was given and whether the person
    /// chose the browser or a device code, and returns what to store and
    /// the login's email (`docs/model-routing.md`, "Logging in").
    /// `host.oauth.refresh` holds no file during the call: what its function
    /// returns is kept in a slot this reads after the flow, never under a
    /// label lock (a login never holds the label's lock while Lua runs).
    pub fn login(
        self: &Arc<Self>,
        credential: &str,
        label: Option<&str>,
        method: LoginMethod,
    ) -> Result<LoggedIn, Error> {
        let slot: LoginSlot = Arc::new(Mutex::new(None));
        let inner: Result<LoggedIn, Error> = (|| {
            let returned = self.extension.provider_login(
                &self.name,
                credential,
                label,
                method,
                Arc::clone(&slot),
            )?;
            // The stored value comes from the slot the login's refresh
            // filled, never from the return: until its `held:write` ran,
            // `credential()` stored nothing.
            let stored = super::lock(&slot).clone().ok_or_else(|| {
                self.bad_return(
                    "credential",
                    "credential() stored nothing during a login".into(),
                )
            })?;
            self.check_token(&stored)?;
            // The login's email rides the return, for `fiber login` alone;
            // every other call ignores it (`docs/model-routing.md`,
            // "Logging in").
            let email = match returned.get("email") {
                Some(Value::String(email)) if !email.is_empty() => Some(email.clone()),
                // An empty address names no label, so it is no email.
                None | Some(Value::Null) | Some(Value::String(_)) => None,
                _ => {
                    return Err(self.bad_return("credential", "`email` is not a string".into()));
                }
            };
            Ok(LoggedIn {
                stored: StoredLogin(stored),
                email,
            })
        })();
        inner.map_err(Self::refresh_error)
    }

    /// Stops this provider's extension: a call waiting in it returns an
    /// error, a later call fails without running, and a parked
    /// `host.oauth.callback` closes its listener. Idempotent.
    pub fn stop(&self) {
        self.extension.dispose();
    }
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
