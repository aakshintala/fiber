//! Starting the configured stdio servers (`docs/mcp.md`, "Starting
//! servers"): a server with a cached tool list for its current declaration
//! is declared from the cache and spawned on the first call to one of its
//! tools, while `required` servers and cache misses start with the session
//! and write the cache. Each session-start server starts in parallel under
//! its own startup deadline, and a server that fails to start or misses its
//! deadline is left out and recorded as `mcp_server_failed`. The session
//! still starts, unless a `required` server failed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::events::{McpServerFailed, ServerFailure, ToolInfo, ToolSource, ToolState};
use contract::images::Images;
use contract::provider::ToolDefinition;
use contract::tool::{ServerRecord, Tool};

use crate::cache::{self, Cached};
use crate::effects::Hints;
use crate::error::McpError;
use crate::prompt::{PromptSource, Prompts};
use crate::server::Server;
use crate::server_json::{ListedPrompt, ListedTool};
use crate::slot::{Death, Slot};
use crate::tool::McpTool;

/// How Fiber reaches a server (`docs/mcp.md`, "What Fiber does with MCP").
#[derive(Clone)]
pub enum Transport {
    /// A child process Fiber starts.
    Stdio(StdioSpec),
    /// A remote server over streamable HTTP.
    Http {
        /// The server's URL.
        url: String,
        /// Credential files injected by the host; tokens are keyed by URL.
        tokens: Arc<dyn contract::mcp_tokens::TokenFiles>,
    },
}

/// Reads one named secret for a stdio server's environment. `main`
/// builds it over `config::read_secret`, so this crate never depends on
/// `config`; the value travels only into the child's environment.
pub type SecretReader = Arc<dyn Fn(&str) -> Result<String, SecretError> + Send + Sync>;

/// Why a secret read failed. The value never prints.
#[derive(Debug, Clone, PartialEq)]
pub enum SecretError {
    /// No credential is stored under the name.
    Missing,
    /// One is stored but cannot be read; the reason, never the value.
    Unreadable(String),
}

/// A stdio server's declaration, and with `run` the pinned copy that runs
/// instead.
#[derive(Clone)]
pub struct StdioSpec {
    /// The program.
    pub command: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Environment overrides, over Fiber's own environment.
    pub env: BTreeMap<String, String>,
    /// Secret references by env key: each names `credentials/<name>`,
    /// read through `reader` when the server starts, never stored here.
    pub secrets: BTreeMap<String, String>,
    /// Reads one named secret. `main` builds it over
    /// `config::read_secret`; a test reads its own fixture.
    pub reader: SecretReader,
    /// The program that runs when it is not the declared one: a
    /// repository server's pinned copy. `command`/`args`/`env` stay the
    /// declaration, the only cache-key input.
    pub run: Option<Program>,
}

impl StdioSpec {
    /// What spawns: the program that runs when one is set, else the
    /// declared command and arguments.
    pub fn program(&self) -> (&str, &[String]) {
        match self.run.as_ref() {
            Some(run) => (&run.command, &run.args),
            None => (&self.command, &self.args),
        }
    }
}

/// One configured server: what [`launch`] starts.
#[derive(Clone)]
pub struct ServerSpec {
    /// The configured name: the middle of every qualified tool name.
    pub name: String,
    /// How Fiber reaches it.
    pub transport: Transport,
    /// The startup deadline.
    pub startup_timeout: Duration,
    /// Each call's timeout.
    pub call_timeout: Duration,
    /// When present, only these server tool names are declared.
    pub enabled: Option<Vec<String>>,
    /// These server tool names are left out.
    pub disabled: Vec<String>,
    /// The person's hint overrides, by server tool name.
    pub hints: BTreeMap<String, Hints>,
    /// Whether failing to start ends the session.
    pub required: bool,
}

