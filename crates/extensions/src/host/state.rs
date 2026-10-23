//! A session's extension-state view and the lines awaiting its log writer.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

use contract::ErrorCode;
use contract::events::{Event, OnFork};
use contract::hook::{StateLine, StateSeed};
use mlua::{Lua, LuaSerdeExt, MultiValue, Value as LuaValue};
use serde_json::Value;

use super::failure::{self, Raise};

pub(crate) const MAX_BYTES: usize = 65_536;

fn invalid(message: impl ToString) -> Raise {
    Raise::Failed(ErrorCode::InvalidArguments, message.to_string())
}

fn key(value: &LuaValue) -> Result<String, Raise> {
    let LuaValue::String(key) = value else {
        return Err(invalid("state: key must be a string"));
    };
    key.to_str().map(|key| key.to_owned()).map_err(invalid)
}

fn failed(error: StateError) -> Raise {
    match error {
        StateError::TooLarge { bytes } => Raise::Failed(
            ErrorCode::StateTooLarge,
            format!("state.set: value is {bytes} JSON bytes; at most {MAX_BYTES} are allowed"),
        ),
        StateError::Closing => {
            Raise::Failed(ErrorCode::Closing, "extension state is closed".into())
        }
    }
}

pub(crate) fn install(lua: &Lua, state: Rc<VmState>, failure: &mlua::Function) -> mlua::Result<()> {
    let table = lua.create_table()?;
    let get = state.clone();
    failure::register(
        lua,
        &table,
        "get",
        failure,
        None,
        move |lua, args: MultiValue| {
            let key = key(args.front().unwrap_or(&LuaValue::Nil))?;
            let value = match get.get(&key) {
                Some(value) => lua.to_value(&value).map_err(invalid)?,
                None => LuaValue::Nil,
            };
            Ok(MultiValue::from_vec(vec![value]))
        },
    )?;
    let set = state.clone();
    failure::register(
        lua,
        &table,
        "set",
        failure,
        None,
        move |_, args: MultiValue| {
            let mut args = args.into_iter();
            let key = key(&args.next().unwrap_or(LuaValue::Nil))?;
            let value = args.next().unwrap_or(LuaValue::Nil);
            if value.is_nil() {
                return Err(invalid(
                    "state.set: value is nil; use state.unset to remove the key",
                ));
            }
            let value = super::to_json(&value).map_err(invalid)?;
            let options = args.next().unwrap_or(LuaValue::Nil);
            let fork = if options.is_nil() {
                LuaValue::Nil
            } else {
                let LuaValue::Table(options) = options else {
                    return Err(invalid("state.set: options must be a table"));
                };
                options.get::<LuaValue>("on_fork").map_err(invalid)?
            };
            let rule = if fork.is_nil() {
                OnFork::AtPoint
            } else {
                let LuaValue::String(rule) = fork else {
                    return Err(invalid(
                        "state.set: on_fork must be at_point, latest or fresh",
                    ));
                };
                match rule.to_str().map_err(invalid)?.as_ref() {
                    "at_point" => OnFork::AtPoint,
                    "latest" => OnFork::Latest,
                    "fresh" => OnFork::Fresh,
                    _ => {
                        return Err(invalid(
                            "state.set: on_fork must be at_point, latest or fresh",
                        ));
                    }
                }
            };
            set.set(&key, value, rule).map_err(failed)?;
            Ok(MultiValue::new())
        },
    )?;
    let unset = state.clone();
    failure::register(
        lua,
        &table,
        "unset",
        failure,
        None,
        move |_, args: MultiValue| {
            let key = key(args.front().unwrap_or(&LuaValue::Nil))?;
            unset.unset(&key).map_err(failed)?;
            Ok(MultiValue::new())
        },
    )?;
    failure::register(
        lua,
        &table,
        "keys",
        failure,
        None,
        move |lua, _: MultiValue| {
            Ok(MultiValue::from_vec(vec![
                lua.to_value(&state.keys()).map_err(invalid)?,
            ]))
        },
    )?;
    lua.globals().set("state", table)
}

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

#[derive(Default)]
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

    // An empty seed builds the same empty registry `Default` does, so the
    // mutant that returns a default one changes no behaviour a test can see.
    #[cfg_attr(false, mutants::skip)]
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
        support::lock(&self.inner)
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
            .entry(line.extension.clone())
            .or_default()
            .keys
            .entry(line.key.clone())
            .or_default();
        if line.landing >= key.last_logged {
            key.last_logged = line.landing;
            events.push(line.into_event());
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
        support::lock(&self.inner)
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
            // Landings are unique, so the newer of the base and this write
            // is the one that landed last.
            let key = store.keys.entry(pending.line.key.clone()).or_default();
            key.base = key
                .base
                .take()
                .into_iter()
                .chain([pending.write])
                .max_by_key(|write| write.landing);
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
        if let Scope::Hook(call) = scope
            && !store.calls.contains_key(&(self.generation, call))
        {
            return;
        }
        inner.landing += 1;
        let landing = inner.landing;
        let line = StateLine {
            extension: self.extension.clone(),
            key: key.into(),
            value: value.clone().map(|value| (value, rule)),
            landing,
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
