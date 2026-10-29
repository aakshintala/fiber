//! A session's extensions and their hooks (`docs/extensions.md`, "Hooks",
//! "When several hooks share a point", "When a hook fails" and "Loading,
//! and cost when nothing is loaded"). Enabled extensions with `init.lua`
//! start before the session's tool set is fixed ("Registering"). Provider
//! files wait for first use and share their extension's VM.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use support::lock;

use config::Config;
use contract::ErrorCode;
use contract::Seq;
use contract::clock::Clock;
use contract::diag::{DebugLog, HookChain, Op, Timing};
use contract::events::{Class, Event, ExtensionReason, LoadedExtension, Notice};
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

use crate::commands::{CommandRunner, CommandSource, Lazy, ProcessCommandRunner, SessionCommands};
use crate::extension_tools::ProcessTool;
use crate::lua::{DeclaredHooks, LuaExtension};
use crate::package::Package;
use crate::runtime::{FunctionRef, FunctionTarget, LuaBinding, LuaKind, Runtime};
use crate::search::{ProcessSearchBackend, SearchBackendKind, SearchSource};
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
    /// The approved repository hooks this session runs, by name: each with
    /// its frozen declaration, pinned mapping, hash and copy. A skipped or
    /// unapproved replacement never removes the running snapshot; only an
    /// approved and materialized replacement swaps it.
    hook_snapshots: BTreeMap<String, crate::repository::ApprovedHook>,
    /// Each approved required hook whose copy could not be made: what the
    /// required gate fails the start with.
    hook_failures: Vec<crate::repository::HookFailure>,
    /// Whether the repository declares any hooks (approved or not): when
    /// it does, the filtered configuration withholds unapproved entries
    /// even when no snapshot is approved.
    has_repository_hooks: bool,
    /// The started Lua extensions.
    lua: Vec<Arc<LuaExtension>>,
    /// The session's backend-neutral callback owner: Lua extensions
    /// register their VMs here, process extensions their generations, and
    /// every hook, watcher, tool, search and command dispatch resolves
    /// through it. Provider and harness calls stay on `LuaExtension`
    /// directly.
    runtime: Runtime,
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
    /// Process registrations remain available after routing releases its
    /// initial provider handles; their owners retain the session services.
    process_providers: BTreeMap<(String, String), Arc<LuaProvider>>,
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
    /// One live process owner per started process extension, in load
    /// order: what the session forwards live events to, replays stored
    /// lines to and shuts down at session end.
    processes: Vec<ProcessLive>,
    /// The last forwarded durable `seq` per live process owner, keyed by
    /// extension name: seeded from the start context's cursor, advancing
    /// as each line forwards. A line at or below the cursor never
    /// forwards twice.
    forwarded: Mutex<BTreeMap<String, u64>>,
    /// The session's latest `session_start` reason: what a restart
    /// resends in its hello. Set at activation, updated on every
    /// `session_start`.
    latest_reason: Mutex<ExtensionReason>,
    /// The session's extension commands, built once from the same final names.
    commands: SessionCommands,
    /// The tools the session declares, each beside its extension's name.
    tools: Vec<(String, Arc<dyn Tool>)>,
    /// The search backend Fiber's own `web_search` runs, chosen at load
    /// from what is installed and `web_search.backend`.
    search_backend: Option<Arc<dyn SearchBackend>>,
}

impl Default for SessionExtensions {
    /// An empty session on the system clock: a load always replaces the
    /// runtime with the session's clock before dispatch, and tests fold
    /// registrations into the empty session directly.
    fn default() -> Self {
        Self {
            state: Default::default(),
            change_state: Default::default(),
            loaded: Default::default(),
            dirs: Default::default(),
            home: Default::default(),
            host_script: Default::default(),
            openings: Default::default(),
            prompts: Default::default(),
            notices: Default::default(),
            holds: Default::default(),
            required: Default::default(),
            hook_snapshots: Default::default(),
            hook_failures: Default::default(),
            has_repository_hooks: Default::default(),
            lua: Default::default(),
            runtime: Runtime::new(Arc::new(support::clock::System)),
            watchers: Default::default(),
            cursors: Default::default(),
            watermark: Default::default(),
            lua_providers: Default::default(),
            provider_vms: Default::default(),
            process_providers: Default::default(),
            browser: Default::default(),
            emitter: Default::default(),
            inbox: Default::default(),
            #[cfg(test)]
            late_pause: Default::default(),
            rejected: Default::default(),
            before_tool: Default::default(),
            before_message: Default::default(),
            after_tool: Default::default(),
            lifecycle: Default::default(),
            timing: Default::default(),
            diagnostics: Default::default(),
            any: Default::default(),
            processes: Default::default(),
            forwarded: Default::default(),
            latest_reason: Mutex::new(ExtensionReason::New),
            commands: Default::default(),
            tools: Default::default(),
            search_backend: Default::default(),
        }
    }
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
    package: Option<Arc<Package>>,
    declared: Option<DeclaredHooks>,
    commands: Vec<(String, String)>,
    tools: Vec<LuaTool>,
    backends: Vec<String>,
    providers: Vec<String>,
    /// For an extension whose manifest has a `commands` key: the start its
    /// first command run owes.
    lazy: Option<Arc<Lazy>>,
}

impl Started {
    /// The settle metadata of a started process package, from the
    /// registrations its process sent so far: its commands and tools join
    /// the same conflict and `replaces` checks Lua registrations meet.
    fn process_meta(&self) -> Option<Meta> {
        let registry = self.package.as_ref()?.registry()?;
        Some(Meta {
            extension: self.name.clone(),
            replaces: self.manifest.replaces.clone(),
            commands: registry
                .commands()
                .iter()
                .map(|(name, entry)| (name.clone(), entry.description.clone()))
                .collect(),
            tools: registry.tools().keys().cloned().collect(),
        })
    }