// An `env` value or an argument can hold a token, so only names and
// counts print (`docs/code-quality.md`, "Errors").
impl std::fmt::Debug for ServerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("ServerSpec");
        debug.field("name", &self.name);
        match &self.transport {
            Transport::Stdio(stdio) => {
                debug
                    .field("command", &stdio.command)
                    .field("args", &stdio.args.len())
                    .field(
                        "env",
                        &stdio
                            .env
                            .keys()
                            .map(|name| (name, "redacted"))
                            .collect::<BTreeMap<_, _>>(),
                    )
                    .field(
                        "secrets",
                        &stdio
                            .secrets
                            .keys()
                            .map(|name| (name, "redacted"))
                            .collect::<BTreeMap<_, _>>(),
                    )
                    .field("run", &stdio.run);
            }
            Transport::Http { url, .. } => {
                debug.field("url", &redacted_url(url));
            }
        }
        debug
            .field("startup_timeout", &self.startup_timeout)
            .field("call_timeout", &self.call_timeout)
            .field("enabled", &self.enabled)
            .field("disabled", &self.disabled)
            .field("hints", &self.hints)
            .field("required", &self.required)
            .finish()
    }
}

/// The program that runs when it is not the declared one: a repository
/// server's pinned copy, with the guard that refuses an unapproved spawn.
/// `command`/`args` hold what runs; the declaration stays on the spec.
pub struct Program {
    /// The program that runs.
    pub command: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Runs immediately before every spawn of this program: `Err` refuses
    /// that spawn as `StartError::StartFailed`.
    pub guard: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}

impl Clone for Program {
    fn clone(&self) -> Self {
        Self {
            command: self.command.clone(),
            args: self.args.clone(),
            guard: Arc::clone(&self.guard),
        }
    }
}

// The command and every argument can hold a token, so only their counts
// print (`docs/code-quality.md`, "Errors").
impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Program")
            .field("command", &"redacted")
            .field("args", &self.args.len())
            .field("guard", &"<guard>")
            .finish()
    }
}

/// The URL as it prints: with its userinfo and query removed, since a
/// token can ride in either; unparsable as `"<unparsed>"`
/// (`docs/code-quality.md`, "Errors").
pub(crate) fn redacted_url(url: &str) -> String {
    let uri: Result<ureq::http::Uri, _> = url.parse();
    let Ok(uri) = uri else {
        return "<unparsed>".to_owned();
    };
    let (Some(scheme), Some(host)) = (uri.scheme_str(), uri.host()) else {
        return "<unparsed>".to_owned();
    };
    let port = uri
        .port_u16()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    format!("{scheme}://{host}{port}{}", uri.path())
}

impl ServerSpec {
    /// The cache key of the declaration (`docs/mcp.md`, "Starting
    /// servers").
    pub(crate) fn key(&self) -> String {
        match &self.transport {
            Transport::Stdio(stdio) => {
                cache::key(&stdio.command, &stdio.args, &stdio.env, &stdio.secrets)
            }
            Transport::Http { url, .. } => cache::url_key(url),
        }
    }

    /// Whether it is a remote server.
    pub(crate) fn remote(&self) -> bool {
        match &self.transport {
            Transport::Stdio(_) => false,
            Transport::Http { .. } => true,
        }
    }
}

/// The socket limits of a remote server: the idle bound is never shorter
/// than its own deadlines, so the kernel never fires before Fiber's own
/// deadline on a long silent call, while a pump `POST` that never
/// answers is still bounded.
fn limits(spec: &ServerSpec) -> net::Limits {
    net::Limits::new(
        net::LIMITS.connect(),
        spec.call_timeout
            .max(spec.startup_timeout)
            .max(net::LIMITS.idle()),
    )
    .unwrap_or(net::LIMITS)
}

