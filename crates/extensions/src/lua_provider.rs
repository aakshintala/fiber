//! A provider's Lua (`docs/model-routing.md`, "Model discovery", "Signing a
//! request" and "Credentials"): its model list from `models()`, cached on
//! disk and refreshed in the background; its token from `credential()`,
//! refreshed off the request path before it expires; and `sign()`, the one
//! function on the request path, which sees the body's SHA-256 and never the
//! body.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use config::{Config, ModelData, ProviderData, Secret};
use contract::ErrorCode;
use contract::signing::{self, SignRequest, Signer};
use serde_json::{Map, Value, json};

use crate::host::sha256_hex;
use crate::{Error, LuaExtension};

mod login;

pub use login::{LoggedIn, LoginMethod, StoredLogin};

/// How long before its expiry a token is refreshed.
pub const REFRESH_BEFORE: Duration = Duration::from_secs(5 * 60);

/// One stored credential: `credentials/<credential>/<label>` in Fiber home.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CredentialPair {
    /// The stored credential's name: the provider's shared credential name
    /// when its data names one, and the provider's own name otherwise.
    pub credential: String,
    /// The session's credential label.
    pub label: String,
}

impl CredentialPair {
    /// The pair for `data` under `label`: `credential` is
    /// `data.credential_name`, else `data.name` (`docs/model-routing.md`,
    /// "Credentials").
    pub fn for_provider(data: &ProviderData, label: impl Into<String>) -> Self {
        Self {
            credential: data
                .credential_name
                .clone()
                .unwrap_or_else(|| data.name.clone()),
            label: label.into(),
        }
    }
}

/// One provider a Lua extension registered with `fiber.provider`.
pub struct LuaProvider {
    extension: Arc<LuaExtension>,
    name: String,
    models: Mutex<Option<Vec<ModelData>>>,
    /// One entry per stored credential and label, so a label switch gets
    /// that label's token, never the previous one (`docs/model-routing.md`,
    /// "Keys, tokens and OAuth").
    token: Mutex<BTreeMap<CredentialPair, TokenState>>,
    /// Completed call values, bounded per pair, plus the values currently
    /// handed to running calls (`docs/errors.md`, "The shape").
    used: Mutex<BTreeMap<CredentialPair, UsedState>>,
}

/// A token `credential()` returned, with the headers it returned with it: a
/// request never pairs a token with another token's headers
/// (`docs/model-routing.md`, "Keys, tokens and OAuth"). `Debug` prints no
/// value: `Secret` prints redacted.
#[derive(Debug, Clone)]
struct Token {
    secret: Secret,
    expires: SystemTime,
    headers: Vec<(String, Secret)>,
}

/// How many distinct values from completed calls `credentials()` retains
/// besides the cached and in-flight values (`docs/errors.md`, "The shape").
const USED_BOUND: usize = 16;

/// One pair's completed-call history and the values owned by calls still
/// running.
#[derive(Debug, Default)]
struct UsedState {
    /// Distinct values from completed calls, oldest first.
    completed: Vec<Secret>,
    /// One entry per value per call still running.
    active: Vec<Secret>,
}

impl UsedState {
    /// Records `values` as owned by a call that starts now.
    fn start(&mut self, values: &[Secret]) {
        self.active.extend_from_slice(values);
    }

    /// Moves one call's values from the in-flight set to the bounded history.
    fn finish(&mut self, values: &[Secret]) {
        for value in values {
            if let Some(held) = self
                .active
                .iter()
                .position(|held| held.expose() == value.expose())
            {
                self.active.remove(held);
            }
            self.completed
                .retain(|known| known.expose() != value.expose());
            self.completed.push(value.clone());
        }
        let excess = self.completed.len().saturating_sub(USED_BOUND);
        self.completed.drain(..excess);
    }
}

/// Removes one `sign` call's values from its pair's running set when the
/// call ends, however it ends.
struct UsedGuard {
    provider: Arc<LuaProvider>,
    pair: CredentialPair,
    values: Vec<Secret>,
}

impl Drop for UsedGuard {
    fn drop(&mut self) {
        self.provider.release_used(&self.pair, &self.values);
    }
}

#[derive(Default)]
struct TokenState {
    current: Option<Token>,
    refreshing: bool,
}

