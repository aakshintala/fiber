//! A provider's quota (`docs/tools.md`, "Provider quota"; `docs/model-routing.md`,
//! "Quota"): one cache per credential label holding its last value, when it
//! was fetched and any fetch in flight, refreshed off the request path by a
//! model call and read by whoever shows quota.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use config::Secret;
use contract::diag::Observe;
use contract::provider::{ModelCall, ModelRequest, Provider, ToolDefinition};
use serde_json::{Map, Value, json};
use support::clock::Parker;

use super::{CredentialPair, lock};
use crate::{Error, LuaProvider};

/// A fetch is due when the cache is older than this. A cache whose age reads
/// exactly this is still fresh.
const FRESH_FOR: Duration = Duration::from_secs(5 * 60);

/// A fetch is overdue at exactly this age, and a reader waits no longer than
/// this on one flight.
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// How much quota one credential label has left: windows of percent used, or
/// credit remaining.
#[derive(Debug, Clone, PartialEq)]
pub enum Quota {
    /// Usage windows, in the vendor's order. An empty list is allowed.
    Windows(Vec<Window>),
    /// Credit remaining, and the limit where the vendor reports one.
    Credit {
        /// What is left.
        remaining: f64,
        /// The limit it is left of, where there is one.
        limit: Option<f64>,
    },
}

/// One usage window: its name and how used it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// The window's name, as the vendor reports it.
    pub name: String,
    /// How used the window is. A stored value holds only `Percent`; `Reset`
    /// appears only in a read, once the reset time has passed.
    pub used: WindowUse,
}

/// How used one window is.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowUse {
    /// Percent used, and the reset time where the vendor reports one.
    Percent {
        /// Percent used, at or above 0.
        used: f64,
        /// When the window resets, where the vendor reports one.
        resets_at: Option<SystemTime>,
    },
    /// The reset time has passed: the window reads as reset.
    Reset,
}

/// What the model sees for one credential label (`docs/tools.md`, "What the
/// model sees").
#[derive(Debug, Clone, PartialEq)]
pub enum QuotaEntry {
    /// The last fetch's value, with reset windows mapped.
    Reported(Quota),
    /// The provider reports no quota: its package registers no `quota()`, or
    /// its `quota()` returned nothing.
    NoQuota,
    /// The fetch failed or timed out, with the last value and its age where
    /// there is one. Never an error.
    Unreachable {
        /// The last value and its age on the monotonic clock, if any.
        last: Option<(Quota, Duration)>,
    },
}

/// One label's cache: its last successful value with its fetch time, whether
/// the last fetch failed, and the fetch in flight, if any.
#[derive(Debug, Default)]
struct Cache {
    /// The last successful fetch: the value (`None` for a `nil` return, which
    /// reports no quota) and when it finished.
    fetched: Option<(Option<Quota>, Instant)>,
    /// The last fetch failed, and no fetch succeeded since.
    failed: bool,
    /// The fetch in flight, if any: one flight per label at a time.
    flight: Option<Flight>,
    /// The next flight's id.
    next_id: u64,
}

/// A fetch in flight: its id and when it started on the monotonic clock.
#[derive(Debug, Clone, Copy)]
struct Flight {
    id: u64,
    started: Instant,
}

/// One fetch flight to wait on: the flight a force-start found in flight
/// or started, with that flight's own deadline. Opaque to the caller,
/// which never names an id or a time: a concurrent replacement flight
/// ends a wait on one of these at once instead of extending it.
#[derive(Debug, Clone, Copy)]
pub struct QuotaFlight {
    id: u64,
    deadline: Instant,
}

/// The per-label quota caches of one provider, sharing one parker: every
/// flight end bumps it, so a parked reader wakes.
pub(super) struct Quotas {
    state: Mutex<BTreeMap<CredentialPair, Cache>>,
    parker: Arc<Parker>,
}

impl Quotas {
    /// Empty caches, waking whoever parks on them whenever `clock` moves.
    pub(super) fn new(clock: &dyn contract::clock::Clock) -> Self {
        let parker = Arc::new(Parker::new());
        let wake: Arc<dyn contract::clock::Wake> = parker.clone();
        clock.subscribe(Arc::downgrade(&wake));
        Self {
            state: Mutex::default(),
            parker,
        }
    }
}