/// Starts one spec on its transport and runs the handshake: the only
/// production caller of [`Server::start_observed`]. Session start
/// ([`open_staged`]) and every lazy first start and restart
/// ([`Slot::serve`]) both go through here, so the guard runs immediately
/// before each spawn. One connection guard starts per attempt through
/// `timing`: owned here and threaded to the tool-list boundary, which
/// consumes it; what remains drops here on the failure path, so every
/// entered attempt completes exactly once. `None` stays untimed. Never
/// wraps a multi-server loop: each server starts its own.
pub(crate) fn launch_observed(
    spec: &ServerSpec,
    workspace: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
    timing: Option<contract::diag::Timing>,
) -> Result<crate::server::OpenServer, crate::server::StartError> {
    // One guard per launch, from starting or dialing until the tool list
    // is in hand. No clock read happens here beyond what the timing
    // callable itself reads.
    let mut connection = timing
        .as_ref()
        .and_then(|timing| timing(contract::diag::Op::McpConnect, None, None));
    match &spec.transport {
        Transport::Stdio(stdio) => {
            if let Some(run) = stdio.run.as_ref()
                && let Err(message) = (run.guard)()
            {
                return Err(crate::server::StartError::StartFailed(message));
            }
            // Each reference is read here, immediately before the spawn,
            // so a secret rotated after the spec was built still reaches
            // the child with its new bytes. The values travel only into
            // the child's environment, never into the spec, an error or
            // the cache key.
            let mut env = stdio.env.clone();
            for (key, name) in &stdio.secrets {
                match (stdio.reader)(name) {
                    Ok(value) => {
                        env.insert(key.clone(), value);
                    }
                    Err(SecretError::Missing) => {
                        return Err(crate::server::StartError::SecretMissing {
                            name: name.clone(),
                        });
                    }
                    Err(SecretError::Unreadable(why)) => {
                        return Err(crate::server::StartError::SecretUnreadable {
                            name: name.clone(),
                            why,
                        });
                    }
                }
            }
            let (command, args) = stdio.program();
            Server::start_observed(
                command,
                args,
                &env,
                workspace,
                clock,
                spec.startup_timeout,
                version,
                &mut connection,
            )
        }
        Transport::Http { url, tokens } => crate::http::start_observed(
            url,
            tokens.clone(),
            ureq::Proxy::try_from_env(),
            limits(spec),
            clock,
            spec.startup_timeout,
            version,
            timing.clone(),
            &mut connection,
        ),
    }
}

/// What [`start`] started: the tools for the loop, their infos, the
/// failures for the log, and the servers for the session's end.
pub struct Started {
    /// `(registered_by, tool)` pairs for the loop, sorted by qualified
    /// name. `registered_by` is the server's name, except `mcp_resources`,
    /// which `builtin` registers when any server offers resources.
    pub tools: Vec<(String, Arc<dyn Tool>)>,
    /// One per tool, built as `main`'s `builtin::registered` builds them
    /// but with [`ToolSource::Mcp`].
    pub infos: Vec<ToolInfo>,
    /// One per server that failed to start or missed its deadline, in spec
    /// order.
    pub failed: Vec<McpServerFailed>,
    /// The servers, started or waiting for their first call, stopped when
    /// the session ends.
    pub servers: Servers,
    /// Every runnable prompt of every server that listed, tagged with its
    /// server's name (`docs/mcp.md`, "Prompts and resources").
    pub prompts: Prompts,
    /// The first `required` server that failed to start or missed its
    /// deadline, in spec order. When set, the caller stops `servers` and
    /// fails the session; `tools` and `infos` are meaningless, and no
    /// `mcp_server_failed` is written: the session log does not exist yet.
    pub required_failed: Option<McpServerFailed>,
}

/// The servers: running ones and ones declared from the cache that start on
/// their first call. The list is shared, so a tool declared from one start
/// reaches the servers another start adopted into it; adopting copies the
/// slot handles, and a slot in two lists stops once.
pub struct Servers {
    pub(crate) slots: Arc<Mutex<Vec<Arc<Slot>>>>,
    resources_declared: AtomicBool,
    /// The timing context attempts start their connection guards from:
    /// `None` starts untimed, as before. Never logged.
    timing: Option<contract::diag::Timing>,
}

impl Servers {
    /// Stops every server at once, each on its own thread: stdin closed,
    /// SIGTERM, the grace, then kill and reap. Never spawns: a slot locked
    /// by an in-flight start is stopped after that start returns.
    pub fn stop(&self) {
        let slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        thread::scope(|scope| {
            for slot in &slots {
                scope.spawn(|| slot.stop());
            }
        });
    }

