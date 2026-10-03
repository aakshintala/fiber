//! A provider's Lua (`docs/model-routing.md`, "Model discovery", "Signing a
//! request" and "Credentials"): its model list from `models()`, cached on
//! disk and refreshed in the background; its token from `credential()`,
//! refreshed off the request path before it expires; and `sign()`, the one
//! function on the request path, which sees the body's SHA-256 and never the
//! body.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use config::{ModelData, Secret};
use contract::signing::{self, SignRequest, Signer};
use serde_json::{Map, Value, json};

use crate::host::sha256_hex;
use crate::{Error, LuaExtension};

/// How long before its expiry a token is refreshed.
pub const REFRESH_BEFORE: Duration = Duration::from_secs(5 * 60);

/// One provider a Lua extension registered with `fiber.provider`.
pub struct LuaProvider {
    extension: Arc<LuaExtension>,
    name: String,
    models: Mutex<Option<Vec<ModelData>>>,
    token: Mutex<TokenState>,
}

#[derive(Default)]
struct TokenState {
    current: Option<(Secret, SystemTime)>,
    refreshing: bool,
}

impl LuaProvider {
    /// The provider `name` of `extension`. Its model cache and secrets use
    /// the extension's Fiber home.
    pub fn new(extension: Arc<LuaExtension>, name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            extension,
            name: name.into(),
            models: Mutex::default(),
            token: Mutex::default(),
        })
    }

    /// The provider's models: the list this process last discovered, else
    /// the cached copy, else what `models()` returns now.
    pub fn models(&self) -> Result<Vec<ModelData>, Error> {
        if let Some(models) = lock(&self.models).clone() {
            return Ok(models);
        }
        if let Some(cached) = config::read_model_cache(self.extension.home(), &self.name)? {
            return Ok(lock(&self.models).get_or_insert(cached).clone());
        }
        self.discover()
    }

    /// Calls `models()` on its own thread, as Fiber does at every start, and
    /// replaces the cached list with what it returns. Until it returns,
    /// [`LuaProvider::models`] serves the copy it had.
    pub fn refresh_models(self: &Arc<Self>) -> JoinHandle<Result<Vec<ModelData>, Error>> {
        let this = Arc::clone(self);
        thread::spawn(move || this.discover())
    }

    fn discover(&self) -> Result<Vec<ModelData>, Error> {
        let returned = self.call("models", Value::Null)?;
        let models: Vec<ModelData> = serde_json::from_value(returned.clone()).map_err(|e| {
            self.bad_return("models", format!("something other than a model list: {e}"))
        })?;
        config::write_model_cache(self.extension.home(), &self.name, &returned)?;
        *lock(&self.models) = Some(models.clone());
        Ok(models)
    }

    /// The token `credential()` last returned. With none, or one that has
    /// expired, it calls `credential()` and waits. Within
    /// [`REFRESH_BEFORE`] of expiry it returns the token it has and calls
    /// `credential()` again on another thread, so a request never waits on
    /// a refresh.
    // debt: a refresh that fails leaves the old token, and the next call
    // inside the window tries again; nothing reports the failure until the
    // token expires. Report a failed refresh as a notice if tokens are seen
    // expiring mid-session.
    pub fn token(self: &Arc<Self>) -> Result<Secret, Error> {
        let mut state = lock(&self.token);
        if let Some((token, expires)) = &state.current
            && let Ok(left) =
                expires.duration_since(contract::clock::Clock::wall(self.extension.clock()))
            && !left.is_zero()
        {
            let token = token.clone();
            let due = left <= REFRESH_BEFORE;
            if due && !state.refreshing {
                state.refreshing = true;
                let this = Arc::clone(self);
                thread::spawn(move || {
                    let fresh = this.fetch_token();
                    let mut state = lock(&this.token);
                    state.refreshing = false;
                    if let Ok(fresh) = fresh {
                        state.current = Some(fresh);
                    }
                });
            }
            return Ok(token);
        }
        let fresh = self.fetch_token()?;
        let token = fresh.0.clone();
        state.current = Some(fresh);
        Ok(token)
    }

    /// Calls `credential()`, which returns `{ token, expires_at }`, the
    /// expiry in seconds since the Unix epoch.
    fn fetch_token(&self) -> Result<(Secret, SystemTime), Error> {
        let returned = self.call("credential", Value::Null)?;
        let token = returned
            .get("token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| self.bad_return("credential", "no `token`".into()))?;
        let expires = returned
            .get("expires_at")
            .and_then(Value::as_f64)
            .and_then(|s| Duration::try_from_secs_f64(s).ok())
            .and_then(|s| UNIX_EPOCH.checked_add(s))
            .ok_or_else(|| {
                self.bad_return(
                    "credential",
                    "no `expires_at` in seconds since the Unix epoch".into(),
                )
            })?;
        // A token that is already expired, or whose expiry is this instant,
        // was never usable. Returning it would send a request that the
        // vendor will reject (`docs/model-routing.md`, "Credentials").
        if expires <= contract::clock::Clock::wall(self.extension.clock()) {
            return Err(self.bad_return("credential", "a token that has already expired".into()));
        }
        Ok((Secret::new(token.to_owned()), expires))
    }

    fn call(&self, function: &'static str, arg: Value) -> Result<Value, Error> {
        self.extension.provider_call(&self.name, function, arg)
    }

    fn bad_return(&self, function: &str, why: String) -> Error {
        Error::BadReturn {
            extension: self.extension.name().to_owned(),
            callback: format!("{}.{function}", self.name),
            why,
        }
    }
}

/// Calls `sign({ method, url, headers, body_sha256 })` and returns the
/// headers it returns, a table of names to values.
impl Signer for LuaProvider {
    fn sign(&self, request: &SignRequest<'_>) -> Result<Vec<(String, String)>, signing::Error> {
        let headers: Map<String, Value> = request
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), Value::String(value.clone())))
            .collect();
        let arg = json!({
            "method": request.method,
            "url": request.url,
            "headers": headers,
            "body_sha256": sha256_hex(request.body),
        });
        let returned = self
            .call("sign", arg)
            .map_err(|e| signing::Error::Failed(e.to_string()))?;
        let not_headers = || {
            signing::Error::NotHeaders(
                self.bad_return("sign", "something other than a table of headers".into())
                    .to_string(),
            )
        };
        match returned {
            Value::Object(map) => map
                .into_iter()
                .map(|(name, value)| {
                    let Value::String(value) = value else {
                        return Err(not_headers());
                    };
                    Ok((name, value))
                })
                .collect(),
            Value::Null => Ok(Vec::new()),
            Value::Array(list) if list.is_empty() => Ok(Vec::new()),
            Value::Array(_) | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                Err(not_headers())
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic aborts the process (`docs/code-quality.md`, "Panics"), so no
    // holder can leave the lock poisoned.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