/// Checks a `quota()` return: nothing is no quota; `{ windows = ... }` is
/// windows; `{ credit = ... }` is credit. Anything else (both keys, neither
/// key, a scalar, a wrong field type, a non-finite or negative percent) is a
/// bad return, which the caller reports as a failed fetch. An empty Lua
/// table arrives as `[]`, so it is a bad return too, while an empty windows
/// list is allowed.
fn parse_quota(returned: &Value) -> Result<Option<Quota>, &'static str> {
    if returned.is_null() {
        return Ok(None);
    }
    let Value::Object(map) = returned else {
        return Err("something other than a table with `windows` or `credit`");
    };
    let windows = map.get("windows").filter(|value| !value.is_null());
    let credit = map.get("credit").filter(|value| !value.is_null());
    match (windows, credit) {
        (Some(windows), None) => Ok(Some(Quota::Windows(parse_windows(windows)?))),
        (None, Some(credit)) => Ok(Some(parse_credit(credit)?)),
        (Some(_), Some(_)) => Err("a table with both `windows` and `credit`"),
        (None, None) => Err("a table with neither `windows` nor `credit`"),
    }
}

/// Checks a `windows` list: each entry names a non-empty `name` and a finite
/// `used_percent` at or above 0, with an optional `resets_at` in Unix
/// seconds. A `resets_at` the epoch cannot hold is a bad return, not a
/// panic.
fn parse_windows(value: &Value) -> Result<Vec<Window>, &'static str> {
    let Value::Array(list) = value else {
        return Err("`windows` that is not a list");
    };
    list.iter()
        .map(|entry| {
            let Value::Object(map) = entry else {
                return Err("a window that is not a table with `name` and `used_percent`");
            };
            let name = map
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or("a window with a missing or empty `name`")?;
            let used = map
                .get("used_percent")
                .and_then(Value::as_f64)
                .filter(|used| used.is_finite() && *used >= 0.0)
                .ok_or("a window with a `used_percent` that is not a number at or above 0")?;
            let resets_at = match map.get("resets_at").filter(|value| !value.is_null()) {
                None => None,
                Some(value) => Some(
                    value
                        .as_f64()
                        .and_then(|secs| Duration::try_from_secs_f64(secs).ok())
                        .and_then(|secs| UNIX_EPOCH.checked_add(secs))
                        .ok_or("a window with a `resets_at` that is not Unix seconds")?,
                ),
            };
            Ok(Window {
                name: name.to_owned(),
                used: WindowUse::Percent { used, resets_at },
            })
        })
        .collect()
}

/// Checks a `credit` table: a finite `remaining`, with an optional finite
/// `limit` at or above 0.
fn parse_credit(value: &Value) -> Result<Quota, &'static str> {
    let Value::Object(map) = value else {
        return Err("`credit` that is not a table with `remaining`");
    };
    let remaining = map
        .get("remaining")
        .and_then(Value::as_f64)
        .filter(|remaining| remaining.is_finite())
        .ok_or("`credit` with a `remaining` that is not a number")?;
    let limit = match map.get("limit").filter(|value| !value.is_null()) {
        None => None,
        Some(value) => Some(
            value
                .as_f64()
                .filter(|limit| limit.is_finite() && *limit >= 0.0)
                .ok_or("`credit` with a `limit` that is not a number at or above 0")?,
        ),
    };
    Ok(Quota::Credit { remaining, limit })
}

/// Marks an overdue flight failed, clears the in-flight slot and wakes the
/// readers. A flight is overdue at exactly `FETCH_TIMEOUT`. True when it
/// expired one.
fn expire_overdue(cache: &mut Cache, now: Instant, parker: &Parker) -> bool {
    let overdue = cache
        .flight
        .as_ref()
        .is_some_and(|flight| now.saturating_duration_since(flight.started) >= FETCH_TIMEOUT);
    if overdue {
        cache.flight = None;
        cache.failed = true;
        parker.bump();
    }
    overdue
}

