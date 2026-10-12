//! Starting the configured stdio servers (`docs/mcp.md`, "Starting
//! servers"): a server with a cached tool list for its current declaration
//! is declared from the cache and spawned on the first call to one of its
//! tools, while `required` servers and cache misses start with the session
//! and write the cache. Each session-start server starts in parallel under
//! its own startup deadline, and a server that fails to start or misses its
//! deadline is left out and recorded as `mcp_server_failed`. The session
//! still starts, unless a `required` server failed.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak, mpsc};
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{McpServerFailed, ServerFailure, ToolInfo, ToolSource, ToolState};
use contract::images::Images;
use contract::provider::ToolDefinition;
use contract::tool::Tool;

use crate::cache;
use crate::effects::Hints;
use crate::prompt::{PromptSource, Prompts};
use crate::server::Server;
use crate::server_json::{ListedPrompt, ListedTool};
use crate::slot::Slot;
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
            Transport::Stdio(stdio) => cache::key(&stdio.command, &stdio.args, &stdio.env),
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
/// production caller of [`Server::start`]. Session start ([`open`]) and
/// every lazy first start and restart ([`Slot::serve`]) both go through
/// here, so the guard runs immediately before each spawn.
pub(crate) fn launch(
    spec: &ServerSpec,
    workspace: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Result<crate::server::OpenServer, crate::server::StartError> {
    match &spec.transport {
        Transport::Stdio(stdio) => {
            if let Some(run) = stdio.run.as_ref()
                && let Err(message) = (run.guard)()
            {
                return Err(crate::server::StartError::StartFailed(message));
            }
            let (command, args) = stdio.program();
            Server::start(
                command,
                args,
                &stdio.env,
                workspace,
                clock,
                spec.startup_timeout,
                version,
            )
        }
        Transport::Http { url, tokens } => crate::http::start(
            url,
            tokens.clone(),
            ureq::Proxy::try_from_env(),
            limits(spec),
            clock,
            spec.startup_timeout,
            version,
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

    /// Whether [`Servers::mark_resources_declared`] has run.
    pub fn resources_declared(&self) -> bool {
        self.resources_declared.load(Ordering::SeqCst)
    }
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
    for (index, spec) in session.into_iter() {
        let done = done.clone();
        let workspace = workspace.to_path_buf();
        let artifacts = artifacts.to_path_buf();
        let images = Arc::clone(&images);
        let cache = cache.to_path_buf();
        let clock = Arc::clone(clock);
        let version = version.to_owned();
        thread::spawn(move || {
            done.send((
                index,
                open(
                    spec, &workspace, &artifacts, &images, &cache, &clock, &version,
                ),
            ))
            .unwrap_or(());
        });
    }
    drop(done);
    let mut opened: Vec<(usize, Opened)> = results.into_iter().collect();
    opened.sort_by_key(|(index, _)| *index);
    let mut tools: Vec<(String, Arc<dyn Tool>, ToolInfo)> = Vec::new();
    let mut failed = Vec::new();
    let mut required_failed = None;
    let mut slots = Vec::new();
    let mut sources = Vec::new();
    for (_, opened) in opened {
        match opened {
            Opened::Up(slot, declared, listed) => {
                slots.push(Arc::clone(&slot));
                sources.extend(listed);
                tools.extend(declared);
            }
            Opened::Down { failure, required } => {
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
        let slot = Slot::lazy(
            spec.clone(),
            workspace,
            cache,
            clock,
            version,
            cached.clone(),
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
    if slots
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|slot| slot.offers_resources())
    {
        let definition = crate::resources_definition();
        let tool: Arc<dyn Tool> =
            Arc::new(crate::resources::Resources::new(&slots, artifacts, &images));
        tools.push(("builtin".to_owned(), tool, resources_info(&definition)));
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
        },
        prompts: Prompts::collect(sources),
        required_failed,
    }
}

/// One `mcp_resources` row: listed as built in, in full, like
/// `main`'s `builtin::registered` builds its rows.
fn resources_info(definition: &ToolDefinition) -> ToolInfo {
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

pub(crate) enum Opened {
    Up(Arc<Slot>, Vec<Declaration>, Vec<PromptSource>),
    Down {
        failure: McpServerFailed,
        required: bool,
    },
}

pub(crate) fn open(
    spec: ServerSpec,
    workspace: &Path,
    artifacts: &Path,
    images: &Arc<dyn Images>,
    cache: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Opened {
    let name = spec.name.clone();
    let required = spec.required;
    let timeout = spec.startup_timeout;
    let remote = spec.remote();
    let open = match launch(&spec, workspace, clock, version) {
        Ok(open) => open,
        Err(error) => {
            return if required {
                Opened::Down {
                    failure: not_started(&name, &error, timeout, true, remote),
                    required: true,
                }
            } else {
                Opened::Down {
                    failure: not_started(&name, &error, timeout, false, remote),
                    required: false,
                }
            };
        }
    };
    let live = open.listed;
    cache::write(cache, &name, &spec.key(), &live);
    let listed_prompts = live.prompts.clone();
    let tools = live.tools.clone();
    let slot = Slot::running(
        spec.clone(),
        workspace,
        cache,
        clock,
        version,
        open.server,
        live,
    );
    let link: Weak<Slot> = Arc::downgrade(&slot);
    let listed = sources_of(&name, &listed_prompts, &link, spec.call_timeout);
    let declared = declare(&spec, &tools, &link, artifacts, images);
    Opened::Up(slot, declared, listed)
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

/// The failure of a server that did not start: `required` servers name
/// themselves, because the session stops before the log, so a required
/// failure is never written as `mcp_server_failed` but returned for the
/// exit-before-session path instead.
pub(crate) fn not_started(
    server: &str,
    error: &crate::server::StartError,
    startup: Duration,
    required: bool,
    remote: bool,
) -> McpServerFailed {
    let (reason, message) = fail_parts(server, error, startup, required, remote);
    McpServerFailed {
        server: server.to_owned(),
        reason,
        will_restart: false,
        error: crate::fail::failure(ErrorCode::McpServerUnavailable, message),
    }
}

fn fail_parts(
    server: &str,
    error: &crate::server::StartError,
    startup: Duration,
    required: bool,
    remote: bool,
) -> (ServerFailure, String) {
    let subject = if required {
        format!("The required MCP server `{server}`")
    } else {
        format!("The MCP server `{server}`")
    };
    // A remote server has no `command` to check: its advice names its `url`.
    let advice = if remote {
        "Check its `url` under `mcp.servers` in your configuration."
    } else {
        "Check its `command` and `args` under `mcp.servers` in your configuration."
    };
    match error {
        crate::server::StartError::StartFailed(detail) => (
            ServerFailure::StartFailed,
            format!("{subject} failed to start: {detail} {advice}",),
        ),
        crate::server::StartError::Login(detail) => (
            ServerFailure::NotLoggedIn,
            format!(
                "{subject} failed to start: {}",
                crate::slot::login_sentence(server, detail)
            ),
        ),
        crate::server::StartError::Deadline => (
            ServerFailure::Deadline,
            format!(
                "{subject} did not answer before its startup deadline of {} ms. Raise `startup_timeout_ms` under `mcp.servers.{server}` if it needs longer.",
                startup.as_millis(),
            ),
        ),
    }
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