    /// Takes in servers another start began: the adopted servers stop with
    /// this stop, after the loop has returned, so nothing is adopted
    /// after it. The adopted handles are copied onto this list's end,
    /// leaving the other list intact; neither declared flag is touched.
    pub fn adopt(&self, other: Servers) {
        let more = other
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(more);
    }

    /// Records that this session already declares `mcp_resources`, so a
    /// later load never offers a second registration for it.
    pub fn mark_resources_declared(&self) {
        self.resources_declared.store(true, Ordering::SeqCst);
    }

    /// Aligns the marker with the installed tool set: set when the session
    /// declares `mcp_resources`, cleared when it does not, so a later
    /// in-session repository approval neither offers a second registration
    /// scoped to its own servers nor suppresses the needed one.
    pub fn set_resources_declared(&self, declared: bool) {
        self.resources_declared.store(declared, Ordering::SeqCst);
    }

    /// Whether [`Servers::mark_resources_declared`] has run.
    pub fn resources_declared(&self) -> bool {
        self.resources_declared.load(Ordering::SeqCst)
    }

    /// Finds the slot `name` runs on, if one names it.
    /// The slot `name` runs in, if it is named; `None` when the server
    /// never started.
    pub(crate) fn slot(&self, name: &str) -> Option<Arc<Slot>> {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|slot| slot.spec().name == name)
            .cloned()
    }

    /// Whether the server `name` runs now: it has a slot in `Running`
    /// state. A name nothing started, one that failed to start and one
    /// stopped all read false. The hub's `mcp_servers` answer reads this
    /// live, so a server that died mid-session reads not-running and its
    /// row falls back to the failure the log holds (`docs/mcp.md`,
    /// "Servers, their state and the per-server commands").
    pub fn running(&self, name: &str) -> bool {
        self.slot(name).is_some_and(|slot| slot.is_running())
    }

    /// Whether the server `name` has a slot: false when the session
    /// never started it, so the hub's reconnect adopts a fresh start
    /// instead of restarting in place.
    pub fn has_slot(&self, name: &str) -> bool {
        self.slot(name).is_some()
    }

    /// The timing context this start's attempts run under, for a later
    /// load that starts more servers into it. A clone of the context;
    /// reading it starts no guard.
    pub fn timing(&self) -> Option<contract::diag::Timing> {
        self.timing.clone()
    }

    /// Stops the slot `name` runs on and drops it from the list, so a
    /// re-added declaration finds `has_slot` false and starts fresh, exactly
    /// as the reconnect path does for a slot-less name. A slot with an
    /// in-flight start is stopped after that start returns, bounded by the
    /// startup deadline: the stop takes the same lease the start holds. The
    /// list lock is never held across the stop.
    pub(crate) fn remove(&self, name: &str) {
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            let position = slots.iter().position(|slot| slot.spec().name == name);
            match position {
                Some(index) => slots.remove(index),
                None => return,
            }
        };
        slot.stop();
    }
}

/// What restarting one server changed: the records to write, the tools
/// to declare, and the qualified names to drop.
pub struct Restarted {
    /// The `mcp_server_failed`/`mcp_server_ready` lines the restart
    /// observed, in order.
    pub records: Vec<ServerRecord>,
    /// The server's tools after the restart, with their owners and
    /// infos, for the loop's registry and the `tools` answer.
    pub used: Vec<(String, Arc<dyn Tool>, ToolInfo)>,
    /// Qualified tool names the restart dropped, for both.
    pub removed: Vec<String>,
}

