//! A session's extensions and their hooks (`docs/extensions.md`, "Hooks",
//! "When several hooks share a point", "When a hook fails" and "Loading,
//! and cost when nothing is loaded"). Enabled extensions with `init.lua`
//! start before the session's tool set is fixed ("Registering"). Provider
//! files wait for first use and share their extension's VM.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use config::Config;
use contract::ErrorCode;
use contract::Seq;
use contract::clock::Clock;
use contract::diag::{DebugLog, HookChain, Op, Timing};
use contract::events::{Class, Event, LoadedExtension, Notice};
use contract::files::PathLock;
use contract::hook::{
    AfterToolAnswer, AfterToolCall, AfterToolOutcome, BeforeHandoff, BeforeHandoffAnswer,
    BeforeMessage, BeforeMessageAnswer, BeforeModelCall, BeforeToolAnswer, BeforeToolCall,
    CheckAnswer, ContextAnswer, Hooks, JobDelivery, SessionStart, StateLine, StateSeed, TurnEnd,
    TurnStart,
};
use contract::inbox::Delivery;
use serde_json::{Map, Value};

use contract::search::SearchBackend;
use contract::tool::Tool;

use crate::commands::{CommandSource, Lazy, SessionCommands};
use crate::lua::{DeclaredHooks, LuaExtension};
use crate::{API, Error, LuaProvider, LuaTool};

mod before;
mod chain;
mod door;
mod handoff;
mod lifecycle;
mod settle;
mod start;
pub use start::dep_dirs;
pub(crate) use start::is_repository_copy;
#[cfg(test)]
mod state_tests;

use settle::Meta;

/// The one hook point this session calls.
const AFTER_TOOL: &str = "after_tool";

/// The script a Lua extension's VM runs first.
const ENTRY: &str = "init.lua";

/// The extensions a session loaded, and the hooks they registered, in the
/// order they run.
#[derive(Default)]
pub struct SessionExtensions {
    state: Arc<crate::host::state::StateRegistry>,
    change_state: Mutex<Vec<StateLine>>,
    loaded: Vec<LoadedExtension>,
    /// Each loaded extension's name and package directory.
    dirs: Vec<(String, PathBuf)>,
    /// Fiber home, anchoring the extensions' data directories.
    home: PathBuf,
    /// Host-call replies supplied only by the extension case runner
    /// (`docs/testing.md`, "Testing an extension").
    host_script: Option<Arc<crate::host::script::HostScript>>,
    /// Each loaded extension with an `opening`: its name, slug and
    /// manifest opening, in load order.
    openings: Vec<(String, String, config::Opening)>,
    /// Each loaded extension naming a `prompt` file: its name and the
    /// file's text, in load order.
    prompts: Vec<(String, String)>,
    notices: Vec<Notice>,
    /// The holds that keep the repository copies this session runs for its
    /// whole life: a reload takes the next set's holds before the old set
    /// drops its own.
    holds: Vec<crate::CopyHold>,
    /// The approved `required` repository extensions the set approved, by
    /// name: what `missing_required` reports when they did not load.
    required: Vec<String>,
    /// The started Lua extensions.
    lua: Vec<Arc<LuaExtension>>,
    /// Every watcher the loaded extensions registered, in extension-name
    /// order then registration order: what [`SessionExtensions::deliver`]
    /// fans an appended event out to.
    watchers: Vec<WatcherRegistration>,
    /// The last offered `seq` per watcher, keyed by extension name and
    /// watcher index: unset until its first offer, when it bases on that
    /// offer. A watcher registered mid-session never catches up from the
    /// log: events before it existed are not in its stream.
    cursors: Mutex<BTreeMap<(String, usize), u64>>,
    /// The log end when the subscription was recorded: what an unset
    /// cursor defaults to at the drain.
    watermark: Mutex<Option<u64>>,
    /// One provider per `fiber.provider` registration, in provider-name
    /// order: the extension's name beside its provider.
    lua_providers: Vec<(String, Arc<LuaProvider>)>,
    provider_vms: Mutex<BTreeMap<String, Weak<LuaExtension>>>,
    browser: Arc<crate::attend::SessionBrowser>,
    emitter: Mutex<Option<Arc<dyn contract::emit::Emit>>>,
    /// The session inbox sender the last `deliver_to` received, kept for
    /// provider VMs started after it: a later sender replaces it, as on
    /// every live VM.
    inbox: Mutex<Option<std::sync::mpsc::Sender<Delivery>>>,
    #[cfg(test)]
    late_pause: Mutex<Option<LatePause>>,
    /// The names of extensions refused for an undeclared Lua registration,
    /// in load order. A data refusal is not listed: `Providers::load`
    /// never admitted that data.
    rejected: Arc<Mutex<Vec<String>>>,
    /// The `before_tool` chain, in run order.
    before_tool: Vec<chain::Entry>,
    /// The `before_message` chain, in run order.
    before_message: Vec<chain::Entry>,
    /// The `after_tool` chain, in run order.
    after_tool: Vec<chain::Entry>,
    /// The `session_start`, `turn_start`, `before_model_call` and `turn_end`
    /// chains, in run order.
    lifecycle: chain::Lifecycle,
    /// Starts one timer around each non-tool hook dispatch, when
    /// installed. Tool hooks have no timer: `tool.hooks` covers them.
    timing: Option<Timing>,
    /// The session's diagnostic sink, installed on every live VM and
    /// kept for late provider VMs.
    diagnostics: Option<Arc<dyn DebugLog>>,
    /// Whether any hook registered at any point.
    any: bool,
    /// The session's extension commands, built once from the same final names.
    commands: SessionCommands,
    /// The tools the session declares, each beside its extension's name.
    tools: Vec<(String, Arc<dyn Tool>)>,
    /// The search backend Fiber's own `web_search` runs, chosen at load
    /// from what is installed and `web_search.backend`.
    search_backend: Option<Arc<dyn SearchBackend>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LatePoint {
    Stored,
    Cleared,
    Read,
}
#[cfg(test)]
type LatePause = Arc<dyn Fn(LatePoint) + Send + Sync>;

struct Started {
    name: String,
    version: String,
    slug: String,
    dir: PathBuf,
    manifest: config::Manifest,
    prompt: Option<String>,
    extension: Option<Arc<LuaExtension>>,
    declared: Option<DeclaredHooks>,
    commands: Vec<(String, String)>,
    tools: Vec<LuaTool>,
    backends: Vec<String>,
    providers: Vec<String>,
    /// For an extension whose manifest has a `commands` key: the start its
    /// first command run owes.
    lazy: Option<Arc<Lazy>>,
}

/// One session-end watcher failure: what the drain collects for `run_turn`
/// to print under `fiber_exited`. A notice cannot carry it: the log is
/// already closed to new lines.
#[derive(Debug)]
pub struct WatcherError {
    /// The extension that owns the watcher.
    pub extension: String,
    /// The watcher that failed, as `watcher:{index}`.
    pub callback: String,
    /// The failure's message.
    pub message: String,
}

/// The watcher's one argument: its `kind` beside its `payload`, serialized
/// once per event (`run` receives `{"kind": ..., "payload": ...}`).
fn watcher_arg(event: &Event) -> Value {
    serde_json::json!({
        "kind": event.kind(),
        "payload": Value::Object(event.payload().unwrap_or_default()),
    })
}

/// One watcher `fiber.watch` registered: its extension's name and handle,
/// its place in registration order, and the event kinds it wants.
struct WatcherRegistration {
    name: String,
    extension: Arc<LuaExtension>,
    index: usize,
    kinds: Vec<String>,
}

impl SessionExtensions {
    /// Lists what is installed in `home`, starts each enabled Lua extension
    /// on `clock` and reads what its entry script registered, ordering each
    /// point's hooks by phase, `hooks.order`, then name. A failure leaves a
    /// notice and the session goes on: an extension whose entry script
    /// failed is not loaded, and a hook that did not register is not run.
    /// `locks` is the session's per-path lock, offered to `host.fs`; every
    /// extension also gets its own clone of `config` and the settings keys
    /// its manifest lists. `host` is available only to the case runner
    /// (`docs/testing.md`, "Testing an extension"). The repository's ignored
    /// settings keys are collected here, once, into [`notices`](Self::notices).
    pub fn load(
        home: &Path,
        config: &Config,
        clock: Arc<dyn Clock>,
        locks: Arc<dyn PathLock>,
        host: Option<Arc<crate::host::script::HostScript>>,
    ) -> Self {
        Self::load_with_state(home, config, clock, locks, host, StateSeed::new())
    }

