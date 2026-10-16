//! A session's extension-state view and the lines awaiting its log writer.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use contract::events::{Event, ExtensionStateSet, ExtensionStateUnset, OnFork};
use contract::hook::{StateLine, StateSeed};
use serde_json::Value;

pub(crate) const MAX_BYTES: usize = 65_536;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Scope {
    Other,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StateError {
    TooLarge { bytes: usize },
}

#[derive(Default)]
struct Key {
    value: Option<Value>,
    last_logged: u64,
}

#[derive(Default)]
struct Inner {
    extensions: BTreeMap<String, BTreeMap<String, Key>>,
    landing: u64,
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
                    .map(|(key, (value, _rule))| {
                        (
                            key,
                            Key {
                                value: Some(value),
                                last_logged: 0,
                            },
                        )
                    })
                    .collect();
                (extension, keys)
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
        self.lock().extensions.entry(extension.into()).or_default();
        Arc::new(ExtensionState {
            inner: self.inner.clone(),
            extension: extension.into(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn take_outbox(&self) -> Vec<Event> {
        let mut inner = self.lock();
        let lines = std::mem::take(&mut inner.outbox);
        admit(&mut inner, lines)
    }

    pub(crate) fn admit(&self, lines: Vec<StateLine>) -> Vec<Event> {
        admit(&mut self.lock(), lines)
    }
}

fn admit(inner: &mut Inner, lines: Vec<StateLine>) -> Vec<Event> {
    let mut events = Vec::new();
    for line in lines {
        let key = inner
            .extensions
            .entry(line.extension)
            .or_default()
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
}

impl Default for ExtensionState {
    fn default() -> Self {
        Self {
            inner: StateRegistry::standalone().inner.clone(),
            extension: String::new(),
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
            .get(key)?
            .value
            .clone()
    }

    pub(crate) fn keys(&self) -> Vec<String> {
        self.lock()
            .extensions
            .get(&self.extension)
            .into_iter()
            .flat_map(|keys| {
                keys.iter()
                    .filter_map(|(key, entry)| entry.value.as_ref().map(|_| key.clone()))
            })
            .collect()
    }

    pub(crate) fn set(
        &self,
        _scope: Scope,
        key: &str,
        value: Value,
        on_fork: OnFork,
    ) -> Result<(), StateError> {
        let bytes = value.to_string().len();
        let mut inner = self.lock();
        if bytes > MAX_BYTES {
            return Err(StateError::TooLarge { bytes });
        }
        inner.landing += 1;
        let landing = inner.landing;
        inner
            .extensions
            .entry(self.extension.clone())
            .or_default()
            .entry(key.into())
            .or_default()
            .value = Some(value.clone());
        inner.outbox.push(StateLine {
            event: Event::ExtensionStateSet(ExtensionStateSet {
                extension: self.extension.clone(),
                key: key.into(),
                value,
                on_fork,
            }),
            extension: self.extension.clone(),
            key: key.into(),
            landing,
            context: None,
        });
        Ok(())
    }

    pub(crate) fn unset(&self, _scope: Scope, key: &str) -> Result<(), StateError> {
        let mut inner = self.lock();
        let entry = inner
            .extensions
            .entry(self.extension.clone())
            .or_default()
            .entry(key.into())
            .or_default();
        if entry.value.take().is_none() {
            return Ok(());
        }
        inner.landing += 1;
        let landing = inner.landing;
        inner.outbox.push(StateLine {
            event: Event::ExtensionStateUnset(ExtensionStateUnset {
                extension: self.extension.clone(),
                key: key.into(),
            }),
            extension: self.extension.clone(),
            key: key.into(),
            landing,
            context: None,
        });
        Ok(())
    }
}