/// Restarts the server `name`: its slot serves again, exactly as a first
/// tool call serves it, so a healthy running server restarts to a no-op
/// and a dead one stays dead. `None` when no slot names `name`. The
/// caller declares `used`, drops `removed`, and writes `records`
/// (`docs/mcp.md`, "Servers, their state and the per-server commands").
pub fn restart(
    servers: &Servers,
    name: &str,
    artifacts: &Path,
    images: &Arc<dyn Images>,
) -> Option<Restarted> {
    let slot = servers.slot(name)?;
    let before: BTreeSet<String> = slot
        .live_names()
        .iter()
        .map(|tool| crate::name::qualified(name, tool))
        .collect();
    let records = match slot.serve() {
        crate::slot::Served::Up(_, records) => records,
        crate::slot::Served::Failed(failed) => failed.records,
    };
    let listed = slot.listed_tools();
    let after: BTreeSet<String> = listed
        .iter()
        .map(|tool| crate::name::qualified(name, &tool.name))
        .collect();
    let removed: Vec<String> = before.difference(&after).cloned().collect();
    let link = Arc::downgrade(&slot);
    let used = declare(slot.spec(), &listed, &link, artifacts, images);
    Some(Restarted {
        records,
        used,
        removed,
    })
}

/// The default startup deadline: 5 seconds (`docs/configuration.md`).
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_millis(5000);

/// The default call timeout: 10 minutes (`docs/mcp.md`, "Calls").
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_millis(600_000);

/// Starts the session-start specs in parallel in `workspace` and declares
/// the tools of the servers that answered, declaring the rest from their
/// cached lists in `cache` (`<home>/cache/mcp`) without spawning them.
/// `version` is Fiber's own version, sent as `clientInfo`. Binary content
/// a tool answers is written to the session artifacts dir `artifacts`, and
/// an image is processed once through `images`.
pub fn start(
    specs: Vec<ServerSpec>,
    workspace: &Path,
    artifacts: &Path,
    images: Arc<dyn Images>,
    cache: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Started {
    start_observed(
        specs, workspace, artifacts, images, cache, clock, version, None,
    )
}

/// As [`start`], timing every session-start attempt's connection through
/// `timing` and keeping the context on the servers for later attempts:
/// cached declarations start nothing and emit nothing. `None` stays
/// untimed, as before.
#[allow(
    clippy::too_many_arguments,
    reason = "the observed companion adds only the Timing"
)]
pub fn start_observed(
    specs: Vec<ServerSpec>,
    workspace: &Path,
    artifacts: &Path,
    images: Arc<dyn Images>,
    cache: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
    timing: Option<contract::diag::Timing>,
) -> Started {
    start_observed_tracked(
        specs, workspace, artifacts, images, cache, clock, version, timing, None,
    )
}

