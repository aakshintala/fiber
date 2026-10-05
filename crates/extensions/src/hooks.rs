//! A session's extensions and their hooks (`docs/extensions.md`, "Hooks",
//! "When several hooks share a point", "When a hook fails" and "Loading,
//! and cost when nothing is loaded"). Every installed, enabled Lua extension
//! starts at session start, because a hook registers before the session's
//! tool set is fixed ("Registering"). A session with none starts no VM.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use config::Config;
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{LoadedExtension, Notice};
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks};
use serde_json::{Map, Value};

use crate::lua::{DeclaredHooks, HookPhase, LuaExtension};
use crate::{API, Error};

/// The one hook point this session calls.
const AFTER_TOOL: &str = "after_tool";

/// The script a Lua extension's VM runs first.
const ENTRY: &str = "init.lua";

/// The extensions a session loaded, and the hooks they registered, in the
/// order they run.
#[derive(Default)]
pub struct SessionExtensions {
    loaded: Vec<LoadedExtension>,
    notices: Vec<Notice>,
    /// The started Lua extensions.
    lua: Vec<LuaExtension>,
    /// The `after_tool` chain, in run order.
    after_tool: Vec<Entry>,
    /// Whether any hook registered at any point.
    any: bool,
}

/// One hook in a chain.
struct Entry {
    /// Its extension, in [`SessionExtensions::lua`].
    extension: usize,
    /// Its place among its extension's hooks at the point, from 0.
    index: usize,
    blocking: bool,
}

