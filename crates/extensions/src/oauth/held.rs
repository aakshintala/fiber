//! What `host.oauth.refresh` holds while its function runs
//! (`docs/extensions.md`, "Host calls"): the stored credential's lock, or,
//! during `fiber login`, a slot for the login's result. `mod.rs` keeps the
//! calls that wait; this module keeps what they hold. Split out so `mod.rs`
//! stays under the file-size rule (`docs/code-quality.md`, "Size").

use std::cell::RefCell;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::UNIX_EPOCH;

use config::CredentialLock;
use contract::clock::Clock;
use mlua::{UserData, UserDataMethods, Value as LuaValue};
use serde_json::Value;

use crate::host::{self, failure};
use crate::lua_provider::REFRESH_BEFORE;

/// Where a login's result waits: the last successful `held:write` in a
/// login fills it, and `fiber login` stores it after the flow, under the
/// chosen label's lock (`docs/model-routing.md`, "Logging in").
pub(crate) type LoginSlot = Arc<Mutex<Option<Value>>>;

/// What a `host.oauth.refresh` holds.
pub(crate) enum Holder {
    /// The stored credential's lock.
    File(CredentialLock),
    /// A login's result slot, which holds no file.
    Login(LoginSlot),
}

impl fmt::Debug for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The lock names its file; the file's contents never print.
            Self::File(lock) => f.debug_tuple("File").field(lock).finish(),
            // The slot holds the login's tokens: only whether it is
            // filled prints, never the value (`docs/code-quality.md`,
            // "Errors").
            Self::Login(slot) => f.debug_tuple("Login").field(&lock(slot).is_some()).finish(),
        }
    }
}

/// A stored credential held under its lock, as Lua sees it. `release()` and
/// a collected handle both free the lock; releasing a login's slot keeps
/// what it holds.
pub(crate) struct Held {
    holder: RefCell<Option<Holder>>,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The holder may carry the login's tokens, and the clock is not
        // `Debug`: neither prints, only which kind is held.
        let holder = match self.holder.borrow().as_ref() {
            None => "released",
            Some(Holder::File(_)) => "file",
            Some(Holder::Login(_)) => "login",
        };
        f.debug_struct("Held")
            .field("holder", &holder)
            .finish_non_exhaustive()
    }
}

impl Held {
    pub(crate) fn new(holder: Holder, clock: Arc<dyn Clock>) -> Self {
        Self {
            holder: RefCell::new(Some(holder)),
            clock,
        }
    }

    /// Whether this hold is a login's, whose refresh function runs directly
    /// instead of through the recorded `attempt`
    /// (`docs/model-routing.md`, "Logging in").
    pub(crate) fn login(&self) -> bool {
        matches!(self.holder.borrow().as_ref(), Some(Holder::Login(_)))
    }

    /// The extension clock's wall time in Unix seconds; past it a value is
    /// expired, as [`crate::lua_provider`] judges a fetched token.
    fn now(&self) -> i64 {
        self.clock
            .wall()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }

    /// Whether `stored` needs refreshing: not a usable credential, or one
    /// that expires within [`REFRESH_BEFORE`].
    pub(crate) fn due(&self, stored: &Value) -> bool {
        let now = self.now();
        let window = i64::try_from(REFRESH_BEFORE.as_secs()).unwrap_or(i64::MAX);
        usable(stored).is_none_or(|expires| expires <= now.saturating_add(window))
    }
}

/// The expiry of a stored credential: an object with a non-empty string
/// `token` and an integer `expires_at`. None for anything else.
fn usable(value: &Value) -> Option<i64> {
    let token = value.get("token").and_then(Value::as_str)?;
    if token.is_empty() {
        return None;
    }
    value.get("expires_at").and_then(Value::as_i64)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic aborts the process (`docs/code-quality.md`, "Panics"), so no
    // holder can leave the lock poisoned.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl UserData for Held {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("read", |lua, this, ()| {
            // A coded failure returns `(nil, code, message)` for the refresh
            // half to raise as the table; no longer held returns
            // `(nil, message)` for it to raise as the string.
            let holder = this.holder.borrow();
            let Some(holder) = holder.as_ref() else {
                return failure::raw_string(
                    lua,
                    "host.oauth.refresh: the credential is no longer held".to_owned(),
                );
            };
            match holder {
                Holder::File(lock) => match lock.read() {
                    Ok(None) => Ok(mlua::MultiValue::from_vec(vec![LuaValue::Nil])),
                    Ok(Some(value)) => {
                        Ok(mlua::MultiValue::from_vec(vec![host::to_lua(lua, &value)?]))
                    }
                    Err(e) => {
                        failure::raw_failure(lua, &contract::ErrorCode::IoFailed, e.to_string())
                    }
                },
                // A login holds no file: its function logs in when it sees
                // nil (`docs/model-routing.md`, "Logging in").
                Holder::Login(_) => Ok(mlua::MultiValue::from_vec(vec![LuaValue::Nil])),
            }
        });
        methods.add_method("due", |_, this, stored: LuaValue| {
            Ok(host::to_json(&stored).map_or(true, |value| this.due(&value)))
        });
        methods.add_method("login", |_, this, ()| Ok(this.login()));
        methods.add_method("write", |lua, this, fresh: LuaValue| {
            let value = match host::to_json(&fresh) {
                Ok(value) => value,
                Err(err) => {
                    return failure::raw_string(
                        lua,
                        format!(
                            "host.oauth.refresh: {}",
                            err.to_string().lines().next().unwrap_or_default()
                        ),
                    );
                }
            };
            let Some(expires) = usable(&value) else {
                return failure::raw_string(
                    lua,
                    "host.oauth.refresh: the function must return a table with a `token` string and an `expires_at` whole number of seconds".to_owned(),
                );
            };
            // An expired value is refused before it is stored, by a refresh
            // or a login: storing it would write a credential that is dead
            // on arrival (`docs/extensions.md`, "Host calls").
            if expires <= this.now() {
                return failure::raw_failure(
                    lua,
                    &contract::ErrorCode::AuthenticationFailed,
                    "host.oauth.refresh: the refreshed token has already expired".to_owned(),
                );
            }
            let holder = this.holder.borrow();
            let Some(holder) = holder.as_ref() else {
                return failure::raw_string(
                    lua,
                    "host.oauth.refresh: the credential is no longer held".to_owned(),
                );
            };
            match holder {
                Holder::File(lock) => match lock.write(&value) {
                    Ok(()) => Ok(mlua::MultiValue::from_vec(vec![])),
                    Err(e) => failure::raw_failure(
                        lua,
                        &contract::ErrorCode::IoFailed,
                        e.to_string(),
                    ),
                },
                // A login keeps its result for `fiber login`, which stores
                // it after the flow; a second write replaces the first.
                Holder::Login(slot) => {
                    *lock(slot) = Some(value);
                    Ok(mlua::MultiValue::from_vec(vec![]))
                }
            }
        });
        methods.add_method("release", |_, this, ()| {
            this.holder.borrow_mut().take();
            Ok(())
        });
    }
}