/// What the cache holds now: the value with reset windows mapped, or
/// `unreachable` when the last fetch failed. The stored value keeps the
/// vendor's figures; only the read maps a window whose reset time is at or
/// before `wall` to reset.
fn entry(cache: &Cache, now: Instant, wall: SystemTime) -> QuotaEntry {
    if cache.failed {
        let last = cache.fetched.as_ref().and_then(|(value, at)| {
            value
                .clone()
                .map(|quota| (reset_view(&quota, wall), now.saturating_duration_since(*at)))
        });
        return QuotaEntry::Unreachable { last };
    }
    match &cache.fetched {
        Some((Some(quota), _)) => QuotaEntry::Reported(reset_view(quota, wall)),
        Some((None, _)) => QuotaEntry::NoQuota,
        None => QuotaEntry::Unreachable { last: None },
    }
}

/// `quota` with each window whose reset time is at or before `wall` shown as
/// reset.
fn reset_view(quota: &Quota, wall: SystemTime) -> Quota {
    match quota {
        Quota::Windows(windows) => Quota::Windows(
            windows
                .iter()
                .map(|window| Window {
                    name: window.name.clone(),
                    used: match &window.used {
                        WindowUse::Percent {
                            resets_at: Some(at),
                            ..
                        } if *at <= wall => WindowUse::Reset,
                        WindowUse::Percent { .. } => window.used.clone(),
                        WindowUse::Reset => WindowUse::Reset,
                    },
                })
                .collect(),
        ),
        quota @ Quota::Credit { .. } => quota.clone(),
    }
}

impl LuaProvider {
    /// `inner`, the provider a session's calls go through, fetching this
    /// provider's quota for `pair` off the request path on every call;
    /// `inner` itself when this provider did not register `quota`. `key` is
    /// the label's resolved key, sent to `quota()` with the fetch and
    /// absent when the label has none. The cache stays keyed by `pair`
    /// alone: the key is a function of the label.
    pub fn with_quota(
        self: &Arc<Self>,
        inner: Arc<dyn Provider>,
        pair: CredentialPair,
        key: Option<Secret>,
    ) -> Result<Arc<dyn Provider>, Error> {
        if !self.registers("quota")? {
            return Ok(inner);
        }
        Ok(Arc::new(Quoted {
            inner,
            provider: Arc::clone(self),
            pair,
            key,
            observe: None,
        }))
    }

    /// As [`LuaProvider::with_quota`], also wrapping `observe`: the
    /// returned provider's ordinary call builds through the factory at the
    /// first attempt, and the returned factory triggers the same shared
    /// preparation before building through it at the caller's attempt. A
    /// provider that registered no `quota` keeps both inputs unchanged.
    pub fn with_quota_observed(
        self: &Arc<Self>,
        inner: Arc<dyn Provider>,
        observe: Option<Observe>,
        pair: CredentialPair,
        key: Option<Secret>,
    ) -> Result<(Arc<dyn Provider>, Option<Observe>), Error> {
        if !self.registers("quota")? {
            return Ok((inner, observe));
        }
        let raw = observe;
        let factory = raw.clone().map(|observe| {
            let provider = Arc::clone(self);
            let pair = pair.clone();
            let key = key.clone();
            let factory: Observe = Arc::new(move |request, attempt| {
                provider.refresh_quota(&pair, key.as_ref());
                observe(request, attempt)
            });
            factory
        });
        let quoted = Arc::new(Quoted {
            inner,
            provider: Arc::clone(self),
            pair,
            key,
            observe: raw,
        });
        Ok((quoted, factory))
    }