fn parse_credential_headers(
    headers: Option<&Value>,
) -> Result<Vec<(String, Secret)>, &'static str> {
    match headers {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(list)) if list.is_empty() => Ok(Vec::new()),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(name, value)| {
                let Value::String(value) = value else {
                    return Err("`headers` with a value that is not a string");
                };
                Ok((name.clone(), Secret::new(value.clone())))
            })
            .collect(),
        Some(Value::Array(_))
        | Some(Value::Bool(_))
        | Some(Value::Number(_))
        | Some(Value::String(_)) => {
            Err("something other than a table of header names to values for `headers`")
        }
    }
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
            used: Mutex::default(),
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
    pub fn signer(
        self: &Arc<Self>,
        pair: CredentialPair,
    ) -> Result<Option<Arc<dyn Signer>>, Error> {
        let functions = self.functions()?;
        let credential = functions.iter().any(|f| f == "credential");
        let sign = functions.iter().any(|f| f == "sign");
        if !credential && !sign {
            return Ok(None);
        }
        Ok(Some(Arc::new(LuaSigner {
            provider: Arc::clone(self),
            pair,
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

    /// Whether an age-checked refresh has nothing to do: `max_age` names
    /// one and the cached list is no older than it, or no cached copy
    /// exists (`models()` runs when the list is first needed). A list whose
    /// age reads exactly `max_age` is not stale.
    fn fresh(&self, max_age: Option<Duration>) -> bool {
        let Some(max) = max_age else {
            return false;
        };
        match config::model_cache_age(
            self.extension.home(),
            &self.name,
            self.extension.clock().wall(),
        ) {
            Ok(Some(age)) => age <= max,
            Ok(None) => true,
            Err(_) => false,
        }
    }

    /// Calls `models()` and parses what it returned, without writing the
    /// cache or storing anything. A return that is not a model list is
    /// `Error::BadReturn` with callback `<provider>.models`.
    pub fn list_models(&self) -> Result<(Vec<ModelData>, Value), Error> {
        let returned = self.call("models", Value::Null)?;
        let models: Vec<ModelData> = serde_json::from_value(returned.clone()).map_err(|e| {
            self.bad_return("models", format!("something other than a model list: {e}"))
        })?;
        Ok((models, returned))
    }

    fn discover(&self) -> Result<Vec<ModelData>, Error> {
        let (models, returned) = self.list_models()?;
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
    pub fn token(self: &Arc<Self>, pair: &CredentialPair) -> Result<Secret, Error> {
        Ok(self.current(pair)?.secret)
    }

    /// The token and headers `credential()` last returned for `pair`. With
    /// none, or one that has expired, it calls `credential()` and waits.
    /// Within [`REFRESH_BEFORE`] of expiry it returns what it has and calls
    /// `credential()` again on another thread, so a request never waits on
    /// a refresh. The token and its headers come from one cache read, so a
    /// request never pairs a new token with old headers
    /// (`docs/model-routing.md`, "Keys, tokens and OAuth").
    fn current(self: &Arc<Self>, pair: &CredentialPair) -> Result<Token, Error> {
        let mut tokens = lock(&self.token);
        let entry = tokens.entry(pair.clone()).or_default();
        if let Some(current) = &entry.current
            && let Ok(left) = current
                .expires
                .duration_since(self.extension.clock().wall())
            && !left.is_zero()
        {
            let current = current.clone();
            let due = left <= REFRESH_BEFORE;
            if due && !entry.refreshing {
                entry.refreshing = true;
                let this = Arc::clone(self);
                let pair = pair.clone();
                thread::spawn(move || {
                    let fresh = this.fetch_token(&pair);
                    let mut tokens = lock(&this.token);
                    let entry = tokens.entry(pair).or_default();
                    entry.refreshing = false;
                    if let Ok(fresh) = fresh {
                        entry.current = Some(fresh);
                    }
                });
            }
            return Ok(current);
        }
        let fresh = self.fetch_token(pair)?;
        entry.current = Some(fresh.clone());
        Ok(fresh)
    }

    /// The token `token()` returns; a failure as the signing seam carries it.
    pub fn credential_token(
        self: &Arc<Self>,
        pair: &CredentialPair,
    ) -> Result<Secret, signing::Error> {
        Ok(self.signing_token(pair)?.secret)
    }

    /// The token and headers `LuaSigner::sign` sends; a failure as the
    /// signing seam carries it.
    fn signing_token(self: &Arc<Self>, pair: &CredentialPair) -> Result<Token, signing::Error> {
        self.current(pair).map_err(|e| {
            let message = detail(&e);
            if matches!(e, Error::Unattended { .. }) {
                signing::Error::Unattended { message }
            } else {
                signing::Error::Credential {
                    code: e.code(),
                    message,
                }
            }
        })
    }

    /// The token `credential()` last returned, without fetching or
    /// refreshing when there is none. What `LuaSigner::credentials`
    /// redacts after `sign()` replaced the `authorization` header.
    pub fn cached_token(&self, pair: &CredentialPair) -> Option<Secret> {
        self.cached(pair).map(|token| token.secret)
    }

    /// The token and headers the cache holds now, without fetching or
    /// refreshing when there is none.
    fn cached(&self, pair: &CredentialPair) -> Option<Token> {
        lock(&self.token)
            .get(pair)
            .and_then(|entry| entry.current.clone())
    }

    /// Records `values` as owned by a `sign` call for `pair` that starts
    /// now, and moves them to completed history when the guard drops
    /// (`docs/errors.md`, "The shape").
    fn hold_used(self: &Arc<Self>, pair: &CredentialPair, values: &[Secret]) -> UsedGuard {
        lock(&self.used)
            .entry(pair.clone())
            .or_default()
            .start(values);
        UsedGuard {
            provider: Arc::clone(self),
            pair: pair.clone(),
            values: values.to_vec(),
        }
    }

    /// Forgets one ended `sign` call's `values` for `pair`.
    fn release_used(&self, pair: &CredentialPair, values: &[Secret]) {
        if let Some(entry) = lock(&self.used).get_mut(pair) {
            entry.finish(values);
        }
    }

    /// The values `credentials()` reports for `pair`: the cached values,
    /// completed history, and values of calls still running. Each value is
    /// listed once (`docs/errors.md`, "The shape").
    fn signing_secrets(&self, pair: &CredentialPair) -> Vec<Secret> {
        let mut secrets = Vec::new();
        if let Some(current) = self.cached(pair) {
            push_unique(&mut secrets, current.secret);
            for (_, value) in current.headers {
                push_unique(&mut secrets, value);
            }
        }
        if let Some(entry) = lock(&self.used).get(pair) {
            for value in entry.completed.iter().chain(&entry.active).cloned() {
                push_unique(&mut secrets, value);
            }
        }
        secrets
    }

    /// Calls `credential()` for `pair`, which returns `{ token, expires_at }`,
    /// the expiry in seconds since the Unix epoch, and optionally `headers`,
    /// a table of header names to values sent with the token on every signed
    /// request, and the login's `email`, which any other call ignores
    /// (`docs/extensions.md`, "What writing a provider looks like"). An
    /// empty table, and an empty table encoded as `[]`, mean no headers, as
    /// for `sign()`.
    fn fetch_token(&self, pair: &CredentialPair) -> Result<Token, Error> {
        let inner: Result<Token, Error> = (|| {
            let returned = self.extension.provider_credential(&self.name, pair)?;
            self.check_token(&returned)
        })();
        inner.map_err(Self::refresh_error)
    }

    /// Checks a `credential()` return the way every token entry does: a
    /// non-empty `token`, an `expires_at` in the future on the extension
    /// clock, and a `headers` table of names to values
    /// (`docs/extensions.md`, "What writing a provider looks like").
    fn check_token(&self, returned: &Value) -> Result<Token, Error> {
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
            return Err(self.bad_return("credential", "a token that has already expired".into()));
        }
        // A numeric `headers` key cannot be seen here: `to_json` turns
        // `{[42] = "v"}` into `{"42": "v"}` and `{[1] = "v"}` into
        // an array, so `returned` refuses those before conversion, and
        // this refuses the shapes that survive it (`docs/extensions.md`,
        // "What writing a provider looks like").
        let headers = parse_credential_headers(returned.get("headers"))
            .map_err(|why| self.bad_return("credential", why.to_owned()))?;
        Ok(Token {
            secret: Secret::new(token.to_owned()),
            expires,
            headers,
        })
    }

    /// Maps a token failure: a failed refresh keeps its own code, not
    /// `credential_failed` (`docs/model-routing.md`, "Keys, tokens and
    /// OAuth").
    fn refresh_error(e: Error) -> Error {
        if matches!(
            e,
            Error::RefreshRejected { .. }
                | Error::RefreshUnreachable { .. }
                | Error::Unattended { .. }
        ) {
            e
        } else {
            Error::Credential(Box::new(e))
        }
    }

    pub(crate) fn call(&self, function: &'static str, arg: Value) -> Result<Value, Error> {
        self.extension.provider_call(&self.name, function, arg)
    }

    pub(crate) fn bad_return(&self, function: &str, why: String) -> Error {
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
            .map_err(|e| signing::Error::Failed(detail(&e)))?;
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
/// a request"): the `credential()` token as `authorization: Bearer` and its
/// `headers`, then what `sign()` returns, which wins when it names
/// `authorization` itself.
struct LuaSigner {
    provider: Arc<LuaProvider>,
    pair: CredentialPair,
    credential: bool,
    sign: bool,
}

impl Signer for LuaSigner {
    fn sign(&self, request: &SignRequest<'_>) -> Result<Vec<(String, String)>, signing::Error> {
        let mut headers = Vec::new();
        let mut values = Vec::new();
        let mut _used = None;
        let current = if self.credential {
            let current = self.provider.signing_token(&self.pair)?;
            values.push(current.secret.clone());
            values.extend(current.headers.iter().map(|(_, value)| value.clone()));
            _used = Some(self.provider.hold_used(&self.pair, &values));
            Some(current)
        } else {
            None
        };

        let result: Result<Vec<(String, String)>, signing::Error> = (|| {
            if let Some(current) = &current {
                // A header `credential()` must not return: `authorization`,
                // and every header the request already carries, whatever
                // their case (`docs/model-routing.md`, "Keys, tokens and
                // OAuth").
                for (name, _) in &current.headers {
                    if name.eq_ignore_ascii_case("authorization")
                        || request
                            .headers
                            .iter()
                            .any(|(sent, _)| sent.eq_ignore_ascii_case(name))
                    {
                        return Err(signing::Error::Credential {
                            code: ErrorCode::CredentialFailed,
                            message: format!(
                                "`{}`'s credential() returned `headers` with a name Fiber already sends: {name:?}.",
                                self.provider.name(),
                            ),
                        });
                    }
                }
                headers.push((
                    "authorization".to_owned(),
                    format!("Bearer {}", current.secret.expose()),
                ));
                for (name, value) in &current.headers {
                    headers.push((name.clone(), value.expose().to_owned()));
                }
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
        })();
        result.map_err(|error| redact_signing_error(error, &values))
    }

    fn credentials(&self) -> Vec<Secret> {
        if !self.credential {
            return Vec::new();
        }
        self.provider.signing_secrets(&self.pair)
    }
}

/// Redacts a returned signing error with the values this call owned, before
/// the provider reads `credentials()` after `sign` returns (`docs/errors.md`,
/// "The shape").
fn redact_signing_error(error: signing::Error, values: &[Secret]) -> signing::Error {
    let redact = |message: String| redact_values(&message, values);
    match error {
        signing::Error::Failed(message) => signing::Error::Failed(redact(message)),
        signing::Error::NotHeaders(message) => signing::Error::NotHeaders(redact(message)),
        signing::Error::Credential { code, message } => signing::Error::Credential {
            code,
            message: redact(message),
        },
        signing::Error::Unattended { message } => signing::Error::Unattended {
            message: redact(message),
        },
    }
}

/// Replaces values owned by one call without scanning inserted placeholders.
/// Longer values match first so an overlapping shorter value cannot leave a
/// fragment; empty values are skipped so the scan always advances.
fn redact_values(message: &str, values: &[Secret]) -> String {
    let mut patterns: Vec<&str> = values
        .iter()
        .map(Secret::expose)
        .filter(|value| !value.is_empty())
        .collect();
    patterns.sort_by_key(|value| std::cmp::Reverse(value.len()));
    let mut redacted = String::with_capacity(message.len());
    let mut rest = message;
    while !rest.is_empty() {
        if let Some(value) = patterns.iter().find(|value| rest.starts_with(**value)) {
            redacted.push_str("[redacted]");
            rest = rest.strip_prefix(*value).unwrap_or("");
        } else if let Some(next) = rest.chars().next() {
            redacted.push(next);
            rest = rest.get(next.len_utf8()..).unwrap_or("");
        } else {
            break;
        }
    }
    redacted
}

/// Pushes `secret` unless one with the same value is already listed, so
/// `credentials()` names each value once for the redaction scan
/// (`docs/errors.md`, "The shape").
fn push_unique(secrets: &mut Vec<Secret>, secret: Secret) {
    if !secrets
        .iter()
        .any(|known| known.expose() == secret.expose())
    {
        secrets.push(secret);
    }
}

/// The first line of the extension's own text in `e`: Lua's own text,
/// what the refresh function raised, or Fiber's own `Display` for a
/// failure Fiber itself raised. Always one line.
fn detail(e: &Error) -> String {
    if let Error::Credential(inner) = e {
        return detail(inner);
    }
    let text = if let Error::Lua { message, .. }
    | Error::RefreshRejected { message, .. }
    | Error::RefreshUnreachable { message, .. } = e
    {
        message.clone()
    } else {
        e.to_string()
    };
    text.lines().next().unwrap_or("").to_owned()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic aborts the process (`docs/code-quality.md`, "Panics"), so no
    // holder can leave the lock poisoned.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "lua_provider_tests.rs"]
mod tests;