    /// The command source of a started process package: its commands list
    /// under their descriptions and run through their runtime references.
    fn process_command_source(&self, runtime: &Runtime) -> Option<CommandSource> {
        let registry = self.package.as_ref()?.registry()?;
        let mut commands = BTreeMap::new();
        for (name, entry) in registry.commands() {
            commands.insert(name.clone(), entry.run.clone());
        }
        Some(CommandSource {
            extension: self.name.clone(),
            replaces: self.manifest.replaces.clone(),
            commands: registry
                .commands()
                .iter()
                .map(|(name, entry)| (name.clone(), entry.description.clone()))
                .collect(),
            runner: CommandRunner::Process(ProcessCommandRunner {
                runtime: runtime.clone(),
                commands,
            }),
        })
    }

    /// The search sources of a started process package: each backend runs
    /// behind its runtime reference.
    fn process_search_sources(&self, runtime: &Runtime) -> Vec<SearchSource> {
        let Some(registry) = self.package.as_ref().and_then(|package| package.registry()) else {
            return Vec::new();
        };
        registry
            .search()
            .iter()
            .map(|(name, (_, run))| SearchSource {
                extension: self.name.clone(),
                name: name.clone(),
                backend: SearchBackendKind::Process(ProcessSearchBackend {
                    runtime: runtime.clone(),
                    owner: self.name.clone(),
                    run: run.clone(),
                }),
            })
            .collect()
    }
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

/// Where one session extension's data directories come from: Fiber home
/// and the session's project, joined with the extension's slug at load,
/// exactly as the Lua half locates them.
#[derive(Debug, Clone)]
pub struct DataDirBases {
    /// Fiber home, anchoring `extensions/<slug>/` machine directories.
    pub home: PathBuf,
    /// The session's project, anchoring `projects/<key>/<slug>/` data.
    pub project: config::ProjectKey,
}

/// What a session activation starts process extensions with: the
/// session's identity, why it started, its workspace, where its data
/// directories come from, the driver once the session opened it, and the
/// log cursor captured before the first process started.
#[derive(Clone)]
pub struct StartContext {
    /// The session's id, as `extension_hello` carries it.
    pub session_id: contract::SessionId,
    /// Why the session started, resent after a restart.
    pub reason: ExtensionReason,
    /// The session's execution workspace, resolving relative paths.
    pub workspace: PathBuf,
    /// Where the extension's data directories come from.
    pub data_dirs: DataDirBases,
    /// What answers ordinary driver commands: `None` while the session
    /// has not opened one yet, installed on the live owners after.
    pub drive: Option<Arc<dyn contract::extension::Drive>>,
    /// The log cursor captured before the first process started: each
    /// owner's durable cursor starts here.
    pub event_cursor: u64,
}

/// One started process extension the session owns: its name, its live
/// owner, its late-bound session services and its finish allowance.
struct ProcessLive {
    name: String,
    owner: Arc<crate::process::ProcessExtension>,
    services: Arc<crate::process::context::NativeHostServices>,
    exit_timeout: Duration,
}

/// The watcher's one argument: its `kind` beside its `payload`, serialized
/// once per event (`run` receives `{"kind": ..., "payload": ...}`).
fn watcher_arg(event: &Event) -> Value {
    serde_json::json!({
        "kind": event.kind(),
        "payload": Value::Object(event.payload().unwrap_or_default()),
    })
}

/// One watcher `fiber.watch` registered: its extension's name, its
/// function behind the session's runtime, its place in registration order,
/// and the event kinds it wants.
struct WatcherRegistration {
    name: String,
    watcher: FunctionRef,
    index: usize,
    kinds: Vec<String>,
    entry: Option<String>,
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
            None,
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
            None,
        )
    }

    /// Loads extensions for a live session, starting process extensions:
    /// each process child runs with the start context's identity, reason,
    /// workspace, data directories and folded state, answering through
    /// session-backed services. The load waits for each child's `ready`
    /// under its startup allowance before admitting it; a child that
    /// misses it is left out with a notice, and a required miss carries
    /// the required code the composition gate fails the session with.
    /// `restart` names the lazy extensions to start, as a reload passes.
    #[allow(
        clippy::too_many_arguments,
        reason = "one live session load carries its context and existing shared services"
    )]
    pub fn load_session(
        home: &Path,
        config: &Config,
        clock: Arc<dyn Clock>,
        locks: Arc<dyn PathLock>,
        host: Option<Arc<crate::host::script::HostScript>>,
        seed: StateSeed,
        repository: crate::RepositorySet,
        start: StartContext,
        restart: BTreeSet<String>,
    ) -> Self {
        let session = Self::load_registry(
            home,
            config,
            clock,
            locks,
            host,
            crate::host::state::StateRegistry::new(seed),
            repository,
            restart,
            Some(&start),
        );
        *lock(&session.latest_reason) = start.reason;
        session
    }

    /// The session's latest `session_start` reason: what a restart
    /// resends in its hello.
    pub fn latest_reason(&self) -> ExtensionReason {
        *lock(&self.latest_reason)
    }

    /// Records the session's latest `session_start` reason on the set
    /// and every live process owner, before the hook chain runs.
    pub(crate) fn record_start_reason(&self, reason: ExtensionReason) {
        *lock(&self.latest_reason) = reason;
        for live in &self.processes {
            live.owner.update_reason(reason);
        }
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
            None,
        )
    }

    /// Reloads extensions with the repository set over the session's live
    /// state without closing it. The set's holds move into the reloaded
    /// session, so its copies live until the next reload replaces them. An
    /// old approved hook stays running when its replacement is unapproved,
    /// skipped or failed: only an approved and materialized replacement
    /// swaps the snapshot, and the old copy stays held until then.
    pub fn reload_repository(
        &self,
        home: &Path,
        config: &Config,
        clock: Arc<dyn Clock>,
        locks: Arc<dyn PathLock>,
        mut repository: crate::RepositorySet,
    ) -> Self {
        for (name, snapshot) in &self.hook_snapshots {
            let has_new_approved = matches!(
                repository.hooks.get(name),
                Some(crate::repository::HookState::Approved(_))
            );
            if has_new_approved {
                continue;
            }
            // A failed gather keeps the running snapshot: the replacement was
            // never classified, so the old copy stays held until an approved
            // and materialized replacement swaps it.
            let new_has_entry = repository.hooks.contains_key(name)
                || repository.hook_failures.iter().any(|f| &f.name == name)
                || repository.hooks_failed;
            if !new_has_entry {
                continue;
            }
            let project = config.extensions().project().clone();
            let store = crate::repository::Store::new(home, &project, Arc::clone(&clock));
            if let Ok(hold) = store.try_hold_finished(&snapshot.hash) {
                repository.holds.push(hold);
                repository.hooks.insert(
                    name.clone(),
                    crate::repository::HookState::Approved(snapshot.clone()),
                );
            }
        }
        Self::load_registry(
            home,
            config,
            clock,
            locks,
            self.host_script.clone(),
            self.state.clone(),
            repository,
            self.started_lazy(),
            None,
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

    /// The first approved required hook whose pinned copy could not be made,
    /// with what making it failed with: what the required gate fails the
    /// start with. The repository's `required` decides, never the merged
    /// person boolean.
    pub fn first_hook_required_failure(&self) -> Option<(String, String)> {
        self.hook_failures
            .iter()
            .find(|failure| failure.required)
            .map(|failure| (failure.name.clone(), failure.error.to_string()))
    }

    /// One filtered `Config` clone for this session: the raw configuration
    /// with only this session's active approved hook snapshot as the
    /// repository's contribution to the `hooks` extension. An unapproved
    /// repository entry stays withheld before the layers merge, so the
    /// person's entry of that name runs unchanged; an approved one merges
    /// key by key in normal layer order. The same frozen mapping governs
    /// the hooks VM, `hooks_state` and main's `missing_hooks`.
    pub fn hooks_config(&self, config: &Config) -> Config {
        filtered_hooks_config(
            config,
            self.has_repository_hooks,
            self.hook_snapshots
                .iter()
                .map(|(name, snapshot)| (name.clone(), snapshot.entry.clone())),
        )
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
        start: Option<&StartContext>,
    ) -> Self {
        let mut session = Self {
            runtime: Runtime::new(Arc::clone(&clock)),
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
        session.has_repository_hooks = repository.has_hook_declarations();
        session.notices.extend(repository.notices);
        session.notices.append(&mut candidate_notices);
        session.holds = repository.holds;
        session.required = repository.required;
        for (name, state) in &repository.hooks {
            if let crate::repository::HookState::Approved(snapshot) = state {
                session
                    .hook_snapshots
                    .insert(name.clone(), snapshot.clone());
            }
        }
        session.hook_failures = repository.hook_failures;
        // The authorized repository contribution for the hooks extension:
        // approved entries mapped to their pinned copies, withheld entries
        // absent so the person's entry runs unchanged.
        let filtered = session.hooks_config(config);
        let config = &filtered;
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
            if manifest.api != API {
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
            // A process extension loads without a VM: its registrations
            // arrive as messages once its process starts, so it is
            // admitted with none yet, and its `.lua` files never run
            // (`docs/extensions.md`, "Process extensions"). Only its data
            // providers need declaring, as for a Lua extension.
            if manifest.process.is_some() {
                // A session activation starts the child; every other load
                // only reads the package, as before.
                if let Some(start) = start {
                    Self::start_process(
                        &mut session,
                        &clock,
                        &locks,
                        config,
                        home,
                        &host,
                        start,
                        name,
                        version,
                        slug,
                        dir,
                        manifest,
                        prompt,
                        &mut started,
                    );
                    continue;
                }
                // A process package loads through the package seam on the
                // session's runtime, so its references resolve where the
                // session dispatches them. A package the seam refuses is
                // not loaded, with the refusal's notice.
                let package = match crate::package::Package::load_with_runtime(
                    crate::package::PackageOptions {
                        dir: dir.clone(),
                        clock: Arc::clone(&clock),
                    },
                    session.runtime.clone(),
                ) {
                    Ok(package) => package,
                    Err(e) => {
                        session.failed(&name, &e);
                        continue;
                    }
                };
                started.push(Started {
                    name,
                    version,
                    slug,
                    dir,
                    manifest,
                    prompt,
                    extension: None,
                    package: Some(package),
                    declared: None,
                    commands: Vec::new(),
                    tools: Vec::new(),
                    backends: Vec::new(),
                    providers: Vec::new(),
                    lazy: None,
                });
                continue;
            }
            let mut entry = Started {
                name,
                version,
                slug,
                dir,
                manifest,
                prompt,
                extension: None,
                package: None,
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
            .filter_map(|s| {
                if s.extension.is_some() {
                    Some(Meta {
                        extension: s.name.clone(),
                        replaces: s.manifest.replaces.clone(),
                        commands: s.commands.clone(),
                        tools: s.tools.iter().map(|tool| tool.definition().name).collect(),
                    })
                } else {
                    s.process_meta()
                }
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
                if let Some(lua) = s.extension.as_ref() {
                    Some(CommandSource {
                        extension: s.name.clone(),
                        replaces: s.manifest.replaces.clone(),
                        commands: s.commands.clone(),
                        runner: crate::commands::CommandRunner::Lua(Arc::clone(lua)),
                    })
                } else {
                    s.process_command_source(&session.runtime)
                }
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
        let mut backend_choices: Vec<crate::search::SearchSource> = Vec::new();
        for s in &started {
            if let Some(lua) = &s.extension {
                for backend in &s.backends {
                    backend_choices.push(crate::search::SearchSource {
                        extension: s.name.clone(),
                        name: backend.clone(),
                        backend: crate::search::SearchBackendKind::Lua(Arc::clone(lua)),
                    });
                }
            } else if s.package.is_some() {
                backend_choices.extend(s.process_search_sources(&session.runtime));
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
            // Every kept VM registers behind the runtime exactly once, so
            // hooks and watchers mint their references under one
            // generation: a second registration would stale the first.
            if let Some(lua) = &s.extension {
                session.runtime.register_owner(&s.name, Some(lua));
            }
            // A lazy extension's VM stays unstarted but joins the session's
            // walks, which only set what `host.drive`, `host.model` and
            // `host.delegate` later read.
            if let (Some(lua), Some(_)) = (&s.extension, &s.lazy) {
                session.lua.push(Arc::clone(lua));
            }
            if let (Some(lua), Some(declared)) = (&s.extension, s.declared) {
                let runs_hooks = declared.by_point.values().any(|hooks| !hooks.is_empty());
                session.register(&s.name, lua, declared, &mut chain);
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
                        let made = session.runtime.register_function(
                            &s.name,
                            contract::CommandId(format!("lua:{}:watcher:{index}", s.name)),
                            "run".to_owned(),
                            FunctionTarget::Lua(LuaBinding {
                                extension: Arc::downgrade(lua),
                                kind: LuaKind::Watcher { index },
                            }),
                        );
                        session.watchers.push(WatcherRegistration {
                            name: s.name.clone(),
                            watcher: made,
                            index,
                            kinds: watcher.kinds.clone(),
                            entry: watcher.entry.clone(),
                        });
                    }
                    if !session.lua.iter().any(|held| Arc::ptr_eq(held, lua)) {
                        session.lua.push(Arc::clone(lua));
                    }
                }
            }
            // Stage the directory for provider metadata before admission;
            // a refused process package must not remain in the loaded set.
            session.dirs.push((s.name.clone(), s.dir));
            if let Some(package) = &s.package
                && !session.admit_process(&s.name, package, &mut chain, &settled.tools)
            {
                drop(session.dirs.pop());
                continue;
            }
            if let Some(opening) = s.manifest.opening {
                session.openings.push((s.name.clone(), s.slug, opening));
            }
            if let Some(text) = s.prompt {
                session.prompts.push((s.name.clone(), text));
            }
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

    /// Starts one process package for a session activation: the child
    /// runs with the start context's identity, reason, workspace, data
    /// directories and folded state, answering through session-backed
    /// services. The pump runs on its own thread while the load waits
    /// for `ready` under the extension's startup allowance; a child that
    /// misses it is reaped and left out, required ones failing the
    /// session at the composition gate. An optional package with no
    /// program loads empty, as without a session.
    #[allow(
        clippy::too_many_arguments,
        reason = "one candidate's activation carries the loader's context"
    )]
    fn start_process(
        session: &mut Self,
        clock: &Arc<dyn Clock>,
        locks: &Arc<dyn PathLock>,
        config: &Config,
        home: &Path,
        host: &Option<Arc<crate::host::script::HostScript>>,
        start: &StartContext,
        name: String,
        version: String,
        slug: String,
        dir: PathBuf,
        manifest: config::Manifest,
        prompt: Option<String>,
        started: &mut Vec<Started>,
    ) {
        let Some(process) = manifest.process.as_ref() else {
            return;
        };
        let required = process.required;
        let exit_timeout = Duration::from_millis(process.exit_timeout_ms);
        let store = session.state.store(&name);
        let project = config.extensions().project().clone();
        let data_dirs = contract::events::ExtensionDataDirs {
            machine: config::extension_data_dir(&start.data_dirs.home, &slug)
                .display()
                .to_string(),
            project: config::project_data_dir(&start.data_dirs.home, &project, &slug)
                .display()
                .to_string(),
        };
        let mut folded = Map::new();
        for key in store.keys() {
            if let Some(value) = store.get(&key) {
                folded.insert(key, value);
            }
        }
        let context = crate::process::context::ProcessContext {
            session_id: start.session_id.clone(),
            reason: start.reason,
            workspace: start.workspace.clone(),
            data_dirs,
            state: folded,
            extension: name.clone(),
            home: home.to_path_buf(),
        };
        let services = Arc::new(crate::process::context::NativeHostServices {
            runtime: session.runtime.clone(),
            state: Arc::clone(&store),
            clock: Arc::clone(clock),
            browser: Arc::clone(&session.browser) as Arc<dyn crate::oauth::Browser>,
            model: Mutex::new(None),
            delegate: Mutex::new(None),
            tool_runner: Mutex::new(None),
            secrets: manifest.secrets.clone(),
            host_script: host.clone(),
            session: Mutex::new(Some(crate::host::Session {
                config: config.clone(),
                repo_settings: manifest.repo_settings.clone(),
                locks: Arc::clone(locks),
            })),
        });
        let package = match crate::package::Package::load_process_with(
            crate::package::PackageOptions {
                dir: dir.clone(),
                clock: Arc::clone(clock),
            },
            session.runtime.clone(),
            context,
            services.clone(),
            store,
            start.drive.clone(),
        ) {
            Ok(package) => package,
            Err(error) => {
                Self::process_failed(session, &name, &(error.code(), error.to_string()), required);
                return;
            }
        };
        // One entry for the started package, kept below once the child
        // is pumped and ready: a spawn failure surfaces as `Err` above,
        // so only an owner reaches this entry.
        let entry = Started {
            name,
            version,
            slug,
            dir,
            manifest,
            prompt,
            extension: None,
            package: Some(package),
            declared: None,
            commands: Vec::new(),
            tools: Vec::new(),
            backends: Vec::new(),
            providers: Vec::new(),
            lazy: None,
        };
        let Some(owner) = entry
            .package
            .as_ref()
            .and_then(|package| package.extension().cloned())
        else {
            return;
        };
        let pump = Arc::clone(&owner);
        let pumped = std::thread::Builder::new()
            .name(format!("process pump {}", entry.name))
            .spawn(move || {
                loop {
                    let spawn = pump.transport().spawn_id();
                    match pump.pump_once() {
                        crate::process::Pumped::Handled => {}
                        crate::process::Pumped::Closed(_) => {
                            if !pump.wait_for_next_spawn(spawn) {
                                break;
                            }
                        }
                    }
                }
            });
        if pumped.is_err() {
            // The child runs with nobody pumping it: shut it down bounded
            // and leave it out, as a wedged startup.
            drop(owner.finish(Duration::ZERO, &support::cancel::Token::new()));
            Self::process_failed(
                session,
                &entry.name,
                &(
                    ErrorCode::IoFailed,
                    format!("{}: Fiber could not pump the process extension", entry.name),
                ),
                required,
            );
            return;
        }
        // The thread ends when the child closes; the shutdown below owns
        // the child from here, and `finish_processes` reaps it at the end.
        drop(pumped);
        let allowance = config
            .get(
                &format!("extensions.\"{}\".startup_timeout_ms", entry.name),
                None,
            )
            .and_then(|(value, _)| value.as_u64())
            .unwrap_or(5000);
        let deadline = clock.now() + Duration::from_millis(allowance);
        if let Err(error) = owner.await_ready(Some(deadline), &support::cancel::Token::new()) {
            // The wedged child may ignore end of input, so the shutdown
            // escalates through TERM to KILL under the manifest's exit
            // allowance; no descendant outlives the load.
            drop(owner.finish(exit_timeout, &support::cancel::Token::new()));
            // The deadline path already killed the child; reap it and join
            // its threads, since the failed owner never reaches the
            // session's finish list.
            let live = owner.transport();
            drop(live.wait_exit());
            live.join();
            Self::process_failed(session, &entry.name, &error, required);
            return;
        }
        session.processes.push(ProcessLive {
            name: entry.name.clone(),
            owner,
            services,
            exit_timeout,
        });
        lock(&session.forwarded).insert(entry.name.clone(), start.event_cursor);
        started.push(entry);
    }

    /// Records a process startup failure: the extension is left out with
    /// the failure's notice, and a required miss carries the required
    /// code the composition gate fails the session with.
    fn process_failed(session: &mut Self, name: &str, error: &(ErrorCode, String), required: bool) {
        let (code, message) = error;
        let code = if required {
            ErrorCode::ExtensionRequiredFailed
        } else {
            code.clone()
        };
        session
            .notices
            .push(notice(code, message.clone(), Some(name)));
    }

    /// One provider per `fiber.provider` registration, in provider-name
    /// order: the extension's name beside its provider.
    pub fn lua_providers(&self) -> &[(String, Arc<LuaProvider>)] {
        &self.lua_providers
    }

    /// The names of extensions refused for an undeclared Lua registration,
    /// in load order: what `Providers::add_session` drops the data of.
    pub fn rejected(&self) -> Vec<String> {
        lock(&self.rejected).clone()
    }

    fn reject(&self, name: &str) {
        lock(&self.rejected).push(name.to_owned());
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
        let provider_vms: Vec<Weak<LuaExtension>> =
            lock(&self.provider_vms).values().cloned().collect();
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
        let provider_vms: Vec<Weak<LuaExtension>> =
            lock(&self.provider_vms).values().cloned().collect();
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
    /// advances to it; an ephemeral offer carries none. Watchers of live
    /// process owners fan out through [`SessionExtensions::deliver_event`]
    /// instead, which tracks their owner's durable cursor first: each
    /// watcher still runs exactly once per offer. No-op when the session
    /// holds no watchers.
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
        let mut cursors = lock(&self.cursors);
        for watcher in &self.watchers {
            if !watcher.kinds.iter().any(|kind| kind == event.kind()) {
                continue;
            }
            if self.processes.iter().any(|live| live.name == watcher.name) {
                continue;
            }
            if ephemeral && self.runtime.is_watcher_busy(&watcher.watcher) {
                continue;
            }
            if self
                .runtime
                .push_watcher(&watcher.watcher, arg.clone(), None)
                .is_some()
                && let Some(seq) = seq
            {
                cursors.insert((watcher.name.clone(), watcher.index), seq);
            }
        }
    }

    /// Forwards one live session event envelope to every live process
    /// owner, in arrival order: each owner's durable cursor advances past
    /// durable lines whether or not any watcher wants the kind, and only
    /// matching watcher callbacks dispatch, as Lua's kinds filter does.
    /// Ephemeral skips a busy watcher, as [`SessionExtensions::deliver`]
    /// does. The process owner reads the original envelope, with its
    /// schema version and durable `seq`, while Lua watchers keep the
    /// filtered `kind`/`payload` argument [`SessionExtensions::deliver`]
    /// offers them.
    pub fn deliver_event(&self, event: &Event, line: &contract::Envelope) {
        self.observe_delegate(event);
        self.forward_process(event, line);
    }

    /// Replays one stored durable line to every live process owner, in
    /// sequence order: a line at or below an owner's cursor never
    /// forwards twice. Unknown lines are skipped, as the log drain does.
    pub fn deliver_line(&self, line: &contract::Envelope) {
        if line.seq.is_none() {
            return;
        }
        let Ok(Some(event)) = Event::from_envelope(line) else {
            return;
        };
        self.forward_process(&event, line);
    }

    /// Forwards `line` to each live process owner's matching watchers,
    /// advancing each owner's durable cursor past its `seq`: the per-owner
    /// stream position replay resumes from. A line without `seq` forwards
    /// live ephemeral events without moving any cursor.
    fn forward_process(&self, event: &Event, line: &contract::Envelope) {
        if self.processes.is_empty() {
            return;
        }
        let ephemeral = event.class() == Class::Ephemeral;
        let envelope = serde_json::to_value(line).unwrap_or(Value::Null);
        let mut forwarded = lock(&self.forwarded);
        for live in &self.processes {
            if let Some(seq) = line.seq.map(|seq| seq.0) {
                if seq <= forwarded.get(&live.name).copied().unwrap_or(0) {
                    continue;
                }
                forwarded.insert(live.name.clone(), seq);
            }
            for watcher in self
                .watchers
                .iter()
                .filter(|watcher| watcher.name == live.name)
            {
                if !watcher.kinds.iter().any(|kind| kind == event.kind()) {
                    continue;
                }
                if ephemeral && self.runtime.is_watcher_busy(&watcher.watcher) {
                    continue;
                }
                let _ = self
                    .runtime
                    .push_watcher(&watcher.watcher, envelope.clone(), None);
            }
        }
    }

    /// Shuts every live process owner down: the in-flight requests drain
    /// first under each manifest's own finish allowance, and a failure
    /// collects for the exit report, never a notice.
    pub fn finish_processes(&self, cancel: &dyn contract::tool::Cancel) -> Vec<WatcherError> {
        let mut errors = Vec::new();
        for live in &self.processes {
            if let Err((code, message)) = live.owner.finish(live.exit_timeout, cancel) {
                errors.push(WatcherError {
                    extension: live.name.clone(),
                    callback: "finish".to_owned(),
                    message: format!("{code:?}: {message}"),
                });
            }
        }
        errors
    }

    /// Records the log end when the subscription is registered: what an
    /// unset cursor defaults to at the drain.
    pub fn set_watermark(&self, count: u64) {
        *lock(&self.watermark) = Some(count);
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
        // it still queued or running. One wait per owner; the hub wakes
        // on every finish and on the stop, which ends the wait and is then
        // refused as `Stopped` by the drain's own push, never lost silent.
        let mut owners = Vec::new();
        let mut seen = BTreeSet::new();
        for watcher in &self.watchers {
            if let Some(owner) = self.runtime.owner_name(&watcher.watcher)
                && seen.insert(owner.clone())
            {
                owners.push(owner);
            }
        }
        for owner in &owners {
            self.runtime.wait_for_watchers_idle(owner);
        }
        let offered = lock(&self.cursors).clone();
        let watermark = *lock(&self.watermark);
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
            replay.push((seq, event, line));
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
            // A process owner reads the original envelope, as the live
            // forwarding does; a Lua watcher keeps its filtered argument.
            let process = self.processes.iter().any(|live| live.name == watcher.name);
            for (seq, event, line) in &replay {
                if *seq < from || !watcher.kinds.iter().any(|kind| kind == event.kind()) {
                    continue;
                }
                let arg = if process {
                    serde_json::to_value(line).unwrap_or(Value::Null)
                } else {
                    watcher_arg(event)
                };
                match self.runtime.drain_watcher(&watcher.watcher, arg, None) {
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
        lock(&self.cursors).extend(advanced);
        (errors, stopped)
    }

    /// Hands the ephemeral emitter to every extension's `host.status`,
    /// `host.widget` and `host.emit`.
    pub fn emit_to(&self, emit: Arc<dyn contract::emit::Emit>) {
        {
            *lock(&self.emitter) = Some(Arc::clone(&emit));
        }
        // Process failures have no VM to carry the sink: the runtime keeps
        // it for the waiter's report.
        self.runtime.set_emit(Arc::clone(&emit));
        #[cfg(test)]
        self.at_late(LatePoint::Stored);
        for extension in self.attached_vms() {
            extension.set_emit(Arc::clone(&emit));
        }
    }

    #[cfg(test)]
    pub(crate) fn pause_at_late(&self, pause: LatePause) {
        *lock(&self.late_pause) = Some(pause);
    }

    #[cfg(test)]
    fn at_late(&self, point: LatePoint) {
        let pause = lock(&self.late_pause).clone();
        if let Some(pause) = pause {
            pause(point);
        }
    }

    /// Hands the in-process driver to every extension's `host.drive`:
    /// Lua VMs take it directly, live process owners take it before any
    /// dispatch reaches them, as the late installation their spawn orders.
    pub fn drive_to(&self, drive: Arc<dyn contract::extension::Drive>) {
        for extension in &self.lua {
            extension.set_driver(Arc::clone(&drive));
        }
        for live in &self.processes {
            live.owner.set_drive(Arc::clone(&drive));
        }
    }

    /// Hands the session's model to every extension's `host.model`: Lua
    /// VMs take it directly, live process owners through their late-born
    /// services, before any dispatch reaches them.
    pub fn model_to(&self, model: Arc<dyn contract::model::HostModel>) {
        for extension in &self.lua {
            extension.set_model(Arc::clone(&model));
        }
        for live in &self.processes {
            *lock(&live.services.model) = Some(Arc::clone(&model));
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
    /// on a reload a dropped extension's delegates end cancelled. Live
    /// process owners take the starter through their late-bound services.
    pub fn delegate_to(&self, delegate: Arc<dyn contract::extension::HostDelegate>) {
        for extension in &self.lua {
            extension.set_delegate(Arc::clone(&delegate));
        }
        for live in &self.processes {
            *lock(&live.services.delegate) = Some(Arc::clone(&delegate));
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
        let mut slot = lock(&self.change_state);
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

    /// The hooks and watchers the session registered, hooks first in
    /// hook-point table order and run order, then watchers in registration
    /// order (`docs/invocation.md`, "What each command does", `hooks`).
    pub fn registrations(&self) -> Vec<contract::events::HookInfo> {
        use contract::events::HookInfo;
        let hooks_name = config::full_name("hooks");
        let entry_of = |extension: &str, entry: &Option<String>| {
            (extension == hooks_name).then(|| entry.clone()).flatten()
        };
        // A declared entry's protocol extension is `hooks`
        // (`docs/events.md`, "`command_accepted`"): the qualified
        // internal name never leaves the session.
        let extension_of = |extension: &str, entry: &Option<String>| {
            if entry_of(extension, entry).is_some() {
                "hooks".to_owned()
            } else {
                extension.to_owned()
            }
        };
        let phase_of = |phase: crate::lua::HookPhase, explicit: bool| {
            explicit.then(|| {
                match phase {
                    crate::lua::HookPhase::Sanitize => "sanitize",
                    crate::lua::HookPhase::Transform => "transform",
                    crate::lua::HookPhase::Check => "check",
                }
                .to_owned()
            })
        };
        let mut out = Vec::new();
        for chain in [
            &self.lifecycle.session_start,
            &self.before_message,
            &self.lifecycle.turn_start,
            &self.before_tool,
            &self.lifecycle.before_model_call,
            &self.after_tool,
            &self.lifecycle.turn_end,
            &self.lifecycle.before_handoff,
        ] {
            for entry in chain {
                out.push(HookInfo::Hook {
                    extension: extension_of(&entry.extension, &entry.entry),
                    entry: entry_of(&entry.extension, &entry.entry),
                    point: entry.point.to_owned(),
                    on_failure: if entry.blocking {
                        "blocking".to_owned()
                    } else {
                        "non-blocking".to_owned()
                    },
                    phase: phase_of(entry.phase, entry.phase_explicit),
                });
            }
        }
        for watcher in &self.watchers {
            out.push(HookInfo::Watcher {
                extension: extension_of(&watcher.name, &watcher.entry),
                entry: entry_of(&watcher.name, &watcher.entry),
                kinds: watcher.kinds.clone(),
            });
        }
        out
    }

    /// Records `name`'s registrations: a notice per hook that did not
    /// register, and its hooks onto `chain`, each behind the session's
    /// runtime. The caller registered the VM first, so the references mint
    /// under its current generation.
    fn register(
        &mut self,
        name: &str,
        lua: &Arc<LuaExtension>,
        declared: DeclaredHooks,
        chain: &mut chain::Builder,
    ) {
        for problem in &declared.problems {
            self.notices.push(notice(
                ErrorCode::ExtensionFailed,
                problem.clone(),
                Some(name),
            ));
        }
        self.any |= declared.by_point.values().any(|hooks| !hooks.is_empty());
        chain.push(&self.runtime, name, lua, &declared);
    }

    /// Folds a process `package` into the session: its refusals become
    /// notices, its tools join the settled tools, and its hooks and
    /// watchers run behind the references its registry minted. A package
    /// one call failed stays out, with the failure's notice, as a load
    /// failure does. Returns whether the package stays in the session.
    fn admit_process(
        &mut self,
        name: &str,
        package: &Package,
        chain: &mut chain::Builder,
        settled_tools: &BTreeSet<(String, String)>,
    ) -> bool {
        let Some(registry) = package.registry() else {
            return true;
        };
        if let Some((code, message)) = registry.failure() {
            self.notices.push(notice(code, message, Some(name)));
            self.reject(name);
            return false;
        }
        for problem in registry
            .problems()
            .iter()
            .chain(registry.watch_problems().iter())
        {
            self.notices.push(notice(
                ErrorCode::ExtensionFailed,
                problem.clone(),
                Some(name),
            ));
        }
        let runtime = self.runtime.clone();
        if !registry.providers().is_empty()
            && let Some(live) = self.processes.iter().find(|live| live.name == name)
        {
            let Some((_, dir)) = self.dirs.iter().rev().find(|(owner, _)| owner == name) else {
                return false;
            };
            let metadata = Arc::new(LuaExtension::new(
                name,
                dir,
                &self.home,
                Arc::clone(&live.services.clock),
            ));
            for (provider_name, entry) in registry.providers() {
                let mut provider = LuaProvider::new(Arc::clone(&metadata), provider_name);
                // `new` returns a unique handle. Bind the backend before
                // publishing any clone to session or routing consumers.
                let Some(handle) = Arc::get_mut(&mut provider) else {
                    return false;
                };
                handle.process = Some(crate::lua_provider::ProcessProvider {
                    runtime: runtime.clone(),
                    runs: Mutex::new(entry.runs.clone()),
                    owner: Arc::clone(&live.owner),
                });
                self.process_providers.insert(
                    (name.to_owned(), provider_name.clone()),
                    Arc::clone(&provider),
                );
                self.lua_providers.push((name.to_owned(), provider));
            }
        }
        for (tool, entry) in registry.tools() {
            if settled_tools.contains(&(name.to_owned(), tool.clone())) {
                self.tools.push((
                    name.to_owned(),
                    Arc::new(ProcessTool::new(
                        runtime.clone(),
                        name.to_owned(),
                        entry.declared.clone(),
                        entry.run.clone(),
                        entry.effects.clone(),
                    )),
                ));
            }
        }
        let hooks = registry.hooks();
        self.any |= hooks.by_point.values().any(|hooks| !hooks.is_empty());
        chain.push_process(name, hooks, registry.hook_runs());
        for (index, watcher) in registry.watchers().watchers.iter().enumerate() {
            let Some(run) = registry.watcher_runs().get(&index) else {
                continue;
            };
            self.watchers.push(WatcherRegistration {
                name: name.to_owned(),
                watcher: run.clone(),
                index,
                kinds: watcher.kinds.clone(),
                entry: watcher.entry.clone(),
            });
        }
        true
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
        std::mem::take(&mut *lock(&self.change_state))
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
                    self.runtime
                        .owner_name(&entry.hook)
                        .map(|name| (name, entry.blocking))
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
            let Some(name) = self.runtime.owner_name(&entry.hook) else {
                continue;
            };
            if let Some(failure) = self.runtime.runs_own_command(&entry.hook, AFTER_TOOL) {
                return AfterToolAnswer {
                    outcome: AfterToolOutcome::Failed(failure),
                    notices,
                    changed_by,
                };
            }
            let arg = shown(
                call,
                content.as_deref().unwrap_or(call.content),
                details.as_ref().or(call.details),
            );
            let returned = self
                .runtime
                .hook_with(&entry.hook, arg, None, |value| {
                    let change = decode(&name, value)?;
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
                        name_once(&mut changed_by, &name);
                    }
                }
                Err(_) if entry.blocking => {
                    name_once(&mut changed_by, &name);
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
                    Some(&name),
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
        *lock(&self.inbox) = Some(inbox.clone());
        // Collected under the weak-map lock and released before handing the
        // sender over, deduped so a shared VM is handed it once.
        for extension in self.attached_vms() {
            extension.deliver_to(inbox.clone());
        }
    }

    fn registrations(&self) -> Vec<contract::events::HookInfo> {
        SessionExtensions::registrations(self)
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

/// Replaces only the repository layer with approved mapped entries.
pub(crate) fn filtered_hooks_config(
    config: &Config,
    has_repository_hooks: bool,
    approved: impl IntoIterator<Item = (String, Value)>,
) -> Config {
    let hooks: Map<String, Value> = approved.into_iter().collect();
    if !has_repository_hooks && hooks.is_empty() {
        return config.clone();
    }
    let mut filtered = config.clone();
    let mut outer = Map::new();
    outer.insert("hooks".to_owned(), Value::Object(hooks));
    filtered
        .extensions_mut()
        .set_overlay(&config::full_name("hooks"), Value::Object(outer));
    filtered
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
    let source = format!("extensions.\"{name}\".settings");
    let Value::Object(entries) = declared else {
        return Err(Error::Config(config::ConfigError::WrongType {
            source_name: source,
            key: "hooks".to_owned(),
            expected: "an object of hooks, one entry per name".to_owned(),
        }));
    };
    // A non-object entry is a value of the wrong type: check every entry
    // before any installed/missing decision, so key order cannot fail the
    // guard open. Several malformed entries report the first in sorted key
    // order (`docs/extensions.md`, "Hooks declared in configuration"):
    // `preserve_order` is never enabled (`docs/dependencies.md`), so `Map`
    // iteration is already sorted and no extra sort is needed.
    for (entry_name, entry) in &entries {
        if !entry.is_object() {
            return Err(Error::Config(config::ConfigError::WrongType {
                source_name: source,
                key: format!("hooks.{entry_name}"),
                expected: "an object".to_owned(),
            }));
        }
    }
    let missing = || {
        let mut names: Vec<String> = entries
            .iter()
            .filter_map(|(name, entry)| {
                let blocking = entry.as_object().is_none_or(|entry| {
                    !entry.contains_key("watch")
                        && entry.get("on_failure").and_then(Value::as_str) != Some("non-blocking")
                });
                blocking.then(|| name.clone())
            })
            .collect();
        names.sort();
        HooksState::MissingBlocking { names }
    };
    if entries.is_empty() {
        return Ok(HooksState::Ready);
    }
    let mut blocking = false;
    let mut hooks = false;
    for entry in entries.values() {
        let Value::Object(entry) = entry else {
            return Ok(missing());
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
        return Ok(missing());
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
    MissingBlocking {
        /// Sorted eligible blocking entry names; empty for a nameless declaration.
        names: Vec<String>,
    },
}

#[cfg(test)]
#[path = "hooks/repository_tests.rs"]
mod repository_tests;
#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