/// As [`start_observed`], reporting every session server still starting
/// in `outstanding`: each server leaves the set when its start reports,
/// so a watcher holding the handle reads exactly the servers that have
/// not reported. `None` tracks nothing, as before.
#[allow(
    clippy::too_many_arguments,
    reason = "the tracked companion adds only the outstanding set"
)]
pub(crate) fn start_observed_tracked(
    specs: Vec<ServerSpec>,
    workspace: &Path,
    artifacts: &Path,
    images: Arc<dyn Images>,
    cache: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
    timing: Option<contract::diag::Timing>,
    outstanding: Option<Arc<Mutex<BTreeSet<String>>>>,
) -> Started {
    let mut lazy = Vec::new();
    let mut session = Vec::new();
    for (index, spec) in specs.into_iter().enumerate() {
        // A `required` server with a cached list still starts with the
        // session and is declared from its live list.
        let hit = if spec.required {
            None
        } else {
            cache::read(cache, &spec.name, &spec.key())
        };
        match hit {
            Some(cached) => lazy.push((index, spec, cached)),
            None => session.push((index, spec)),
        }
    }
    let (done, results) = mpsc::channel();
    if let Some(outstanding) = outstanding.as_ref() {
        for (_, spec) in session.iter() {
            outstanding
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(spec.name.clone());
        }
    }
    for (index, spec) in session.into_iter() {
        let done = done.clone();
        let workspace = workspace.to_path_buf();
        let artifacts = artifacts.to_path_buf();
        let images = Arc::clone(&images);
        let cache = cache.to_path_buf();
        let clock = Arc::clone(clock);
        let version = version.to_owned();
        let timing = timing.clone();
        let name = spec.name.clone();
        let outstanding = outstanding.clone();
        thread::spawn(move || {
            let required = spec.required;
            let key = spec.key();
            let staged = stage_and_write(
                open_staged(
                    spec, &workspace, &artifacts, &images, &cache, &clock, &version, timing,
                ),
                &cache,
                &name,
                &key,
            );
            if let Some(outstanding) = outstanding.as_ref() {
                outstanding
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&name);
            }
            done.send((index, required, staged)).unwrap_or(());
        });
    }
    drop(done);
    let mut staged: Vec<(usize, bool, Shadowed)> = results.into_iter().collect();
    staged.sort_by_key(|(index, _, _)| *index);
    let mut tools: Vec<(String, Arc<dyn Tool>, ToolInfo)> = Vec::new();
    let mut failed = Vec::new();
    let mut required_failed = None;
    let mut slots = Vec::new();
    let mut sources = Vec::new();
    for (_, required, staged) in staged {
        match staged {
            Shadowed::Up(opened) => {
                slots.push(Arc::clone(&opened.slot));
                sources.extend(opened.sources);
                tools.extend(opened.declared);
            }
            Shadowed::Down { failure } => {
                if required {
                    if required_failed.is_none() {
                        required_failed = Some(failure);
                    }
                } else {
                    failed.push(failure);
                }
            }
        }
    }
    // Lazy servers declare from the cache with no spawn: a session that
    // never calls one never spawns it. Their prompts come from the same
    // cached lists, so a `/name` is answered before anything starts.
    for (_, spec, cached) in lazy {
        let slot = Slot::lazy_observed(
            spec.clone(),
            workspace,
            cache,
            clock,
            version,
            cached.clone(),
            timing.clone(),
        );
        let link: Weak<Slot> = Arc::downgrade(&slot);
        slots.push(slot);
        sources.extend(sources_of(
            &spec.name,
            &cached.prompts,
            &link,
            spec.call_timeout,
        ));
        tools.extend(declare(&spec, &cached.tools, &link, artifacts, &images));
    }
    let slots: Arc<Mutex<Vec<Arc<Slot>>>> = Arc::new(Mutex::new(slots));
    // One `mcp_resources` for every start with a server offering
    // resources, reaching the start's own slots, so it follows adoption.
    if let Some(declaration) = resources_declaration(&slots, artifacts, &images) {
        tools.push(declaration);
    }
    tools.sort_by(|left, right| left.1.definition().name.cmp(&right.1.definition().name));
    let (pairs, infos): (Vec<_>, Vec<_>) = tools
        .into_iter()
        .map(|(registered_by, tool, info)| ((registered_by, tool), info))
        .unzip();
    Started {
        tools: pairs,
        infos,
        failed,
        servers: Servers {
            slots,
            resources_declared: AtomicBool::new(false),
            timing,
        },
        prompts: Prompts::collect(sources),
        required_failed,
    }
}

/// One `mcp_resources` row: listed as built in, in full, like
/// `main`'s `builtin::registered` builds its rows.
pub(crate) fn resources_info(definition: &ToolDefinition) -> ToolInfo {
    let bytes =
        u64::try_from(serde_json::to_vec(definition).unwrap_or_default().len()).unwrap_or(u64::MAX);
    ToolInfo {
        name: definition.name.clone(),
        source: ToolSource::Builtin,
        state: ToolState::Full,
        bytes,
        tokens: None,
    }
}

pub(crate) type Declaration = (String, Arc<dyn Tool>, ToolInfo);

/// One `mcp_resources` declaration when any post-sync slot offers
/// resources, as at session start: `None` when none does, so the caller
/// declares nothing.
pub(crate) fn resources_declaration(
    slots: &Arc<Mutex<Vec<Arc<Slot>>>>,
    artifacts: &Path,
    images: &Arc<dyn Images>,
) -> Option<Declaration> {
    let offering = slots
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|slot| slot.offers_resources());
    if !offering {
        return None;
    }
    let definition = crate::resources_definition();
    let tool: Arc<dyn Tool> = Arc::new(crate::resources::Resources::new(slots, artifacts, images));
    Some(("builtin".to_owned(), tool, resources_info(&definition)))
}