    /// Starts a fetch for `pair` when one is due and none is in flight, on
    /// its own thread, sending the `key` the caller passes. Never waits;
    /// never returns an error. A fetch is due
    /// when nothing was ever fetched, or the last fetch is older than
    /// `FRESH_FOR`; a failure keeps the last fetch time, so the next trigger
    /// retries. The lock is never held across the `quota()` call.
    pub fn refresh_quota(self: &Arc<Self>, pair: &CredentialPair, key: Option<&Secret>) {
        let id = {
            let mut states = lock(&self.quotas.state);
            let now = self.extension.clock().now();
            let cache = states.entry(pair.clone()).or_default();
            expire_overdue(cache, now, &self.quotas.parker);
            let due = match cache.fetched {
                None => true,
                Some((_, at)) => now.saturating_duration_since(at) > FRESH_FOR,
            };
            if !due || cache.flight.is_some() {
                return;
            }
            let id = cache.next_id;
            cache.next_id = cache.next_id.wrapping_add(1);
            cache.flight = Some(Flight { id, started: now });
            id
        };
        self.start_flight(pair, key.cloned(), id);
    }

    /// Spawns the fetch thread for flight `id`: calls `quota()`, then
    /// stores the result when its flight still owns the slot. Both
    /// triggers share this one spawn.
    fn start_flight(self: &Arc<Self>, pair: &CredentialPair, key: Option<Secret>, id: u64) {
        let this = Arc::clone(self);
        let pair = pair.clone();
        thread::spawn(move || {
            let fetched = this
                .fetch_quota(&pair, key.as_ref())
                .map(|(quota, _)| quota)
                .map_err(|_| ());
            this.finish_quota(&pair, id, fetched);
        });
    }

    /// Starts a fetch for `pair` when none is in flight, whatever the
    /// cache's age, on its own thread, sending the `key` the caller
    /// passes, and returns the flight to wait on: the shared flight when
    /// one is already in flight, else the new one with its own deadline.
    /// Never waits; never returns an error. An overdue flight is expired
    /// first, so a wait on the returned flight is always bounded. The
    /// lock is never held across the `quota()` call.
    pub fn force_start_quota(
        self: &Arc<Self>,
        pair: &CredentialPair,
        key: Option<&Secret>,
    ) -> QuotaFlight {
        let (id, started, spawned) = {
            let mut states = lock(&self.quotas.state);
            let now = self.extension.clock().now();
            let cache = states.entry(pair.clone()).or_default();
            expire_overdue(cache, now, &self.quotas.parker);
            match &cache.flight {
                Some(flight) => (flight.id, flight.started, false),
                None => {
                    let id = cache.next_id;
                    cache.next_id = cache.next_id.wrapping_add(1);
                    cache.flight = Some(Flight { id, started: now });
                    (id, now, true)
                }
            }
        };
        if spawned {
            self.start_flight(pair, key.cloned(), id);
        }
        QuotaFlight {
            id,
            deadline: started.checked_add(FETCH_TIMEOUT).unwrap_or(started),
        }
    }

    /// Waits for the flight `wait` names to end, bounded by that flight's
    /// own deadline, and reads the cache then. A flight that ended before
    /// the wait answers at once; a replacement flight ends the wait at
    /// once too, reported as it stands, so a concurrent new flight never
    /// extends the original wait. Past the deadline the flight is expired
    /// and the entry is `unreachable`, as in the shared reader.
    pub fn wait_quota(self: &Arc<Self>, pair: &CredentialPair, wait: QuotaFlight) -> QuotaEntry {
        loop {
            // Read before the state check, so a bump between the check and
            // the park wakes the park at once instead of being lost.
            let seen = self.quotas.parker.generation();
            let clock = self.extension.clock();
            enum Next {
                Return(QuotaEntry),
                Park(Instant),
            }
            let next = {
                let mut states = lock(&self.quotas.state);
                let now = clock.now();
                let cache = states.entry(pair.clone()).or_default();
                expire_overdue(cache, now, &self.quotas.parker);
                match &cache.flight {
                    None => Next::Return(entry(cache, now, clock.wall())),
                    Some(flight) if flight.id != wait.id => {
                        Next::Return(entry(cache, now, clock.wall()))
                    }
                    Some(_) => Next::Park(wait.deadline),
                }
            };
            match next {
                Next::Return(found) => return found,
                Next::Park(deadline) => {
                    self.quotas.parker.park(clock, Some(deadline), seen);
                }
            }
        }
    }

