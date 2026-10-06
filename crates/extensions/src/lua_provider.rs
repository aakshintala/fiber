//! A provider's Lua (`docs/model-routing.md`, "Model discovery", "Signing a
//! request" and "Credentials"): its model list from `models()`, cached on
//! disk and refreshed in the background; its token from `credential()`,
//! refreshed off the request path before it expires; and `sign()`, the one
//! function on the request path, which sees the body's SHA-256 and never the
//! body.

use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use config::{Config, ModelData, ProviderData, Secret};
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

    /// The provider's name, as `fiber.provider` registered it.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The registering extension's package directory, which its models'
    /// `prompt_addendum` paths resolve against.
    pub(crate) fn dir(&self) -> &Path {
        self.extension.dir()
    }

    /// The functions `fiber.provider` registered for this provider, sorted.
    pub fn functions(&self) -> Result<Vec<String>, Error> {
        self.extension.provider_functions(&self.name)
    }

    /// Whether `fiber.provider` registered `function` for this provider.
    pub fn registers(&self, function: &str) -> Result<bool, Error> {
        Ok(self.functions()?.iter().any(|name| name == function))
    }

    /// Signs this provider's requests: `None` when it registered neither
    /// `credential` nor `sign`.
    pub fn signer(self: &Arc<Self>) -> Result<Option<Arc<dyn Signer>>, Error> {
        let functions = self.functions()?;
        let credential = functions.iter().any(|f| f == "credential");
        let sign = functions.iter().any(|f| f == "sign");
        if !credential && !sign {
            return Ok(None);
        }
        Ok(Some(Arc::new(LuaSigner {
            provider: Arc::clone(self),
            credential,
            sign,
        })))
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

    /// Whether a cached model list waits on disk: what [`LuaProvider::models`]
    /// serves without calling `models()`.
    pub fn has_model_cache(&self) -> bool {
        matches!(
            config::read_model_cache(self.extension.home(), &self.name),
            Ok(Some(_))
        )
    }

    /// Whether this provider has a credential to refresh with: it registers
    /// `credential`, or the session's key lookup finds one for `data`
    /// (`docs/model-routing.md`, "Model discovery"). `data` is the data
    /// file's, else one naming only the provider: the same lookup a session
    /// uses for its key.
    pub fn has_credential(&self, config: &Config, data: &ProviderData) -> bool {
        self.registers("credential").unwrap_or(false)
            || config
                .credential(data, &config.credential_label(data))
                .is_ok()
    }

    /// Calls `models()` on its own thread, unless `max_age` names one and
    /// the cached list is no older than it; `None` then, and no `models()`
    /// call. A provider that did not register `models` never starts: no
    /// lock, no thread, no call. `None` for `max_age` always runs, whatever
    /// the cache holds.
    /// Across the processes sharing a Fiber home a provider refreshes once:
    /// an exclusive lock on its lock file is held for the whole refresh,
    /// and a provider already refreshing is not started again. The age is
    /// re-read under the lock, so a refresh that finished between the
    /// first check and the lock is skipped too. A failing `models()` keeps
    /// the old cache (`docs/model-routing.md`, "Model discovery").
    pub fn refresh(
        self: &Arc<Self>,
        max_age: Option<Duration>,
    ) -> Option<JoinHandle<Result<Vec<ModelData>, Error>>> {
        if !self.registers("models").unwrap_or(false) {
            return None;
        }
        if self.fresh(max_age) {
            return None;
        }
        let home = self.extension.home();
        let path = config::model_cache_lock_file(home, &self.name).ok()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        // The lock is the file description, never its bytes: the file
        // stays empty, and is created when missing.
        let lock = File::options()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .ok()?;
        // Held elsewhere, by this process or another: skipped, not queued.
        if lock.try_lock().is_err() {
            return None;
        }
        if self.fresh(max_age) {
            return None;
        }
        let this = Arc::clone(self);
        Some(thread::spawn(move || {
            // Held until the cache is written: dropping the file
            // releases the lock.
            let _held = lock;
            this.discover()
        }))
    }

    /// Whether `max_age` names one and the cached list is no older than
    /// it. A list whose age reads exactly `max_age` is not stale.
    fn fresh(&self, max_age: Option<Duration>) -> bool {
        let Some(max) = max_age else {
            return false;
        };
        matches!(
            config::model_cache_age(
                self.extension.home(),
                &self.name,
                self.extension.clock().wall()
            ),
            Ok(Some(age)) if age <= max
        )
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
            && let Ok(left) = expires.duration_since(self.extension.clock().wall())
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
        let inner: Result<(Secret, SystemTime), Error> = (|| {
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
            if expires <= self.extension.clock().wall() {
                return Err(
                    self.bad_return("credential", "a token that has already expired".into())
                );
            }
            Ok((Secret::new(token.to_owned()), expires))
        })();
        // A failed refresh has its own code, not `credential_failed`.
        inner.map_err(|e| {
            if matches!(
                e,
                Error::RefreshRejected { .. } | Error::RefreshUnreachable { .. }
            ) {
                e
            } else {
                Error::Credential(Box::new(e))
            }
        })
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

/// One Lua provider's requests, signed (`docs/model-routing.md`, "Signing
/// a request"): the `credential()` token as `authorization: Bearer`, then
/// what `sign()` returns, which wins when it names `authorization` itself.
struct LuaSigner {
    provider: Arc<LuaProvider>,
    credential: bool,
    sign: bool,
}

impl Signer for LuaSigner {
    fn sign(&self, request: &SignRequest<'_>) -> Result<Vec<(String, String)>, signing::Error> {
        let mut headers = Vec::new();
        if self.credential {
            let token = self
                .provider
                .token()
                .map_err(|e| signing::Error::Credential {
                    code: e.code(),
                    message: e.to_string(),
                })?;
            headers.push((
                "authorization".to_owned(),
                format!("Bearer {}", token.expose()),
            ));
        }
        if self.sign {
            let seen: Vec<(String, String)> = request
                .headers
                .iter()
                .cloned()
                .chain(headers.iter().cloned())
                .collect();
            let signed = self.provider.sign(&SignRequest {
                method: request.method,
                url: request.url,
                headers: &seen,
                body: request.body,
            })?;
            if signed
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            {
                headers.retain(|(name, _)| !name.eq_ignore_ascii_case("authorization"));
            }
            headers.extend(signed);
        }
        Ok(headers)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic aborts the process (`docs/code-quality.md`, "Panics"), so no
    // holder can leave the lock poisoned.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