/// Starts one spec and stages the outcome, writing the cache only when
/// the start listed: one helper for the session-start thread and the
/// direct-start test path.
fn stage_and_write(staged: Shadowed, cache: &Path, name: &str, key: &str) -> Shadowed {
    if let Shadowed::Up(ref opened) = staged {
        cache::write(cache, name, key, &opened.listed);
    }
    staged
}

/// Session start without timing: direct-start tests only.
#[cfg(test)]
pub(crate) fn open(
    spec: ServerSpec,
    workspace: &Path,
    artifacts: &Path,
    images: &Arc<dyn Images>,
    cache: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Shadowed {
    let name = spec.name.clone();
    let key = spec.key();
    stage_and_write(
        open_staged(
            spec, workspace, artifacts, images, cache, clock, version, None,
        ),
        cache,
        &name,
        &key,
    )
}

/// What a shadow start holds: the running slot, its declarations, the
/// raw lists for the commit's cache write, and the prompt sources the
/// session would list. The cache file is unwritten: the commit writes it
/// only after every required start succeeds, so a required failure leaves
/// the old caches byte-identical.
pub(crate) struct StagedOpen {
    pub(crate) slot: Arc<Slot>,
    pub(crate) declared: Vec<Declaration>,
    pub(crate) listed: Cached,
    pub(crate) sources: Vec<PromptSource>,
}

/// What a shadow start stages: the running slot with its declarations
/// and cache payload, or the start failure the caller files.
pub(crate) enum Shadowed {
    Up(StagedOpen),
    Down { failure: McpServerFailed },
}

/// Starts one spec and stages the outcome without writing the cache:
/// the caller writes it only after its required starts succeed, so a
/// required failure leaves the old caches byte-identical. The guard is
/// consumed at the tool-list boundary, before prompt discovery. `None`
/// timing stays untimed, as before.
#[allow(
    clippy::too_many_arguments,
    reason = "one shadow's startup inputs, as at session start"
)]
pub(crate) fn open_staged(
    spec: ServerSpec,
    workspace: &Path,
    artifacts: &Path,
    images: &Arc<dyn Images>,
    cache: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
    timing: Option<contract::diag::Timing>,
) -> Shadowed {
    let name = spec.name.clone();
    let required = spec.required;
    let timeout = spec.startup_timeout;
    let remote = spec.remote();
    let open = match launch_observed(&spec, workspace, clock, version, timing.clone()) {
        Ok(open) => open,
        Err(error) => {
            return Shadowed::Down {
                failure: not_started(&name, &error, timeout, required, remote).record,
            };
        }
    };
    let live = open.listed;
    let listed_prompts = live.prompts.clone();
    let tools = live.tools.clone();
    let slot = Slot::running_observed(
        spec.clone(),
        workspace,
        cache,
        clock,
        version,
        open.server,
        live.clone(),
        open.advertises_tools,
        open.advertises_prompts,
        timing,
    );
    let link: Weak<Slot> = Arc::downgrade(&slot);
    let sources = sources_of(&name, &listed_prompts, &link, spec.call_timeout);
    let declared = declare(&spec, &tools, &link, artifacts, images);
    Shadowed::Up(StagedOpen {
        slot,
        declared,
        listed: live,
        sources,
    })
}

/// One prompt row source per listed prompt of `server`.
fn sources_of(
    server: &str,
    prompts: &[ListedPrompt],
    slot: &Weak<Slot>,
    timeout: Duration,
) -> Vec<PromptSource> {
    prompts
        .iter()
        .map(|prompt| PromptSource {
            server: server.to_owned(),
            prompt: prompt.clone(),
            slot: slot.clone(),
            timeout,
        })
        .collect()
}