    /// Reads the cache at once, starting a fetch when due. The caller must
    /// already know this provider registers `quota()`. Never parks or calls
    /// Lua on the caller's thread; a first fetch reads unreachable until it
    /// supplies a value.
    pub fn quota_now(self: &Arc<Self>, pair: &CredentialPair, key: Option<&Secret>) -> QuotaEntry {
        self.refresh_quota(pair, key);
        self.quota_cached(pair)
    }

    /// Reads only the existing quota cache, without starting a fetch or waiting.
    pub fn quota_cached(self: &Arc<Self>, pair: &CredentialPair) -> QuotaEntry {
        let mut states = lock(&self.quotas.state);
        let clock = self.extension.clock();
        let now = clock.now();
        let cache = states.entry(pair.clone()).or_default();
        expire_overdue(cache, now, &self.quotas.parker);
        entry(cache, now, clock.wall())
    }

    /// Whether `pair`'s first fetch is still in flight with no last value:
    /// true when nothing was ever fetched and the last fetch did not fail,
    /// after expiring an overdue flight. Never waits and never calls Lua;
    /// the caller reads it before `quota_now`, so a value landing between
    /// the two reads shows the value, never `unreachable`
    /// (`docs/tools.md`, "What the model sees").
    pub fn quota_pending(self: &Arc<Self>, pair: &CredentialPair) -> bool {
        let mut states = lock(&self.quotas.state);
        let now = self.extension.clock().now();
        let cache = states.entry(pair.clone()).or_default();
        expire_overdue(cache, now, &self.quotas.parker);
        cache.fetched.is_none() && !cache.failed
    }

    /// What the model sees for `pair`: no quota when the package registers
    /// none, else the cache, fetching with `key` when due and waiting for
    /// the flight in
    /// flight, bounded by that flight's own deadline. A package that failed
    /// to start reads `unreachable` with no last value. A failure is
    /// `unreachable`, never an error.
    pub fn quota(self: &Arc<Self>, pair: &CredentialPair, key: Option<&Secret>) -> QuotaEntry {
        match self.registers("quota") {
            Err(_) => return QuotaEntry::Unreachable { last: None },
            Ok(false) => return QuotaEntry::NoQuota,
            Ok(true) => {}
        }
        self.refresh_quota(pair, key);
        // The flight this read waits on: read once, and waited on at most
        // until its own deadline, so a chain of new flights cannot hold one
        // reader past it.
        let mut saw: Option<(u64, Instant)> = None;
        loop {
            // Read before the state check, so a bump between the check and
            // the park wakes the park at once instead of being lost.
            let seen = self.quotas.parker.generation();
            let clock = self.extension.clock();
            enum Next {
                Return(QuotaEntry),
                Park(Instant),
            }
            let next = {
                let mut states = lock(&self.quotas.state);
                let now = clock.now();
                let cache = states.entry(pair.clone()).or_default();
                expire_overdue(cache, now, &self.quotas.parker);
                match &cache.flight {
                    None => Next::Return(entry(cache, now, clock.wall())),
                    Some(flight) => match saw {
                        Some((id, _)) if id != flight.id => {
                            Next::Return(entry(cache, now, clock.wall()))
                        }
                        _ => {
                            let deadline = flight.started.checked_add(FETCH_TIMEOUT).unwrap_or(now);
                            saw = Some((flight.id, deadline));
                            Next::Park(deadline)
                        }
                    },
                }
            };
            match next {
                Next::Return(found) => return found,
                Next::Park(deadline) => {
                    self.quotas.parker.park(clock, Some(deadline), seen);
                }
            }
        }
    }