    /// Loads extensions with their folded state installed before any Lua runs.
    pub fn load_with_state(
        home: &Path,
        config: &Config,
        clock: Arc<dyn Clock>,
        locks: Arc<dyn PathLock>,
        host: Option<Arc<crate::host::script::HostScript>>,
        seed: StateSeed,
    ) -> Self {
        Self::load_registry(
            home,
            config,
            clock,
            locks,
            host,
            crate::host::state::StateRegistry::new(seed),
            crate::RepositorySet::default(),
            BTreeSet::new(),
        )
    }

    /// Loads extensions with the repository set the session approved, in
    /// place of the personal installs of the same names. The set's holds
    /// move into the session, so its copies live for the session's life.
    pub fn load_repository(
        home: &Path,
        config: &Config,
        clock: Arc<dyn Clock>,
        locks: Arc<dyn PathLock>,
        host: Option<Arc<crate::host::script::HostScript>>,
        seed: StateSeed,
        repository: crate::RepositorySet,
    ) -> Self {
        Self::load_registry(
            home,
            config,
            clock,
            locks,
            host,
            crate::host::state::StateRegistry::new(seed),
            repository,
            BTreeSet::new(),
        )
    }

    /// Reloads extensions over the session's live state without closing it.
    pub fn reload(
        &self,
        home: &Path,
        config: &Config,
        clock: Arc<dyn Clock>,
        locks: Arc<dyn PathLock>,
    ) -> Self {
        Self::load_registry(
            home,
            config,
            clock,
            locks,
            self.host_script.clone(),
            self.state.clone(),
            crate::RepositorySet::default(),
            self.started_lazy(),
        )
    }

    /// Reloads extensions with the repository set over the session's live
    /// state without closing it. The set's holds move into the reloaded
    /// session, so its copies live until the next reload replaces them.
    pub fn reload_repository(
        &self,
        home: &Path,
        config: &Config,
        clock: Arc<dyn Clock>,
        locks: Arc<dyn PathLock>,
        repository: crate::RepositorySet,
    ) -> Self {
        Self::load_registry(
            home,
            config,
            clock,
            locks,
            self.host_script.clone(),
            self.state.clone(),
            repository,
            self.started_lazy(),
        )
    }

    /// The `required` repository extensions the set approved that are not
    /// loaded, by name.
    pub fn missing_required(&self) -> Vec<String> {
        self.required
            .iter()
            .filter(|name| !self.loaded.iter().any(|loaded| &loaded.name == *name))
            .cloned()
            .collect()
    }

    /// The lazy extensions whose VM started: a reload starts these again and
    /// leaves every other lazy extension unstarted.
    fn started_lazy(&self) -> BTreeSet<String> {
        self.commands
            .lazies()
            .iter()
            .filter(|lazy| lazy.started())
            .map(|lazy| lazy.extension().to_owned())
            .collect()
    }

