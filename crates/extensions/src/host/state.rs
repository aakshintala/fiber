//! A session's extension-state view and the lines awaiting its log writer.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use contract::events::{Event, ExtensionStateSet, ExtensionStateUnset, OnFork};
use contract::hook::{StateLine, StateSeed};
use serde_json::Value;

pub(crate) const MAX_BYTES: usize = 65_536;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Scope {
    Other,
    Hook(u64),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StateError {
    TooLarge { bytes: usize },
    Closing,
}

#[derive(Clone)]
struct Write {
    value: Option<Value>,
    rule: OnFork,
    landing: u64,
}

#[derive(Default)]
struct Key {
    base: Option<Write>,
    last_logged: u64,
}

struct Pending {
    line: StateLine,
    write: Write,
}

#[derive(Default)]
struct Store {
    keys: BTreeMap<String, Key>,
    calls: BTreeMap<(u64, u64), Vec<Pending>>,
}

impl Store {
    fn view(&self, key: &str) -> Option<&Write> {
        self.keys
            .get(key)
            .and_then(|entry| entry.base.as_ref())
            .into_iter()
            .chain(
                self.calls
                    .values()
                    .flatten()
                    .filter(|pending| pending.line.key == key)
                    .map(|pending| &pending.write),
            )
            .max_by_key(|write| write.landing)
    }
}

#[derive(Default)]
struct Inner {
    extensions: BTreeMap<String, Store>,
    generation: u64,
    landing: u64,
    closed: bool,
    outbox: Vec<StateLine>,
}

pub(crate) struct StateRegistry {
    inner: Arc<Mutex<Inner>>,
}

impl StateRegistry {
    pub(crate) fn new(seed: StateSeed) -> Arc<Self> {
        let extensions = seed
            .into_iter()
            .map(|(extension, keys)| {
                let keys = keys
                    .into_iter()
                    .map(|(key, (value, rule))| {
                        (
                            key,
                            Key {
                                base: Some(Write {
                                    value: Some(value),
                                    rule,
                                    landing: 0,
                                }),
                                last_logged: 0,
                            },
                        )
                    })
                    .collect();
                (
                    extension,
                    Store {
                        keys,
                        ..Store::default()
                    },
                )
            })
            .collect();
        Arc::new(Self {
            inner: Arc::new(Mutex::new(Inner {
                extensions,
                ..Inner::default()
            })),
        })
    }

    pub(crate) fn standalone() -> Arc<Self> {
        Self::new(StateSeed::new())
    }

    pub(crate) fn store(&self, extension: &str) -> Arc<ExtensionState> {
        let mut inner = self.lock();
        inner.extensions.entry(extension.into()).or_default();
        inner.generation += 1;
        Arc::new(ExtensionState {
            inner: self.inner.clone(),
            extension: extension.into(),
            generation: inner.generation,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn take_outbox(&self) -> Vec<Event> {
        let mut inner = self.lock();
        let mut lines = std::mem::take(&mut inner.outbox);
        lines.sort_by_key(|line| line.landing);
        admit(&mut inner, lines)
    }

    pub(crate) fn admit(&self, lines: Vec<StateLine>) -> Vec<Event> {
        admit(&mut self.lock(), lines)
    }

    pub(crate) fn queue(&self, lines: Vec<StateLine>) {
        self.lock().outbox.extend(lines);
    }

    pub(crate) fn close(&self) {
        self.lock().closed = true;
    }
}

fn admit(inner: &mut Inner, lines: Vec<StateLine>) -> Vec<Event> {
    let mut events = Vec::new();
    for line in lines {
        let key = inner
            .extensions
            .entry(line.extension)
            .or_default()
            .keys
            .entry(line.key)
            .or_default();
        if line.landing >= key.last_logged {
            key.last_logged = line.landing;
            events.push(line.event);
        }
    }
    events
}

pub(crate) struct ExtensionState {
    inner: Arc<Mutex<Inner>>,
    extension: String,
    generation: u64,
}

impl Default for ExtensionState {
    fn default() -> Self {
        Self {
            inner: StateRegistry::standalone().inner.clone(),
            extension: String::new(),
            generation: 0,
        }
    }
}

impl ExtensionState {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn get(&self, key: &str) -> Option<Value> {
        self.lock()
            .extensions
            .get(&self.extension)?
            .view(key)?
            .value
            .clone()
    }

    pub(crate) fn keys(&self) -> Vec<String> {
        let inner = self.lock();
        let Some(store) = inner.extensions.get(&self.extension) else {
            return Vec::new();
        };
        store
            .keys
            .keys()
            .filter(|key| store.view(key).is_some_and(|write| write.value.is_some()))
            .cloned()
            .collect()
    }

    pub(crate) fn begin(&self, call: u64) {
        self.lock()
            .extensions
            .entry(self.extension.clone())
            .or_default()
            .calls
            .insert((self.generation, call), Vec::new());
    }

    pub(crate) fn take_call(&self, call: u64) -> Vec<StateLine> {
        let mut inner = self.lock();
        let store = inner.extensions.entry(self.extension.clone()).or_default();
        let Some(pending) = store.calls.remove(&(self.generation, call)) else {
            return Vec::new();
        };
        let mut lines = Vec::with_capacity(pending.len());
        for pending in pending {
            let key = store.keys.entry(pending.line.key.clone()).or_default();
            if key
                .base
                .as_ref()
                .is_none_or(|base| pending.write.landing > base.landing)
            {
                key.base = Some(pending.write);
            }
            lines.push(pending.line);
        }
        lines
    }

    pub(crate) fn discard_call(&self, call: u64) {
        self.lock()
            .extensions
            .entry(self.extension.clone())
            .or_default()
            .calls
            .remove(&(self.generation, call));
    }

    pub(crate) fn set(
        &self,
        scope: Scope,
        key: &str,
        value: Value,
        on_fork: OnFork,
    ) -> Result<(), StateError> {
        let bytes = value.to_string().len();
        let mut inner = self.lock();
        if inner.closed {
            return Err(StateError::Closing);
        }
        if bytes > MAX_BYTES {
            return Err(StateError::TooLarge { bytes });
        }
        self.write(&mut inner, scope, key, Some(value), on_fork);
        Ok(())
    }

    pub(crate) fn unset(&self, scope: Scope, key: &str) -> Result<(), StateError> {
        let mut inner = self.lock();
        if inner.closed {
            return Err(StateError::Closing);
        }
        let Some(previous) = inner
            .extensions
            .get(&self.extension)
            .and_then(|store| store.view(key))
        else {
            return Ok(());
        };
        if previous.value.is_none() {
            return Ok(());
        }
        let rule = previous.rule;
        self.write(&mut inner, scope, key, None, rule);
        Ok(())
    }

    fn write(
        &self,
        inner: &mut Inner,
        scope: Scope,
        key: &str,
        value: Option<Value>,
        rule: OnFork,
    ) {
        let store = inner.extensions.entry(self.extension.clone()).or_default();
        if let Scope::Hook(call) = scope {
            if !store.calls.contains_key(&(self.generation, call)) {
                return;
            }
        }
        inner.landing += 1;
        let landing = inner.landing;
        let event = match &value {
            Some(value) => Event::ExtensionStateSet(ExtensionStateSet {
                extension: self.extension.clone(),
                key: key.into(),
                value: value.clone(),
                on_fork: rule,
            }),
            None => Event::ExtensionStateUnset(ExtensionStateUnset {
                extension: self.extension.clone(),
                key: key.into(),
            }),
        };
        let line = StateLine {
            event,
            extension: self.extension.clone(),
            key: key.into(),
            landing,
            context: None,
        };
        let write = Write {
            value,
            rule,
            landing,
        };
        let entry = store.keys.entry(key.into()).or_default();
        match scope {
            Scope::Other => {
                entry.base = Some(write);
                inner.outbox.push(line);
            }
            Scope::Hook(call) => {
                if let Some(pending) = store.calls.get_mut(&(self.generation, call)) {
                    pending.push(Pending { line, write });
                }
            }
        }
    }
}

pub(crate) struct VmState {
    pub(crate) store: Arc<ExtensionState>,
    pub(crate) scope: Cell<Scope>,
}

impl VmState {
    pub(crate) fn enter(&self, scope: Scope) {
        self.scope.set(scope);
    }
    pub(crate) fn leave(&self) {
        self.enter(Scope::Other);
    }
    pub(crate) fn get(&self, key: &str) -> Option<Value> {
        self.store.get(key)
    }
    pub(crate) fn keys(&self) -> Vec<String> {
        self.store.keys()
    }
    pub(crate) fn set(&self, key: &str, value: Value, on_fork: OnFork) -> Result<(), StateError> {
        self.store.set(self.scope.get(), key, value, on_fork)
    }
    pub(crate) fn unset(&self, key: &str) -> Result<(), StateError> {
        self.store.unset(self.scope.get(), key)
    }
}
