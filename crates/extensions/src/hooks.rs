//! A session's extensions and their hooks (`docs/extensions.md`, "Hooks",
//! "When several hooks share a point", "When a hook fails" and "Loading,
//! and cost when nothing is loaded"). Every installed, enabled Lua extension
//! starts at session start, because a hook registers before the session's
//! tool set is fixed ("Registering"). A session with none starts no VM.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use config::Config;
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{LoadedExtension, Notice};
use contract::files::PathLock;
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks};
use contract::inbox::Delivery;
use serde_json::{Map, Value};

use crate::commands::{CommandSource, SessionCommands};
use crate::git::short_name;
use crate::host::Session;
use crate::lua::{DeclaredHooks, HookPhase, LuaExtension};
use crate::{API, Error, LuaProvider};

/// The one hook point this session calls.
const AFTER_TOOL: &str = "after_tool";

/// The script a Lua extension's VM runs first.
const ENTRY: &str = "init.lua";

/// The extensions a session loaded, and the hooks they registered, in the
/// order they run.
#[derive(Default)]
pub struct SessionExtensions {
    loaded: Vec<LoadedExtension>,
    /// Each loaded extension's name and package directory.
    dirs: Vec<(String, PathBuf)>,
    /// Fiber home, anchoring the extensions' data directories.
    home: PathBuf,
    /// Each loaded extension with an `opening`: its name, slug and
    /// manifest opening, in load order.
    openings: Vec<(String, String, config::Opening)>,
    /// Each loaded extension naming a `prompt` file: its name and the
    /// file's text, in load order.
    prompts: Vec<(String, String)>,
    notices: Vec<Notice>,
    /// The started Lua extensions.
    lua: Vec<Arc<LuaExtension>>,
    /// One provider per `fiber.provider` registration, in provider-name
    /// order: the extension's name beside its provider.
    lua_providers: Vec<(String, Arc<LuaProvider>)>,
    /// The `after_tool` chain, in run order.
    after_tool: Vec<Entry>,
    /// Whether any hook registered at any point.
    any: bool,
    /// The session's extension commands, built once from the same final names.
    commands: SessionCommands,
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
    /// `locks` is the session's per-path lock, offered to `host.fs`; every
    /// extension also gets its own clone of `config` and the settings keys
    /// its manifest lists. The repository's ignored settings keys are
    /// collected here, once, into [`notices`](Self::notices).
    pub fn load(
        home: &Path,
        config: &Config,
        clock: Arc<dyn Clock>,
        locks: Arc<dyn PathLock>,
    ) -> Self {
        let mut session = Self {
            home: home.to_path_buf(),
            ..Self::default()
        };
        let listing = match crate::list(home, clock.as_ref()) {
            Ok(listing) => listing,
            Err(e) => {
                session.notices.push(notice(e.code(), e.to_string(), None));
                return session;
            }
        };
        // A damaged directory is skipped, with a notice naming it and
        // the fix, and the rest load.
        for hit in &listing.damaged {
            session.notices.push(notice(
                ErrorCode::ExtensionFailed,
                hit.to_string(),
                Some(short_name(&hit.name)),
            ));
        }
        let installed = listing.installed;
        let mut chain = Vec::new();
        // debt: entry scripts load one after another, each up to its load
        // timeout; start every VM before waiting on any if session start
        // shows the sum (docs/performance.md).
        // First pass: start every VM and collect hooks and commands, so final
        // command names (renames, conflicts, `replaces`) settle globally
        // before any extension is kept or unloaded.
        struct Started {
            name: String,
            version: String,
            slug: String,
            dir: PathBuf,
            manifest: config::Manifest,
            prompt: Option<String>,
            extension: Option<LuaExtension>,
            declared: Option<DeclaredHooks>,
            commands: Vec<(String, String)>,
        }
        let mut started: Vec<Started> = Vec::new();
        for item in installed {
            let quoted = format!("extensions.\"{}\"", item.name);
            let enabled = config
                .get(&format!("{quoted}.enabled"), None)
                .and_then(|(value, _)| value.as_bool())
                .unwrap_or(true);
            if !enabled {
                continue;
            }
            let slug = match crate::install::slug(&item.name) {
                Ok(slug) => slug,
                Err(e) => {
                    session.failed(&item.name, &e);
                    continue;
                }
            };
            let dir = home.join("extensions").join(&slug);
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
            // A named prompt file that is missing or outside the package
            // fails the load, before its VM starts: it registers no hooks
            // and no Lua providers (`docs/system-prompt.md`, "Extension
            // texts").
            let prompt = match manifest.prompt.as_deref() {
                Some(relative) => match config::read_package_text(&dir, relative, "prompt") {
                    Ok(text) => Some(text),
                    Err(e) => {
                        session.failed(&item.name, &Error::Config(e));
                        continue;
                    }
                },
                None => None,
            };
            if dir.join(ENTRY).is_file() {
                let repo: Vec<&str> = manifest.repo_settings.iter().map(String::as_str).collect();
                match config.extension_settings(&item.name, &repo) {
                    Ok((_, mut ignored)) => session.notices.append(&mut ignored),
                    Err(e) => {
                        session.failed(&item.name, &Error::Config(e));
                        continue;
                    }
                }
                let mut extension = LuaExtension::new(&item.name, &dir, home, Arc::clone(&clock))
                    .with_session(Session {
                        config: config.clone(),
                        repo_settings: manifest.repo_settings.clone(),
                        locks: Arc::clone(&locks),
                    });
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
                let commands = match extension.commands() {
                    Ok(commands) => commands,
                    Err(e) => {
                        session.failed(&item.name, &e);
                        continue;
                    }
                };
                started.push(Started {
                    name: item.name,
                    version: item.version,
                    slug,
                    dir,
                    manifest,
                    prompt,
                    extension: Some(extension),
                    declared: Some(declared),
                    commands,
                });
            } else {
                started.push(Started {
                    name: item.name,
                    version: item.version,
                    slug,
                    dir,
                    manifest,
                    prompt,
                    extension: None,
                    declared: None,
                    commands: Vec::new(),
                });
            }
        }
        // Settle final command names across every extension from metadata
        // alone; real VMs attach below.
        struct Meta {
            extension: String,
            replaces: Vec<String>,
            commands: Vec<(String, String)>,
        }
        let metas: Vec<Meta> = started
            .iter()
            .filter(|s| s.extension.is_some())
            .map(|s| Meta {
                extension: s.name.clone(),
                replaces: s.manifest.replaces.clone(),
                commands: s.commands.clone(),
            })
            .collect();
        // Temporary build for notices/unload/conflicts from metadata alone.
        // Real `Arc<LuaExtension>` values are attached after, keyed by name.
        let probe_sources: Vec<CommandSource> = metas
            .iter()
            .map(|m| CommandSource {
                extension: m.extension.clone(),
                replaces: m.replaces.clone(),
                commands: m.commands.clone(),
                lua: Arc::new(LuaExtension::new(
                    &m.extension,
                    home,
                    home,
                    Arc::clone(&clock),
                )),
            })
            .collect();
        let probe = SessionCommands::build(&probe_sources, config);
        let unloaded = probe.unloaded();
        session.notices.extend(probe.notices());
        // Second pass: keep every loaded extension; an undeclared built-in
        // replacement unloads the extension.
        let mut real_sources: Vec<CommandSource> = Vec::new();
        let mut kept: Vec<usize> = Vec::new();
        for (idx, s) in started.iter().enumerate() {
            if unloaded.contains(&s.name) {
                continue;
            }
            kept.push(idx);
        }
        // Attach real VMs for kept Lua extensions, in load order.
        let mut by_name: BTreeMap<String, Arc<LuaExtension>> = BTreeMap::new();
        // Move real extensions out of `started` for kept entries.
        let mut started = started;
        for idx in &kept {
            let Some(s) = started.get_mut(*idx) else {
                continue;
            };
            if let Some(ext) = s.extension.take() {
                by_name.insert(s.name.clone(), Arc::new(ext));
            }
        }
        for idx in &kept {
            let Some(s) = started.get(*idx) else {
                continue;
            };
            if let Some(lua) = by_name.get(&s.name) {
                real_sources.push(CommandSource {
                    extension: s.name.clone(),
                    replaces: s.manifest.replaces.clone(),
                    commands: s.commands.clone(),
                    lua: Arc::clone(lua),
                });
            }
        }
        session.commands = SessionCommands::build(&real_sources, config);
        // The probe's notices were already recorded; the real build agrees on
        // final names, so keep only its admission table. Re-record notices
        // only if they differ (they cannot: same metadata and config).
        for idx in kept {
            let Some(s) = started.get(idx) else {
                continue;
            };
            let Some(declared) = s.declared.clone() else {
                // Data-only extension: no hooks, no commands, no providers.
                if let Some(opening) = s.manifest.opening.clone() {
                    session
                        .openings
                        .push((s.name.clone(), s.slug.clone(), opening));
                }
                if let Some(text) = s.prompt.clone() {
                    session.prompts.push((s.name.clone(), text));
                }
                session.dirs.push((s.name.clone(), s.dir.clone()));
                session.loaded.push(LoadedExtension {
                    name: s.name.clone(),
                    version: s.version.clone(),
                });
                continue;
            };
            let runs_hooks = declared.by_point.values().any(|hooks| !hooks.is_empty());
            let has_commands = !s.commands.is_empty();
            let Some(lua) = by_name.get(&s.name) else {
                continue;
            };
            session.register(&s.name, declared, &mut chain);
            match lua.provider_names() {
                Ok(names) => {
                    for name in names {
                        let provider = LuaProvider::new(Arc::clone(lua), name);
                        session.lua_providers.push((s.name.clone(), provider));
                    }
                }
                Err(e) => session.failed(&s.name, &e),
            }
            // Command-only extensions stay loaded: an extension with a hook
            // or a command is used by the session and stays.
            if runs_hooks || has_commands {
                session.lua.push(Arc::clone(lua));
            }
            if let Some(opening) = s.manifest.opening.clone() {
                session
                    .openings
                    .push((s.name.clone(), s.slug.clone(), opening));
            }
            if let Some(text) = s.prompt.clone() {
                session.prompts.push((s.name.clone(), text));
            }
            session.dirs.push((s.name.clone(), s.dir.clone()));
            session.loaded.push(LoadedExtension {
                name: s.name.clone(),
                version: s.version.clone(),
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
            .lua_providers
            .sort_by(|a, b| a.1.name().cmp(b.1.name()));
        session
    }

    /// One provider per `fiber.provider` registration, in provider-name
    /// order: the extension's name beside its provider.
    pub fn lua_providers(&self) -> &[(String, Arc<LuaProvider>)] {
        &self.lua_providers
    }

    /// Drops every Lua provider except those in `keep`, by provider name.
    /// An extension that registered hooks stays for them; one that only
    /// provided models is then held by nothing, once any refresh thread
    /// holding it ends, and its VM goes with it (`docs/model-routing.md`,
    /// "Model discovery").
    pub fn retain_lua_providers(&mut self, keep: &[&str]) {
        self.lua_providers
            .retain(|(_, provider)| keep.contains(&provider.name()));
    }

    /// Every extension the session loaded, by name: `extensions_loaded`
    /// (`docs/events.md`).
    pub fn loaded(&self) -> Vec<LoadedExtension> {
        self.loaded.clone()
    }

    /// Each loaded extension's name and package directory, as listed:
    /// where its `skills/` and `prompts/` are read from.
    pub fn dirs(&self) -> Vec<(String, PathBuf)> {
        self.dirs.clone()
    }

    /// Each loaded extension naming a `prompt` file: its name and the
    /// file's text, in load order. The loop sorts by name
    /// (`docs/system-prompt.md`, "Extension texts").
    pub fn prompts(&self) -> Vec<(String, String)> {
        self.prompts.clone()
    }

    /// Each loaded extension's section files for the opening message:
    /// the extension's name, its files' absolute paths with the machine
    /// directory's first, then the project's, each in manifest order, and
    /// its byte budget. In extension-name order. An `opening` with no
    /// paths yields no paths; the opening-message build drops a section
    /// with no files. Reads no file (`docs/system-prompt.md`,
    /// "Extension sections").
    pub fn sections(
        &self,
        project: &config::ProjectKey,
    ) -> Vec<(String, Vec<PathBuf>, Option<u64>)> {
        let mut out: Vec<(String, Vec<PathBuf>, Option<u64>)> = self
            .openings
            .iter()
            .map(|(name, slug, opening)| {
                let mut paths = Vec::new();
                for relative in &opening.machine {
                    paths.push(self.home.join("data").join(slug).join(relative));
                }
                for relative in &opening.project {
                    paths.push(
                        self.home
                            .join("projects")
                            .join(project.as_str())
                            .join("data")
                            .join(slug)
                            .join(relative),
                    );
                }
                (name.clone(), paths, opening.budget_bytes)
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// What loading raised: an entry script that failed, a hook that did
    /// not register.
    pub fn notices(&self) -> Vec<Notice> {
        let mut out = self.notices.clone();
        out.extend(self.commands.notices());
        out
    }

    /// The extension entries of the `commands` answer, sorted by name.
    pub fn commands(&self) -> Vec<contract::events::CommandInfo> {
        self.commands.list()
    }

    /// Hands the ephemeral emitter to every extension's `host.status`,
    /// `host.widget` and `host.emit`.
    pub fn emit_to(&self, emit: Arc<dyn contract::emit::Emit>) {
        for extension in &self.lua {
            extension.set_emit(Arc::clone(&emit));
        }
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

    fn deliver_to(&self, inbox: std::sync::mpsc::Sender<Delivery>) {
        // A later `deliver_to` (a resume hands a new sender) replaces the
        // sender; each extension flushes what ended before the first one.
        for extension in &self.lua {
            extension.deliver_to(inbox.clone());
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

impl contract::extension::ExtensionDoor for SessionExtensions {
    fn command(
        &self,
        name: &str,
        text: &str,
    ) -> Result<Box<dyn FnOnce() + Send>, contract::inbox::Rejection> {
        let admitted = self.commands.admit_with(name, text)?;
        let queued = std::sync::Arc::clone(admitted.queued());
        let lua = std::sync::Arc::clone(admitted.lua());
        let extension = admitted.extension().to_owned();
        let command = name.to_owned();
        // One waiter thread per admitted command waits for the call's end;
        // an `Err` gives a `notice` naming the extension through the
        // extension's late-bound emitter.
        std::thread::Builder::new()
            .name(format!("command {name}"))
            .spawn(move || {
                if let Err(e) = queued.wait() {
                    lua.emit(contract::events::Event::Notice(notice(
                        ErrorCode::ExtensionFailed,
                        format!("Command `{command}` failed: {e}"),
                        Some(&extension),
                    )));
                }
            })
            .map_err(|e| contract::inbox::Rejection {
                code: ErrorCode::IoFailed,
                message: format!("cannot start a thread: {e}"),
            })?;
        let release_queued = std::sync::Arc::clone(admitted.queued());
        Ok(Box::new(move || release_queued.release()))
    }

    fn seal(&self) {
        for extension in &self.lua {
            extension.seal();
        }
    }

    fn reply(
        &self,
        reply: contract::commands::Reply,
        ack: contract::inbox::Ack,
    ) -> Option<(contract::commands::Reply, contract::inbox::Ack)> {
        // Part 4 owns `host.ask` replies; hand every reply back for the loop.
        Some((reply, ack))
    }
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