impl SessionExtensions {
    /// Lists what is installed in `home`, starts each enabled Lua extension
    /// on `clock` and reads what its entry script registered, ordering each
    /// point's hooks by phase, `hooks.order`, then name. A failure leaves a
    /// notice and the session goes on: an extension whose entry script
    /// failed is not loaded, and a hook that did not register is not run.
    pub fn load(home: &Path, config: &Config, clock: Arc<dyn Clock>) -> Self {
        let mut session = Self::default();
        let installed = match crate::list(home, clock.as_ref()) {
            Ok(installed) => installed,
            Err(e) => {
                session.notices.push(notice(e.code(), e.to_string(), None));
                return session;
            }
        };
        let mut chain = Vec::new();
        // debt: entry scripts load one after another, each up to its load
        // timeout; start every VM before waiting on any if session start
        // shows the sum (docs/performance.md).
        for item in installed {
            let quoted = format!("extensions.\"{}\"", item.name);
            let enabled = config
                .get(&format!("{quoted}.enabled"), None)
                .and_then(|(value, _)| value.as_bool())
                .unwrap_or(true);
            if !enabled {
                continue;
            }
            let dir = match crate::install::slug(&item.name) {
                Ok(slug) => home.join("extensions").join(slug),
                Err(e) => {
                    session.failed(&item.name, &e);
                    continue;
                }
            };
            let manifest = match config::read_manifest(&dir) {
                Ok(manifest) => manifest,
                Err(e) => {
                    session.failed(&item.name, &Error::Config(e));
                    continue;
                }
            };
            // Written for another API: it does not load (`docs/extensions.md`,
            // "The extension API version").
            if manifest.api != API || manifest.process.is_some() {
                continue;
            }
            if dir.join(ENTRY).is_file() {
                let mut extension = LuaExtension::new(&item.name, &dir, home, Arc::clone(&clock));
                if let Some(cap) = manifest
                    .memory_mib
                    .and_then(|mib| usize::try_from(mib).ok())
                    .and_then(|mib| mib.checked_mul(1 << 20))
                    .and_then(NonZeroUsize::new)
                {
                    extension = extension.with_memory_cap(cap);
                }
                if let Some(ms) = config
                    .get(&format!("{quoted}.hook_timeout_ms"), None)
                    .and_then(|(value, _)| value.as_u64())
                    .filter(|ms| *ms > 0)
                {
                    extension.override_hook_timeout(Duration::from_millis(ms));
                }
                let declared = match extension.hooks() {
                    Ok(declared) => declared,
                    Err(e) => {
                        session.failed(&item.name, &e);
                        continue;
                    }
                };
                session.register(&item.name, declared, &mut chain);
                session.lua.push(extension);
            }
            session.loaded.push(LoadedExtension {
                name: item.name,
                version: item.version,
            });
        }
        let order: Vec<String> = config
            .get(&format!("hooks.order.{AFTER_TOOL}"), None)
            .and_then(|(value, _)| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let rank = |name: &str| order.iter().position(|named| named == name);
        // Within a phase: the extensions `hooks.order` names, in its order,
        // then the rest by name; one extension's hooks in registration order.
        chain.sort_by_key(|(phase, name, entry): &(HookPhase, String, Entry)| {
            (
                *phase,
                rank(name).map_or((1, 0), |at| (0, at)),
                name.clone(),
                entry.index,
            )
        });
        session.after_tool = chain.into_iter().map(|(_, _, entry)| entry).collect();
        session
    }

    /// Every extension the session loaded, by name: `extensions_loaded`
    /// (`docs/events.md`).
    pub fn loaded(&self) -> Vec<LoadedExtension> {
        self.loaded.clone()
    }

    /// What loading raised: an entry script that failed, a hook that did
    /// not register.
    pub fn notices(&self) -> Vec<Notice> {
        self.notices.clone()
    }

    /// Whether any extension registered a hook at any point.
    pub fn has_hooks(&self) -> bool {
        self.any
    }

    /// Records `name`'s registrations: a notice per hook that did not
    /// register, and its `after_tool` hooks onto `chain`.
    fn register(
        &mut self,
        name: &str,
        declared: DeclaredHooks,
        chain: &mut Vec<(HookPhase, String, Entry)>,
    ) {
        for problem in declared.problems {
            self.notices
                .push(notice(ErrorCode::ExtensionFailed, problem, Some(name)));
        }
        self.any |= declared.by_point.values().any(|hooks| !hooks.is_empty());
        let extension = self.lua.len();
        for (index, hook) in declared
            .by_point
            .get(AFTER_TOOL)
            .into_iter()
            .flatten()
            .enumerate()
        {
            chain.push((
                hook.phase,
                name.to_owned(),
                Entry {
                    extension,
                    index,
                    blocking: hook.blocking,
                },
            ));
        }
    }

    /// An extension that failed to load: a notice, and it is not loaded.
    fn failed(&mut self, name: &str, e: &Error) {
        self.notices.push(notice(
            ErrorCode::ExtensionFailed,
            e.to_string(),
            Some(name),
        ));
    }
}

impl Hooks for SessionExtensions {
    fn after_tool(&self, call: &AfterToolCall<'_>) -> AfterToolAnswer {
        let mut content: Option<String> = None;
        let mut details: Option<Value> = None;
        let mut artifact: Option<String> = None;
        let mut changed_by: Vec<String> = Vec::new();
        let mut notices = Vec::new();
        for entry in &self.after_tool {
            let Some(extension) = self.lua.get(entry.extension) else {
                continue;
            };
            let name = extension.name();
            let arg = shown(
                call,
                content.as_deref().unwrap_or(call.content),
                details.as_ref().or(call.details),
            );
            let returned = extension
                .hook(AFTER_TOOL, entry.index, arg)
                .and_then(|value| decode(name, value));
            match returned {
                Ok(change) => {
                    let mut changed = false;
                    if let Some(text) = change.content {
                        content = Some(text);
                        changed = true;
                    }
                    if let Some(value) = change.details {
                        details = Some(value);
                        changed = true;
                    }
                    if let Some(text) = change.artifact {
                        artifact = Some(text);
                        changed = true;
                    }
                    if changed {
                        name_once(&mut changed_by, name);
                    }
                }
                Err(_) if entry.blocking => {
                    name_once(&mut changed_by, name);
                    return AfterToolAnswer {
                        outcome: AfterToolOutcome::Withheld {
                            extension: name.to_owned(),
                        },
                        changed_by,
                        notices,
                    };
                }
                Err(e) => notices.push(notice(
                    ErrorCode::HookFailed,
                    format!("The `{AFTER_TOOL}` hook failed, so its change was dropped: {e}"),
                    Some(name),
                )),
            }
        }
        let outcome = if changed_by.is_empty() {
            AfterToolOutcome::Unchanged
        } else {
            AfterToolOutcome::Changed {
                content,
                details,
                artifact,
            }
        };
        AfterToolAnswer {
            outcome,
            changed_by,
            notices,
        }
    }
}

/// What an `after_tool` hook is given: the call, with `content` and
/// `details` as the hook before it left them.
fn shown(call: &AfterToolCall<'_>, content: &str, details: Option<&Value>) -> Value {
    let mut shown = Map::new();
    shown.insert("tool".into(), Value::String(call.tool.to_owned()));
    shown.insert("arguments".into(), Value::Object(call.arguments.clone()));
    shown.insert(
        "status".into(),
        serde_json::to_value(call.status).unwrap_or(Value::Null),
    );
    shown.insert("content".into(), Value::String(content.to_owned()));
    if let Some(details) = details {
        shown.insert("details".into(), details.clone());
    }
    if let Some(process) = call.process.and_then(|p| serde_json::to_value(p).ok()) {
        shown.insert("process".into(), process);
    }
    Value::Object(shown)
}

/// What an `after_tool` hook returned, each key absent when it left that
/// thing as it was.
#[derive(Default)]
struct Change {
    content: Option<String>,
    details: Option<Value>,
    artifact: Option<String>,
}

/// Reads a hook's return: `nil`, or a table of `content` (a string),
/// `details` (any JSON) and `artifact` (a string), each optional. Anything
/// else is the hook's failure.
fn decode(extension: &str, value: Value) -> Result<Change, Error> {
    let bad = |why: String| Error::BadReturn {
        extension: extension.to_owned(),
        callback: AFTER_TOOL.to_owned(),
        why,
    };
    let map = match value {
        Value::Null => return Ok(Change::default()),
        // An empty Lua table reads as an empty array.
        Value::Array(items) if items.is_empty() => return Ok(Change::default()),
        Value::Object(map) => map,
        other @ (Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_)) => {
            return Err(bad(format!("{}, not a table", kind(&other))));
        }
    };
    let mut change = Change::default();
    for (key, value) in map {
        match (key.as_str(), value) {
            ("content", Value::String(text)) => change.content = Some(text),
            ("artifact", Value::String(text)) => change.artifact = Some(text),
            ("details", value) => change.details = Some(value),
            ("content" | "artifact", other) => {
                return Err(bad(format!("`{key}` as {}, not a string", kind(&other))));
            }
            ("status", _) => {
                return Err(bad(
                    "`status`, which a hook cannot change: whether a call succeeded is the tool's answer"
                        .to_owned(),
                ));
            }
            (_, _) => {
                return Err(bad(format!(
                    "`{key}`, which is not `content`, `details` or `artifact`"
                )));
            }
        }
    }
    Ok(change)
}

/// A JSON value's kind, as an error names it.
fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "a table",
    }
}

/// Adds `name` to `names` unless it is there.
fn name_once(names: &mut Vec<String>, name: &str) {
    if !names.iter().any(|named| named == name) {
        names.push(name.to_owned());
    }
}

fn notice(code: ErrorCode, message: String, extension: Option<&str>) -> Notice {
    Notice {
        code,
        message,
        extension: extension.map(str::to_owned),
    }
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