/// `enabled` names only these, `disabled` removes those, both is enabled
/// minus disabled: all by the server's own names, not the qualified ones.
pub(crate) fn kept(spec: &ServerSpec, tool: &str) -> bool {
    if let Some(enabled) = spec.enabled.as_ref()
        && !enabled.iter().any(|name| name == tool)
    {
        return false;
    }
    !spec.disabled.iter().any(|name| name == tool)
}

pub(crate) fn info(server: &str, tool: &McpTool) -> ToolInfo {
    let definition = tool.definition();
    let bytes = u64::try_from(serde_json::to_vec(&definition).unwrap_or_default().len())
        .unwrap_or(u64::MAX);
    ToolInfo {
        name: definition.name,
        source: ToolSource::Mcp {
            server: server.to_owned(),
            tool: tool.tool_name().to_owned(),
        },
        state: ToolState::Full,
        bytes,
        tokens: None,
    }
}

/// Declares `tools` (a live list or a cached one) through the tool seam:
/// `enabled`/`disabled` filtering and the person's hint overrides apply to
/// either, so changing those keys needs no cache miss.
pub(crate) fn declare(
    spec: &ServerSpec,
    tools: &[ListedTool],
    link: &Weak<Slot>,
    artifacts: &Path,
    images: &Arc<dyn Images>,
) -> Vec<Declaration> {
    let mut declared = Vec::new();
    for tool in tools {
        if !kept(spec, &tool.name) {
            continue;
        }
        let hints = spec
            .hints
            .get(&tool.name)
            .cloned()
            .unwrap_or_else(|| tool.hints());
        let made = McpTool::declare(
            &spec.name,
            &tool.name,
            tool.description.clone(),
            tool.schema.clone(),
            &hints,
            spec.remote(),
            crate::tool::Target {
                timeout: spec.call_timeout,
                artifacts: artifacts.to_path_buf(),
                images: Arc::clone(images),
                slot: link.clone(),
            },
        );
        let registered = spec.name.clone();
        let details = info(&spec.name, &made);
        let made: Arc<dyn Tool> = Arc::new(made);
        declared.push((registered, made, details));
    }
    declared
}

/// The death of a server that did not start: `required` servers name
/// themselves, because the session stops before the log, so a required
/// death is never written as `mcp_server_failed` but returned for the
/// exit-before-session path instead.
pub(crate) fn not_started(
    server: &str,
    error: &crate::server::StartError,
    startup: Duration,
    required: bool,
    remote: bool,
) -> Death {
    let (reason, error) = fail_parts(server, error, startup, required, remote);
    Death {
        record: McpServerFailed {
            server: server.to_owned(),
            reason,
            will_restart: false,
            error: contract::shapes::Failure::from(&error),
        },
        error,
    }
}

fn fail_parts(
    server: &str,
    error: &crate::server::StartError,
    startup: Duration,
    required: bool,
    remote: bool,
) -> (ServerFailure, McpError) {
    match error {
        crate::server::StartError::StartFailed(detail) => (
            ServerFailure::StartFailed,
            McpError::StartFailed {
                server: server.to_owned(),
                required,
                remote,
                detail: detail.clone(),
            },
        ),
        crate::server::StartError::Login(detail) => (
            ServerFailure::NotLoggedIn,
            McpError::NotLoggedIn {
                server: server.to_owned(),
                required,
                detail: detail.clone(),
            },
        ),
        crate::server::StartError::Deadline => (
            ServerFailure::Deadline,
            McpError::StartDeadline {
                server: server.to_owned(),
                required,
                startup_ms: startup.as_millis(),
            },
        ),
        crate::server::StartError::SecretMissing { name } => (
            ServerFailure::StartFailed,
            McpError::SecretMissing {
                server: server.to_owned(),
                required,
                name: name.clone(),
            },
        ),
        crate::server::StartError::SecretUnreadable { name, why } => (
            ServerFailure::StartFailed,
            McpError::SecretUnreadable {
                server: server.to_owned(),
                required,
                name: name.clone(),
                why: why.clone(),
            },
        ),
    }
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