    /// `restart` names the lazy extensions to start before the session is
    /// returned.
    #[allow(
        clippy::too_many_arguments,
        reason = "the loader receives the session's existing services and shared state"
    )]
    fn load_registry(
        home: &Path,
        config: &Config,
        clock: Arc<dyn Clock>,
        locks: Arc<dyn PathLock>,
        host: Option<Arc<crate::host::script::HostScript>>,
        state: Arc<crate::host::state::StateRegistry>,
        repository: crate::RepositorySet,
        restart: BTreeSet<String>,
    ) -> Self {
        let mut session = Self {
            state,
            home: home.to_path_buf(),
            host_script: host.clone(),
            ..Self::default()
        };
        let entries = match crate::installed::locked_entries(home, clock.as_ref()) {
            Ok(entries) => entries,
            Err(e) => {
                session.notices.push(notice(e.code(), e.to_string(), None));
                return session;
            }
        };
        // A damaged directory is skipped: `Providers::load` already
        // reported it, so no notice here.
        let found = entries.found;
        // An approved repository copy loads in place of the personal
        // install of the same name: the merged list carries both in slug
        // order, with one notice per shadowed install.
        let (merged, mut candidate_notices): (Vec<crate::repository::Candidate>, Vec<Notice>) =
            crate::repository::candidates(found, &repository, &|name: &str| {
                crate::is_enabled(config, name)
            });
        session.notices.extend(repository.notices);
        session.notices.append(&mut candidate_notices);
        session.holds = repository.holds;
        session.required = repository.required;
        let mut chain = chain::Builder::default();
        // debt: entry scripts load one after another, each up to its load
        // timeout; start every VM before waiting on any if session start
        // shows the sum (docs/performance.md).
        // First pass: start every VM and collect what it registered, so
        // names settle across every extension before any is kept or
        // unloaded.
        let mut started: Vec<Started> = Vec::new();
        for candidate in merged {
            // The first-party `hooks` extension loads only when the
            // configuration declares a `hooks` key (`docs/extensions.md`,
            // "Hooks declared in configuration"): without one there is
            // nothing for it to run, so it is skipped silently. Unreadable
            // settings skip the load and fail closed at the gate.
            if candidate.name == config::full_name("hooks") {
                match config
                    .extensions()
                    .get(&candidate.name, &["hooks"], "hooks")
                {
                    Ok(None) => continue,
                    Err(error) => {
                        session.failed(&candidate.name, &Error::Config(error));
                        continue;
                    }
                    Ok(Some(_)) => {}
                }
            }
            let slug = match crate::install::slug(&candidate.name) {
                Ok(slug) => slug,
                Err(e) => {
                    session.failed(&candidate.name, &e);
                    continue;
                }
            };
            // The locked read already checked the record and read the
            // manifest, so the directory and manifest are reused. A
            // repository entry's directory is its pinned copy, never the
            // repository's files.
            let name = candidate.name;
            let version = candidate.version;
            let dir = candidate.dir;
            let manifest = candidate.manifest;
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
                        session.failed(&name, &Error::Config(e));
                        continue;
                    }
                },
                None => None,
            };
            let mut entry = Started {
                name,
                version,
                slug,
                dir,
                manifest,
                prompt,
                extension: None,
                declared: None,
                commands: Vec::new(),
                tools: Vec::new(),
                backends: Vec::new(),
                providers: Vec::new(),
                lazy: None,
            };
            // A data provider the manifest does not declare under
            // `providers` keeps the extension out, before its VM starts.
            // `Providers::load` already reported it, so no notice here.
            match config::provider_files(&entry.dir) {
                Ok(files) => {
                    let stems: Vec<String> = files
                        .iter()
                        .filter_map(|file| file.file_stem())
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .collect();
                    if crate::providers::first_undeclared(
                        &entry.manifest.providers,
                        stems.iter().map(String::as_str),
                    )
                    .is_some()
                    {
                        continue;
                    }
                }
                Err(e) => {
                    session.failed(&entry.name, &Error::Config(e));
                    continue;
                }
            }
            entry.providers = match crate::providers::provider_scripts(&entry.dir) {
                Ok(names) => names,
                Err(e) => {
                    session.failed(&entry.name, &e);
                    continue;
                }
            };
            if let Some(unclaimed) = crate::providers::first_undeclared(
                &entry.manifest.providers,
                entry.providers.iter().map(String::as_str),
            ) {
                session.failed(
                    &entry.name,
                    &Error::UndeclaredProvider {
                        extension: entry.name.clone(),
                        provider: unclaimed.to_owned(),
                    },
                );
                session.reject(&entry.name);
                continue;
            }
            // A `commands` key, `{}` included, makes the extension lazy: its
            // VM is built but not started, and its commands come from the
            // manifest.
            if entry.manifest.commands.is_some() || entry.dir.join(ENTRY).is_file() {
                let extension = match start::start_vm(
                    &entry.name,
                    &entry.dir,
                    home,
                    config,
                    &entry.manifest,
                    Arc::clone(&clock),
                    Arc::clone(&locks),
                    Arc::clone(&session.state),
                ) {
                    Ok((extension, mut ignored)) => {
                        session.notices.append(&mut ignored);
                        let extension = extension.with_browser(session.browser.clone());
                        Arc::new(match &host {
                            Some(script) => extension.with_host_script(Arc::clone(script)),
                            None => extension,
                        })
                    }
                    Err(e) => {
                        session.failed(&entry.name, &e);
                        continue;
                    }
                };
                if let Some(declared) = &entry.manifest.commands {
                    entry.commands = declared
                        .iter()
                        .map(|(name, spec)| (name.clone(), spec.description.clone()))
                        .collect();
                    entry.lazy = Some(Lazy::new(
                        entry.name.clone(),
                        Arc::clone(&extension),
                        declared
                            .iter()
                            .map(|(name, spec)| (name.clone(), spec.argument_hint.clone()))
                            .collect(),
                        Arc::clone(&session.rejected),
                    ));
                    entry.extension = Some(extension);
                    started.push(entry);
                    continue;
                }
                let registered = extension.hooks().and_then(|declared| {
                    Ok((
                        declared,
                        extension.commands()?,
                        extension.tools()?,
                        extension.search_backends()?,
                    ))
                });
                match registered {
                    Ok((declared, commands, tools, backends)) => {
                        entry.declared = Some(declared);
                        entry.commands = commands;
                        entry.tools = keep_listed(config, &entry.name, tools);
                        entry.backends = backends;
                    }
                    Err(e) => {
                        session.failed(&entry.name, &e);
                        if extension.misplaced_provider() {
                            session.reject(&entry.name);
                        }
                        continue;
                    }
                }
                entry.extension = Some(extension);
            }
            started.push(entry);
        }
        let metas: Vec<Meta> = started
            .iter()
            .filter(|s| s.extension.is_some())
            .map(|s| Meta {
                extension: s.name.clone(),
                replaces: s.manifest.replaces.clone(),
                commands: s.commands.clone(),
                tools: s.tools.iter().map(|tool| tool.definition().name).collect(),
            })
            .collect();
        let settled = settle::settle(&metas, config, home, &clock);
        session.notices.extend(settled.notices);
        started.retain(|s| !settled.unloaded.contains(&s.name));
        // The real build settles the same final names from the same
        // metadata, so it keeps only its admission table: its notices are
        // already recorded above, and publishing them again would show
        // every rename and conflict notice twice.
        let real_sources: Vec<CommandSource> = started
            .iter()
            .filter_map(|s| {
                s.extension.as_ref().map(|lua| CommandSource {
                    extension: s.name.clone(),
                    replaces: s.manifest.replaces.clone(),
                    commands: s.commands.clone(),
                    lua: Arc::clone(lua),
                })
            })
            .collect();
        session.commands = SessionCommands::build(&real_sources, config);
        session
            .commands
            .attach_lazy(started.iter().filter_map(|s| s.lazy.clone()).collect());
        for lazy in session.commands.lazies() {
            if restart.contains(lazy.extension())
                && let Err(rejection) = lazy.start()
            {
                session.notices.push(notice(
                    rejection.code,
                    rejection.message,
                    Some(lazy.extension()),
                ));
            }
        }
        let mut backend_choices: Vec<(String, String, Arc<LuaExtension>)> = Vec::new();
        for s in &started {
            if let Some(lua) = &s.extension {
                for backend in &s.backends {
                    backend_choices.push((s.name.clone(), backend.clone(), Arc::clone(lua)));
                }
            }
        }
        let setting = config
            .get("web_search.backend", None)
            .and_then(|(value, _)| value.as_str().map(str::to_owned));
        let (search_backend, mut search_notices) =
            crate::search::choose(backend_choices, setting.as_deref());
        session.notices.append(&mut search_notices);
        session.search_backend = search_backend;
        for s in started {
            if let Err(e) = start::provider_handles(
                &mut session,
                &s,
                config,
                Arc::clone(&clock),
                Arc::clone(&locks),
            ) {
                session.failed(&s.name, &e);
                continue;
            }
            // A lazy extension's VM stays unstarted but joins the session's
            // walks, which only set what `host.drive`, `host.model` and
            // `host.delegate` later read.
            if let (Some(lua), Some(_)) = (&s.extension, &s.lazy) {
                session.lua.push(Arc::clone(lua));
            }
            if let (Some(lua), Some(declared)) = (&s.extension, s.declared) {
                let runs_hooks = declared.by_point.values().any(|hooks| !hooks.is_empty());
                session.register(&s.name, declared, &mut chain);
                let mut has_tools = false;
                for tool in s.tools {
                    if settled
                        .tools
                        .contains(&(s.name.clone(), tool.definition().name))
                    {
                        has_tools = true;
                        session.tools.push((s.name.clone(), Arc::new(tool)));
                    }
                }
                // An extension with a hook, a command, a tool or a search
                // backend is used by the session and stays.
                if runs_hooks || !s.commands.is_empty() || has_tools || !s.backends.is_empty() {
                    session.lua.push(Arc::clone(lua));
                }
            }
            // An extension with only watchers is used by the session too:
            // its watchers join the fan-out, and its VM stays for them.
            // Reading them here boots nothing new: `hooks`, `commands`,
            // `tools` and `search_backends` above already waited on the
            // same registration.
            if let Some(lua) = &s.extension
                && s.lazy.is_none()
                && let Ok(declared) = lua.watchers()
            {
                // A watch refusal surfaces as the registration notice,
                // as hooks do (Ruling 6): even when no watcher survived,
                // the problems still name the extension.
                for problem in &declared.problems {
                    session.notices.push(notice(
                        ErrorCode::ExtensionFailed,
                        problem.clone(),
                        Some(&s.name),
                    ));
                }
                if !declared.watchers.is_empty() {
                    for (index, watcher) in declared.watchers.iter().enumerate() {
                        session.watchers.push(WatcherRegistration {
                            name: s.name.clone(),
                            extension: Arc::clone(lua),
                            index,
                            kinds: watcher.kinds.clone(),
                        });
                    }
                    if !session.lua.iter().any(|held| Arc::ptr_eq(held, lua)) {
                        session.lua.push(Arc::clone(lua));
                    }
                }
            }
            if let Some(opening) = s.manifest.opening {
                session.openings.push((s.name.clone(), s.slug, opening));
            }
            if let Some(text) = s.prompt {
                session.prompts.push((s.name.clone(), text));
            }
            session.dirs.push((s.name.clone(), s.dir));
            session.loaded.push(LoadedExtension {
                name: s.name,
                version: s.version,
                replaces: s.manifest.replaces,
            });
        }
        chain.fill(config, &mut session);
        session
            .lua_providers
            .sort_by(|a, b| a.1.name().cmp(b.1.name()));
        // The fan-out order: extension-name order, watcher registration
        // order within each extension.
        session
            .watchers
            .sort_by(|a, b| (&a.name, a.index).cmp(&(&b.name, b.index)));
        // Declared hooks with no extension to run them are not silently
        // dropped: all-`non-blocking` leaves one notice naming the
        // extension (`MissingBlocking` fails the start at the gate). A
        // settings read that fails leaves no notice here: the gate fails
        // the start with the read's error instead.
        if let Ok(HooksState::MissingNotice { notice }) =
            hooks_state(config, &session.loaded_names())
        {
            session.notices.push(notice);
        }
        session
    }

    /// One provider per `fiber.provider` registration, in provider-name
    /// order: the extension's name beside its provider.
    pub fn lua_providers(&self) -> &[(String, Arc<LuaProvider>)] {
        &self.lua_providers
    }

    /// The names of extensions refused for an undeclared Lua registration,
    /// in load order: what `Providers::add_session` drops the data of.
    pub fn rejected(&self) -> Vec<String> {
        self.rejected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn reject(&self, name: &str) {
        self.rejected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(name.to_owned());
    }

    /// Installs the session's diagnostics after `load`, before any
    /// provider work: `timing` starts one guard around each non-tool
    /// hook dispatch, and `debug` observes eligible provider HTTP on
    /// every live VM, hook and provider-only alike. Attachment runs no
    /// provider callback, and a later installation never redirects a VM
    /// that already has a sink: the first one wins.
    pub fn with_diagnostics(mut self, timing: Timing, debug: Arc<dyn DebugLog>) -> Self {
        self.timing = Some(timing);
        self.diagnostics = Some(Arc::clone(&debug));
        let mut vms: Vec<Arc<LuaExtension>> = self.lua.iter().map(Arc::clone).collect();
        // Collected under the mutex and released before attaching: no
        // setter runs under it.
        let provider_vms: Vec<Weak<LuaExtension>> = self
            .provider_vms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for vm in vms.drain(..) {
            vm.set_diagnostics(Arc::clone(&debug));
        }
        for weak in provider_vms {
            if let Some(vm) = weak.upgrade() {
                vm.set_diagnostics(Arc::clone(&debug));
            }
        }
        self
    }

    /// The installed diagnostic sink, kept for late provider VMs after
    /// `retain_lua_providers` clears the handles.
    pub(crate) fn diagnostics(&self) -> Option<Arc<dyn DebugLog>> {
        self.diagnostics.clone()
    }

    /// Every live VM handle diagnostics attached to: the hook VMs and
    /// every live weak provider VM. Collect under the weak-map lock, then
    /// release it before attaching a sink or sealing a VM.
    pub(crate) fn attached_vms(&self) -> Vec<Arc<LuaExtension>> {
        let mut vms: Vec<Arc<LuaExtension>> = self.lua.iter().map(Arc::clone).collect();
        let provider_vms: Vec<Weak<LuaExtension>> = self
            .provider_vms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for weak in provider_vms {
            if let Some(vm) = weak.upgrade()
                && !vms.iter().any(|held| Arc::ptr_eq(held, &vm))
            {
                vms.push(vm);
            }
        }
        vms
    }

    /// Starts the chain's timer, when installed: one owned guard around
    /// the whole public dispatch, dropped however it returns. No tool
    /// hook takes one: `tool.hooks` covers those chains.
    fn timed(&self, chain: HookChain) -> Option<Box<dyn Send>> {
        self.timing
            .as_ref()
            .and_then(|timing| timing(Op::Hook(chain), None, None))
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

    /// The loaded extensions' names, in load order: what the removed-`hooks`
    /// gate reads (`docs/extensions.md`, "Hooks declared in configuration").
    pub fn loaded_names(&self) -> Vec<String> {
        self.loaded
            .iter()
            .map(|loaded| loaded.name.clone())
            .collect()
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
                    paths.push(config::extension_data_dir(&self.home, slug).join(relative));
                }
                for relative in &opening.project {
                    paths.push(config::project_data_dir(&self.home, project, slug).join(relative));
                }
                (name.clone(), paths, opening.budget_bytes)
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// What loading raised: an entry script that failed, a hook, command or
    /// tool that did not register, the tool and command `replaces` checks,
    /// command renames and conflicts, tool clashes, and the search-backend
    /// choice (`web_search_unavailable` or a duplicated backend name). Each
    /// is recorded once, when names settle at load.
    pub fn notices(&self) -> Vec<Notice> {
        self.notices.clone()
    }

    /// The extension entries of the `commands` answer, sorted by name.
    pub fn commands(&self) -> Vec<contract::events::CommandInfo> {
        self.commands.list()
    }

    /// The tools the session declares, each beside its extension's name:
    /// extensions in load order, each one's tools by name. A tool named
    /// like another extension's, or like a built-in its manifest does not
    /// list in `replaces`, is not here.
    pub fn tools(&self) -> Vec<(String, Arc<dyn Tool>)> {
        self.tools.clone()
    }

    /// The search backend Fiber's own `web_search` runs, chosen at load.
    /// The same `Arc` for the whole session.
    pub fn search_backend(&self) -> Option<Arc<dyn SearchBackend>> {
        self.search_backend.clone()
    }

    /// Offers a delegate end to the extension that binds its job: the
    /// finish is held until the completion pushes the one `run` item. A
    /// finish no loaded extension binds is not held, so model-owned
    /// delegates pass through; a completion no extension binds pushes
    /// nothing, so released or removed ids never run. Runs on whatever
    /// thread appended the line, before the watcher fan-out below.
    fn observe_delegate(&self, event: &Event) {
        if let Event::DelegateFinished(finished) = event {
            for extension in &self.lua {
                if extension.hub().hold_delegate_finish(finished) {
                    break;
                }
            }
        } else if let Event::JobCompleted(completed) = event {
            for extension in &self.lua {
                if extension
                    .hub()
                    .push_delegate_run(extension.name(), completed)
                {
                    break;
                }
            }
        }
    }

    /// `Log::append` offer fans out here. An ephemeral event skips a
    /// watcher with a delivery queued or running — a watcher that falls
    /// behind loses ephemeral events — while a durable one always queues,
    /// never losing one. On `Some` seq the cursor of every watcher offered
    /// advances to it; an ephemeral offer carries none. No-op when the
    /// session holds no watchers.
    pub fn deliver(&self, event: &Event, seq: Option<Seq>) {
        self.observe_delegate(event);
        if self.watchers.is_empty() {
            return;
        }
        let arg = watcher_arg(event);
        let ephemeral = event.class() == Class::Ephemeral;
        let seq = seq.map(|seq| seq.0);
        // The cursor lock is the leaf: the hub lock joins it per push,
        // never the reverse.
        let mut cursors = self.cursors.lock().unwrap_or_else(PoisonError::into_inner);
        for watcher in &self.watchers {
            if !watcher.kinds.iter().any(|kind| kind == event.kind()) {
                continue;
            }
            if ephemeral && watcher.extension.is_watcher_busy(watcher.index) {
                continue;
            }
            if watcher
                .extension
                .push_watcher(watcher.index, arg.clone())
                .is_some()
                && let Some(seq) = seq
            {
                cursors.insert((watcher.name.clone(), watcher.index), seq);
            }
        }
    }

    /// Records the log end when the subscription is registered: what an
    /// unset cursor defaults to at the drain.
    pub fn set_watermark(&self, count: u64) {
        *self
            .watermark
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(count);
    }

    /// Replays each watcher's `(cursor, end]` log range after `fiber_exited`
    /// was written: what outlives the session's turn. Watchers run in
    /// extension-name order then registration order, each within its own
    /// declared timeout, and every offered line advances its cursor. A
    /// failure is collected, never noticed — the log is closed to new
    /// lines — except a stop, which ends the drain at once and comes back
    /// as what the wait answered. A watcher with no cursor drains from the
    /// watermark, so lines before registration never reach it. No-op when
    /// the session holds no watchers.
    pub fn drain_watchers(&self, log: &log::Log, dir: &Path) -> (Vec<WatcherError>, bool) {
        if self.watchers.is_empty() {
            return (Vec::new(), false);
        }
        // A durable delivery the live offer already covered runs before the
        // drain replays past it: the cursor advanced when it queued, so the
        // drain's range skips it, and the teardown after the drain would lose
        // it still queued or running. One wait per extension; the hub wakes
        // on every finish and on the stop, which ends the wait and is then
        // refused as `Stopped` by the drain's own push, never lost silent.
        let mut extensions: Vec<Arc<LuaExtension>> = Vec::new();
        for watcher in &self.watchers {
            if !extensions
                .iter()
                .any(|held| Arc::ptr_eq(held, &watcher.extension))
            {
                extensions.push(Arc::clone(&watcher.extension));
            }
        }
        for extension in &extensions {
            extension.wait_for_watchers_idle();
        }
        let offered = self
            .cursors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let watermark = *self
            .watermark
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut errors = Vec::new();
        let mut replay = Vec::new();
        // The range the drain replays: what the log held when the session
        // ended, so nothing appended after — there is nothing after — can
        // slip in while the drain runs.
        let end = log.count();
        // A log the drain cannot read replays nothing, and the failure is
        // collected like a watcher failure: the drain is the report, since
        // the log is closed to new lines.
        let lines = match log::lines(dir) {
            Ok(lines) => lines,
            Err(error) => {
                errors.push(WatcherError {
                    extension: "log".to_owned(),
                    callback: "drain".to_owned(),
                    message: format!("the session-end drain could not read the log: {error}"),
                });
                return (errors, false);
            }
        };
        for line in lines {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    errors.push(WatcherError {
                        extension: "log".to_owned(),
                        callback: "drain".to_owned(),
                        message: format!(
                            "the session-end drain skipped an unreadable log line: {error}"
                        ),
                    });
                    continue;
                }
            };
            let Some(seq) = line.seq.map(|seq| seq.0) else {
                continue;
            };
            if seq >= end {
                continue;
            }
            let event = match Event::from_envelope(&line) {
                Ok(Some(event)) => event,
                // A kind this build does not know is none of this session's
                // watchers' business (`docs/events.md`, "Versioning").
                Ok(None) => continue,
                Err(error) => {
                    errors.push(WatcherError {
                        extension: "log".to_owned(),
                        callback: "drain".to_owned(),
                        message: format!(
                            "the session-end drain skipped a line it could not read as an event: {error}"
                        ),
                    });
                    continue;
                }
            };
            replay.push((seq, event));
        }
        let mut advanced = BTreeMap::new();
        let mut stopped = false;
        'watchers: for watcher in &self.watchers {
            let key = (watcher.name.clone(), watcher.index);
            // A cursor names the last offered `seq`, so the next one is
            // the bound; the watermark is already the log count at
            // registration, so the first line after it is the bound.
            let Some(mut from) = offered
                .get(&key)
                .copied()
                .map(|offered| offered + 1)
                .or(watermark)
            else {
                continue;
            };
            for (seq, event) in &replay {
                if *seq < from || !watcher.kinds.iter().any(|kind| kind == event.kind()) {
                    continue;
                }
                match watcher
                    .extension
                    .drain_watcher(watcher.index, watcher_arg(event))
                {
                    Ok(()) => {}
                    Err(Error::Stopped { .. }) => {
                        stopped = true;
                        break 'watchers;
                    }
                    Err(error) => errors.push(WatcherError {
                        extension: watcher.name.clone(),
                        callback: format!("watcher:{}", watcher.index),
                        message: error.to_string(),
                    }),
                }
                from = *seq;
                advanced.insert(key.clone(), *seq);
            }
        }
        self.cursors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(advanced);
        (errors, stopped)
    }

    /// Hands the ephemeral emitter to every extension's `host.status`,
    /// `host.widget` and `host.emit`.
    pub fn emit_to(&self, emit: Arc<dyn contract::emit::Emit>) {
        {
            *self.emitter.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&emit));
        }
        #[cfg(test)]
        self.at_late(LatePoint::Stored);
        for extension in self.attached_vms() {
            extension.set_emit(Arc::clone(&emit));
        }
    }

    #[cfg(test)]
    pub(crate) fn pause_at_late(&self, pause: LatePause) {
        *self
            .late_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(pause);
    }

    #[cfg(test)]
    fn at_late(&self, point: LatePoint) {
        let pause = self
            .late_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(pause) = pause {
            pause(point);
        }
    }

    /// Hands the in-process driver to every extension's `host.drive`.
    pub fn drive_to(&self, drive: Arc<dyn contract::extension::Drive>) {
        for extension in &self.lua {
            extension.set_driver(Arc::clone(&drive));
        }
    }

    /// Hands the session's model to every extension's `host.model`.
    pub fn model_to(&self, model: Arc<dyn contract::model::HostModel>) {
        for extension in &self.lua {
            extension.set_model(Arc::clone(&model));
        }
    }

    /// The started VM for `name`, for tests that drive one loaded extension.
    #[cfg(test)]
    pub(crate) fn lua_extension(&self, name: &str) -> Option<Arc<LuaExtension>> {
        self.lua.iter().find(|ext| ext.name() == name).cloned()
    }

    /// Forgets the delegate bindings for `jobs` on every loaded extension:
    /// after the normal end releases an id, its completion queues no `run`
    /// and its queued item never starts. Idempotent, and never blocks on a
    /// job's end; never called under the hub lock.
    pub fn release_delegates(&self, jobs: &[contract::JobId]) {
        for extension in &self.lua {
            extension.hub().release_delegates(jobs);
        }
    }

    /// Hands the session's delegate starter to every extension's
    /// `host.delegate`, then stops the running delegates of extensions no
    /// longer loaded: at session start nothing runs, so nothing stops, and
    /// on a reload a dropped extension's delegates end cancelled.
    pub fn delegate_to(&self, delegate: Arc<dyn contract::extension::HostDelegate>) {
        for extension in &self.lua {
            extension.set_delegate(Arc::clone(&delegate));
        }
        let loaded: Vec<String> = self.loaded_names();
        delegate.retain(&loaded);
    }

    /// Whether the session loaded any extension, even one without hooks.
    pub fn has_extensions(&self) -> bool {
        !self.loaded.is_empty()
    }

    /// Refuses further state writes before the final drain.
    pub fn close_state(&self) {
        self.state.close();
    }

    fn set_change_lines(&self, lines: Vec<StateLine>) {
        let mut slot = self
            .change_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        debug_assert!(
            slot.is_empty(),
            "change lines slot not taken before the next hook call"
        );
        *slot = lines;
    }

    /// Whether any extension registered a hook at any point.
    pub fn has_hooks(&self) -> bool {
        self.any
    }

    /// Records `name`'s registrations: a notice per hook that did not
    /// register, and its hooks onto `chain`.
    fn register(&mut self, name: &str, declared: DeclaredHooks, chain: &mut chain::Builder) {
        for problem in &declared.problems {
            self.notices.push(notice(
                ErrorCode::ExtensionFailed,
                problem.clone(),
                Some(name),
            ));
        }
        self.any |= declared.by_point.values().any(|hooks| !hooks.is_empty());
        chain.push(name, self.lua.len(), &declared);
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
    fn take_state_lines(&self) -> Vec<Event> {
        self.state.take_outbox()
    }

    fn admit_state_lines(&self, lines: Vec<StateLine>) -> Vec<Event> {
        let mut events = self.state.take_outbox();
        events.extend(self.state.admit(lines));
        events
    }

    fn take_change_state(&self) -> Vec<StateLine> {
        std::mem::take(
            &mut *self
                .change_state
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    fn after_tool(&self, call: &AfterToolCall<'_>) -> AfterToolAnswer {
        // A path that is not a regular file counts as a failure of the
        // chain's `after_tool` hooks for that pass, under each hook's own
        // `on_failure` (`docs/extensions.md`, "When a hook fails"). No
        // hook runs: the answer synthesizes the failure from the
        // registration, which the pass knows before it opens the file.
        if call.not_regular {
            let chain = self
                .after_tool
                .iter()
                .filter_map(|entry| {
                    let extension = self.lua.get(entry.extension)?;
                    Some((extension.name().to_owned(), entry.blocking))
                })
                .collect::<Vec<_>>();
            return contract::hook::not_regular_answer(&chain);
        }
        let mut content: Option<String> = None;
        let mut details: Option<Value> = None;
        let mut artifact: Option<String> = None;
        let mut changed_by: Vec<String> = Vec::new();
        let mut notices = Vec::new();
        let mut pending = Vec::new();
        for entry in &self.after_tool {
            let Some(extension) = self.lua.get(entry.extension) else {
                continue;
            };
            if let Some(failure) = before::own_command_failure(extension, AFTER_TOOL) {
                return AfterToolAnswer {
                    outcome: AfterToolOutcome::Failed(failure),
                    notices,
                    changed_by,
                };
            }
            let name = extension.name();
            let arg = shown(
                call,
                content.as_deref().unwrap_or(call.content),
                details.as_ref().or(call.details),
            );
            let returned = extension
                .hook_with(AFTER_TOOL, entry.index, arg, |value| {
                    let change = decode(name, value)?;
                    let changed = change.content.is_some()
                        || change.details.is_some()
                        || change.artifact.is_some();
                    Ok((change, changed))
                })
                .map(|(change, lines, changed)| {
                    if changed {
                        pending.extend(lines);
                    } else {
                        self.state.queue(lines);
                    }
                    change
                });
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
            self.set_change_lines(pending);
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

    fn before_tool(&self, call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        before::before_tool(self, call)
    }

    fn check_tool(&self, call: &BeforeToolCall<'_>) -> CheckAnswer {
        before::check_tool(self, call)
    }

    fn before_message(&self, message: &BeforeMessage<'_>) -> BeforeMessageAnswer {
        let _timer = self.timed(HookChain::BeforeMessage);
        before::before_message(self, message)
    }

    fn session_start(&self, start: &SessionStart<'_>) -> ContextAnswer {
        let _timer = self.timed(HookChain::SessionStart);
        lifecycle::session_start(self, start)
    }

    fn turn_start(&self, turn: &TurnStart<'_>) -> ContextAnswer {
        let _timer = self.timed(HookChain::TurnStart);
        lifecycle::turn_start(self, turn)
    }

    fn before_model_call(&self, call: &BeforeModelCall<'_>) -> CheckAnswer {
        let _timer = self.timed(HookChain::BeforeModelCall);
        lifecycle::before_model_call(self, call)
    }

    fn turn_end(&self, end: &TurnEnd<'_>) -> ContextAnswer {
        let _timer = self.timed(HookChain::TurnEnd);
        lifecycle::turn_end(self, end)
    }

    fn before_handoff(&self, handoff: &BeforeHandoff<'_>) -> BeforeHandoffAnswer {
        let _timer = self.timed(HookChain::BeforeHandoff);
        handoff::before_handoff(self, handoff)
    }

    fn wait_for_runs(&self) {
        // Every live VM delivers to the session inbox, hook and provider
        // alike, so the shutdown wait covers each one; a shared VM is
        // waited on once.
        for extension in self.attached_vms() {
            extension.wait_for_runs();
        }
    }

    fn deliver_to(&self, inbox: std::sync::mpsc::Sender<Delivery>) {
        // A later `deliver_to` (a resume hands a new sender) replaces the
        // sender, on the retained copy and on every live VM; each extension
        // flushes what ended before the first one. The guard drops before
        // the sweep, so a concurrent late start never waits on this lock.
        *self.inbox.lock().unwrap_or_else(PoisonError::into_inner) = Some(inbox.clone());
        // Collected under the weak-map lock and released before handing the
        // sender over, deduped so a shared VM is handed it once.
        for extension in self.attached_vms() {
            extension.deliver_to(inbox.clone());
        }
    }
}

/// The config list's strings: non-strings are skipped, anything but an
/// array is empty, as an MCP server's `tools.enabled` and `tools.disabled`.
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The extension's tools left after its `tools.enabled` and
/// `tools.disabled` lists (`docs/configuration.md`, "Keys"): `enabled`
/// names only these, `disabled` removes those, both is enabled minus
/// disabled; a name no tool has changes nothing. Filtering here, before
/// names settle, keeps a left-out tool out of the clash check too.
fn keep_listed(config: &Config, name: &str, tools: Vec<LuaTool>) -> Vec<LuaTool> {
    let list = |field: &str| {
        config
            .get(&format!("extensions.\"{name}\".tools.{field}"), None)
            .map(|(value, _)| strings(&value))
    };
    let enabled = list("enabled");
    let disabled = list("disabled").unwrap_or_default();
    tools
        .into_iter()
        .filter(|tool| {
            let tool = tool.definition().name;
            if let Some(enabled) = enabled.as_ref()
                && !enabled.iter().any(|listed| listed == &tool)
            {
                return false;
            }
            !disabled.iter().any(|listed| listed == &tool)
        })
        .collect()
}

/// What an `after_tool` hook is given: the call, with `content` and
/// `details` as the hook before it left them, the delivery word when this
/// is a job delivery, and the spilled file's `path` and `size` when the
/// tool already wrote it. A delivery from no tool leaves `tool` out; a
/// large output file leaves `content` out, since the file is the input.
fn shown(call: &AfterToolCall<'_>, content: &str, details: Option<&Value>) -> Value {
    let mut shown = Map::new();
    if call.delivery.is_none() || !call.tool.is_empty() {
        shown.insert("tool".into(), Value::String(call.tool.to_owned()));
    }
    shown.insert("arguments".into(), Value::Object(call.arguments.clone()));
    shown.insert(
        "status".into(),
        serde_json::to_value(call.status).unwrap_or(Value::Null),
    );
    if call.delivery != Some(JobDelivery::OutputFile) || call.spilled.is_none() {
        shown.insert("content".into(), Value::String(content.to_owned()));
    }
    if let Some(delivery) = call.delivery {
        shown.insert(
            "delivery".into(),
            Value::String(delivery.as_str().to_owned()),
        );
    }
    if let Some(details) = details {
        shown.insert("details".into(), details.clone());
    }
    if let Some(process) = call.process.and_then(|p| serde_json::to_value(p).ok()) {
        shown.insert("process".into(), process);
    }
    if let Some(spilled) = call.spilled {
        shown.insert(
            "path".into(),
            Value::String(spilled.path.display().to_string()),
        );
        shown.insert(
            "size".into(),
            Value::Number(serde_json::Number::from(spilled.size)),
        );
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

/// What the session owes declared hooks when the `hooks` extension did
/// not load (`docs/extensions.md`, "Hooks declared in configuration"):
/// nothing, a notice, or a failed start, so a guard never fails open.
/// Entries with `watch` keys are watcher entries (#565) and never trip
/// this gate. A settings read that fails is not a missing extension:
/// the error propagates, so malformed settings fail as `config_invalid`
/// and unreadable ones as `io_failed` (`docs/errors.md`).
pub fn hooks_state(config: &Config, loaded: &[String]) -> Result<HooksState, Error> {
    let name = config::full_name("hooks");
    let declared = config.extensions().get(&name, &["hooks"], "hooks")?;
    let Some(declared) = declared else {
        return Ok(HooksState::Ready);
    };
    let Value::Object(entries) = declared else {
        return Ok(HooksState::MissingBlocking);
    };
    if entries.is_empty() {
        return Ok(HooksState::Ready);
    }
    let mut blocking = false;
    let mut hooks = false;
    for entry in entries.values() {
        let Value::Object(entry) = entry else {
            return Ok(HooksState::MissingBlocking);
        };
        if entry.contains_key("watch") {
            continue;
        }
        hooks = true;
        // A missing or invalid `on_failure` counts as `blocking`: a
        // forgotten guard must not fail open as `non-blocking` would.
        if entry.get("on_failure").and_then(Value::as_str) != Some("non-blocking") {
            blocking = true;
            break;
        }
    }
    if !hooks {
        return Ok(HooksState::Ready);
    }
    if loaded.contains(&name) {
        return Ok(HooksState::Ready);
    }
    if blocking {
        return Ok(HooksState::MissingBlocking);
    }
    Ok(HooksState::MissingNotice {
        notice: notice(
            ErrorCode::ExtensionMissing,
            "The `hooks` extension is not installed, so its declared hooks will not run. Run `fiber extension install hooks`."
                .to_owned(),
            Some(&name),
        ),
    })
}

/// What a session with declared hooks owes them when the `hooks`
/// extension did not load (`docs/extensions.md`, "Hooks declared in
/// configuration").
#[derive(Debug, PartialEq)]
pub enum HooksState {
    /// No hook declarations, an empty `hooks` key, watcher-only
    /// declarations, or the extension loaded: the session goes on.
    Ready,
    /// Declared-but-unloaded non-empty all-`non-blocking` hooks: the
    /// session starts and one notice names the missing extension.
    MissingNotice {
        /// The notice the load pushes, naming the missing extension.
        notice: Notice,
    },
    /// Declared-but-unloaded hooks with any `blocking` entry or a missing
    /// or invalid `on_failure`: the start fails with `extension_missing`.
    MissingBlocking,
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
