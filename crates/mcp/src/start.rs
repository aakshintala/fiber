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
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{McpServerFailed, ServerFailure, ToolInfo, ToolSource, ToolState};
use contract::shapes::Failure;
use contract::tool::Tool;

use crate::cache;
use crate::effects::Hints;
use crate::prompt::{PromptSource, Prompts};
use crate::server::Server;
use crate::server_json::{ListedPrompt, ListedTool};
use crate::slot::Slot;
use crate::tool::McpTool;

/// One configured stdio server: what [`start`] spawns.
#[derive(Clone, PartialEq, Eq)]
pub struct ServerSpec {
    /// The configured name: the middle of every qualified tool name.
    pub name: String,
    /// The program.
    pub command: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Environment overrides, over Fiber's own environment.
    pub env: BTreeMap<String, String>,
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

// An `env` value can hold a token, so only names print
// (`docs/code-quality.md`, "Errors").
impl std::fmt::Debug for ServerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerSpec")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("args", &self.args)
            .field(
                "env",
                &self
                    .env
                    .keys()
                    .map(|name| (name, "redacted"))
                    .collect::<BTreeMap<_, _>>(),
            )
            .field("startup_timeout", &self.startup_timeout)
            .field("call_timeout", &self.call_timeout)
            .field("enabled", &self.enabled)
            .field("disabled", &self.disabled)
            .field("hints", &self.hints)
            .field("required", &self.required)
            .finish()
    }
}

/// What [`start`] started: the tools for the loop, their infos, the
/// failures for the log, and the servers for the session's end.
pub struct Started {
    /// `(registered_by, tool)` pairs for the loop, sorted by qualified
    /// name. `registered_by` is the server's name.
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
/// their first call.
pub struct Servers {
    pub(crate) slots: Vec<Arc<Slot>>,
}

impl Servers {
    /// Stops every server at once, each on its own thread: stdin closed,
    /// SIGTERM, the grace, then kill and reap. Never spawns: a slot locked
    /// by an in-flight start is stopped after that start returns.
    pub fn stop(&self) {
        thread::scope(|scope| {
            for slot in &self.slots {
                scope.spawn(|| slot.stop());
            }
        });
    }
}

/// The default startup deadline: 5 seconds (`docs/configuration.md`).
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_millis(5000);

/// The default call timeout: 10 minutes (`docs/mcp.md`, "Calls").
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_millis(600_000);

/// Starts the session-start specs in parallel in `workspace` and declares
/// the tools of the servers that answered, declaring the rest from their
/// cached lists in `cache` (`<home>/cache/mcp`) without spawning them.
/// `version` is Fiber's own version, sent as `clientInfo`.
pub fn start(
    specs: Vec<ServerSpec>,
    workspace: &Path,
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
            cache::read(
                cache,
                &spec.name,
                &cache::key(&spec.command, &spec.args, &spec.env),
            )
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
        let cache = cache.to_path_buf();
        let clock = Arc::clone(clock);
        let version = version.to_owned();
        thread::spawn(move || {
            done.send((index, open(spec, &workspace, &cache, &clock, &version)))
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
                tools.extend(declared.into_iter().map(|tool| {
                    let info = info(&tool);
                    let registered_by = tool.registered_by.clone();
                    let tool: Arc<dyn Tool> = Arc::new(tool.tool);
                    (registered_by, tool, info)
                }));
            }
            Opened::Down(failure) => failed.push(failure),
            Opened::RequiredDown(failure) => {
                if required_failed.is_none() {
                    required_failed = Some(failure);
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
        tools.extend(declare(&spec, &cached.tools, &link).into_iter().map(|tool| {
            let info = info(&tool);
            let registered_by = tool.registered_by.clone();
            let tool: Arc<dyn Tool> = Arc::new(tool.tool);
            (registered_by, tool, info)
        }));
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
        servers: Servers { slots },
        prompts: Prompts::collect(sources),
        required_failed,
    }
}

pub(crate) struct Declared {
    tool: McpTool,
    registered_by: String,
}

pub(crate) enum Opened {
    Up(Arc<Slot>, Vec<Declared>, Vec<PromptSource>),
    Down(McpServerFailed),
    RequiredDown(McpServerFailed),
}

pub(crate) fn open(
    spec: ServerSpec,
    workspace: &Path,
    cache: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Opened {
    let name = spec.name.clone();
    let required = spec.required;
    let timeout = spec.startup_timeout;
    let open = match Server::start(
        &spec.command,
        &spec.args,
        &spec.env,
        workspace,
        clock,
        timeout,
        version,
    ) {
        Ok(open) => open,
        Err(error) => {
            return if required {
                Opened::RequiredDown(failed(&name, &error, timeout, true))
            } else {
                Opened::Down(failed(&name, &error, timeout, false))
            };
        }
    };
    let live = open.listed;
    cache::write(
        cache,
        &name,
        &cache::key(&spec.command, &spec.args, &spec.env),
        &live,
    );
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
    let declared = declare(&spec, &tools, &link);
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

pub(crate) fn info(declared: &Declared) -> ToolInfo {
    let definition = declared.tool.definition();
    let bytes = u64::try_from(serde_json::to_vec(&definition).unwrap_or_default().len())
        .unwrap_or(u64::MAX);
    ToolInfo {
        name: definition.name,
        source: ToolSource::Mcp {
            server: declared.registered_by.clone(),
            tool: declared.tool.tool_name().to_owned(),
        },
        state: ToolState::Full,
        bytes,
        tokens: None,
    }
}

/// Declares `tools` (a live list or a cached one) through the tool seam:
/// `enabled`/`disabled` filtering and the person's hint overrides apply to
/// either, so changing those keys needs no cache miss.
pub(crate) fn declare(spec: &ServerSpec, tools: &[ListedTool], link: &Weak<Slot>) -> Vec<Declared> {
    let mut declared = Vec::new();
    for tool in tools {
        if !kept(spec, &tool.name) {
            continue;
        }
        let hints = spec.hints.get(&tool.name).cloned().unwrap_or_else(|| tool.hints());
        let hints = &hints;
        declared.push(Declared {
            registered_by: spec.name.clone(),
            tool: McpTool::declare(
                &spec.name,
                &tool.name,
                tool.description.clone(),
                tool.schema.clone(),
                hints,
                spec.call_timeout,
                link.clone(),
            ),
        });
    }
    declared
}

/// The failure of a server that did not start: `required` servers name
/// themselves, because the session stops before the log, so a required
/// failure is never written as `mcp_server_failed` but returned for the
/// exit-before-session path instead.
pub(crate) fn failed(
    server: &str,
    error: &crate::server::StartError,
    startup: Duration,
    required: bool,
) -> McpServerFailed {
    let (reason, message) = fail_parts(server, error, startup, required);
    McpServerFailed {
        server: server.to_owned(),
        reason,
        will_restart: false,
        error: Failure {
            code: ErrorCode::McpServerUnavailable,
            message,
            retry_after_ms: None,
            provider: None,
        },
    }
}

fn fail_parts(
    server: &str,
    error: &crate::server::StartError,
    startup: Duration,
    required: bool,
) -> (ServerFailure, String) {
    let subject = if required {
        format!("The required MCP server `{server}`")
    } else {
        format!("The MCP server `{server}`")
    };
    match error {
        crate::server::StartError::StartFailed(detail) => (
            ServerFailure::StartFailed,
            format!(
                "{subject} failed to start: {detail} Check its `command` and `args` under `mcp.servers` in your configuration.",
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