    /// Calls `quota({ label, credential, key, token, headers })` and checks
    /// what it returned, returning the checked quota with the raw return.
    /// `key` is the label's resolved key, absent from the argument when none
    /// was given. For a provider that registers `credential()`, `token` and
    /// `headers` are what `credential()` last returned for the label, read
    /// through `current` exactly as a request's are: refreshed when absent
    /// or expired, refreshed in the background when close to expiry. A
    /// `current` failure is an error here, which the cache path reports as
    /// a failed fetch. A provider with no `credential()` gets no
    /// `token` or `headers`, so `host.oauth.refresh` refuses inside its
    /// `quota()` as it does for `models()` and `cost()`; only `credential()`
    /// refreshes a stored credential. Uncached: it never touches `Quotas`.
    pub fn fetch_quota(
        self: &Arc<Self>,
        pair: &CredentialPair,
        key: Option<&Secret>,
    ) -> Result<(Option<Quota>, Value), Error> {
        let mut arg = Map::new();
        arg.insert("label".into(), json!(pair.label));
        arg.insert("credential".into(), json!(pair.credential));
        if let Some(key) = key {
            arg.insert("key".into(), json!(key.expose()));
        }
        if self.registers("credential")? {
            // The token and headers `credential()` last returned, read as
            // a request's are; built as `credential_value` builds them.
            let current = self.current(pair)?;
            arg.insert("token".into(), json!(current.secret.expose()));
            let headers: Map<String, Value> = current
                .headers
                .iter()
                .map(|(name, value)| (name.clone(), Value::String(value.expose().to_owned())))
                .collect();
            arg.insert("headers".into(), Value::Object(headers));
        }
        let returned = self.call("quota", Value::Object(arg), None)?;
        let parsed =
            parse_quota(&returned).map_err(|why| self.bad_return("quota", why.to_owned()))?;
        Ok((parsed, returned))
    }

    /// Stores a fetch's result when its id still owns the in-flight slot and
    /// the flight is not overdue; a result for an id no longer in flight is
    /// discarded, so a late value never replaces the `unreachable` entry. A
    /// flight overdue at exactly `FETCH_TIMEOUT` is expired instead: the
    /// slot clears, the fetch is marked failed and the result is discarded,
    /// with no intervening reader or trigger needed. A success stores the
    /// value and clears the failure; a failure keeps the last value and its
    /// fetch time. Every flight end wakes the readers.
    fn finish_quota(&self, pair: &CredentialPair, id: u64, fetched: Result<Option<Quota>, ()>) {
        let now = self.extension.clock().now();
        {
            let mut states = lock(&self.quotas.state);
            let Some(cache) = states.get_mut(pair) else {
                return;
            };
            let Some(flight) = cache.flight else {
                return;
            };
            if flight.id != id {
                return;
            }
            if now.saturating_duration_since(flight.started) >= FETCH_TIMEOUT {
                cache.flight = None;
                cache.failed = true;
            } else {
                cache.flight = None;
                match fetched {
                    Ok(value) => {
                        cache.fetched = Some((value, now));
                        cache.failed = false;
                    }
                    Err(()) => {
                        cache.failed = true;
                    }
                }
            }
        }
        self.quotas.parker.bump();
    }
}

/// A provider whose calls are another's, fetching quota off the request path
/// on every call. Every [`Provider`] method is the inner provider's, so the
/// request bytes are unchanged.
struct Quoted {
    inner: Arc<dyn Provider>,
    provider: Arc<LuaProvider>,
    pair: CredentialPair,
    key: Option<Secret>,
    /// The observed factory the ordinary call builds through at the first
    /// attempt; `None` calls the inner provider directly.
    observe: Option<Observe>,
}

impl Quoted {
    /// Triggers the quota refresh for this attempt: off the request path,
    /// it only locks the cache and, when due, spawns a fetch thread. It
    /// never waits, parks, or calls `registers`. The ordinary call and the
    /// observed factory share this one preparation.
    fn prepare(&self) {
        self.provider.refresh_quota(&self.pair, self.key.as_ref());
    }
}

impl Provider for Quoted {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        self.prepare();
        match &self.observe {
            // The factory's own attempt for a debug-provider call made
            // outside the loop's factories.
            Some(observe) => observe(request, NonZeroU32::MIN),
            None => self.inner.call(request),
        }
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        self.inner.wire_tools(tools)
    }

    fn warms(&self, request: &ModelRequest) -> bool {
        self.inner.warms(request)
    }

    fn defers(&self) -> bool {
        self.inner.defers()
    }

    fn cost_lookup(&self) -> Option<Arc<dyn contract::provider::CostLookup>> {
        self.inner.cost_lookup()
    }
}

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;
