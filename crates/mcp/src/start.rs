//! Starting every configured stdio server with the session
//! (`docs/mcp.md`, "Starting servers"): each starts in parallel under its
//! own startup deadline, its tools declared through the tool seam, and a
//! server that fails to start or misses its deadline left out and recorded
//! as `mcp_server_failed`. The session still starts.

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

use crate::effects::Hints;
use crate::server::{Server, StartError};
use crate::tool::McpTool;

/// One configured stdio server: what [`start`] spawns.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// What [`start`] started: the tools for the loop, their infos, the
/// failures for the log, and the running servers for the session's end.
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
    /// The running servers, stopped when the session ends.
    pub servers: Servers,
}

/// The running servers.
pub struct Servers {
    servers: Vec<Arc<Server>>,
}

impl Servers {
    /// Stops every server at once, each on its own thread: stdin closed,
    /// SIGTERM, the grace, then kill and reap.
    pub fn stop(&self) {
        thread::scope(|scope| {
            for server in &self.servers {
                scope.spawn(|| server.stop());
            }
        });
    }
}

/// The default startup deadline: 5 seconds (`docs/configuration.md`).
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_millis(5000);

/// The default call timeout: 10 minutes (`docs/mcp.md`, "Calls").
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_millis(600_000);

/// Starts every spec in parallel in `workspace` and declares the tools of
/// the servers that answered. `version` is Fiber's own version, sent as
/// `clientInfo`.
pub fn start(
    specs: Vec<ServerSpec>,
    workspace: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Started {
    let (done, results) = mpsc::channel();
    for (index, spec) in specs.into_iter().enumerate() {
        let done = done.clone();
        let workspace = workspace.to_path_buf();
        let clock = Arc::clone(clock);
        let version = version.to_owned();
        thread::spawn(move || {
            done.send((index, open(spec, &workspace, &clock, &version)))
                .unwrap_or(());
        });
    }
    drop(done);
    let mut opened: Vec<(usize, Opened)> = results.into_iter().collect();
    opened.sort_by_key(|(index, _)| *index);
    let mut tools: Vec<(String, Arc<dyn Tool>, ToolInfo)> = Vec::new();
    let mut failed = Vec::new();
    let mut servers = Vec::new();
    for (_, opened) in opened {
        match opened {
            Opened::Up(server, declared) => {
                servers.push(Arc::clone(&server));
                tools.extend(declared.into_iter().map(|tool| {
                    let info = info(&tool);
                    let registered_by = tool.registered_by.clone();
                    let tool: Arc<dyn Tool> = Arc::new(tool.tool);
                    (registered_by, tool, info)
                }));
            }
            Opened::Down(failure) => failed.push(failure),
        }
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
        servers: Servers { servers },
    }
}

pub(crate) struct Declared {
    tool: McpTool,
    registered_by: String,
}

pub(crate) enum Opened {
    Up(Arc<Server>, Vec<Declared>),
    Down(McpServerFailed),
}

pub(crate) fn open(
    spec: ServerSpec,
    workspace: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Opened {
    let name = spec.name.clone();
    let open = match Server::start(
        &spec.command,
        &spec.args,
        &spec.env,
        workspace,
        clock,
        spec.startup_timeout,
        version,
    ) {
        Ok(open) => open,
        Err(error) => return Opened::Down(failed(&name, error)),
    };
    let server = Arc::new(open.server);
    let mut declared = Vec::new();
    for tool in open.tools {
        if !kept(&spec, &tool.name) {
            continue;
        }
        let hints = spec.hints.get(&tool.name).unwrap_or(&tool.hints);
        let link: Weak<Server> = Arc::downgrade(&server);
        declared.push(Declared {
            registered_by: name.clone(),
            tool: McpTool::declare(
                &name,
                &tool.name,
                tool.description,
                tool.schema,
                hints,
                spec.call_timeout,
                link,
            ),
        });
    }
    Opened::Up(server, declared)
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
        },
        state: ToolState::Full,
        bytes,
        tokens: None,
    }
}

pub(crate) fn failed(server: &str, error: StartError) -> McpServerFailed {
    let (reason, message) = match error {
        StartError::StartFailed(detail) => (
            ServerFailure::StartFailed,
            format!("The MCP server `{server}` failed to start: {detail}"),
        ),
        StartError::Deadline => (
            ServerFailure::Deadline,
            format!("The MCP server `{server}` did not answer before its startup deadline."),
        ),
    };
    McpServerFailed {
        server: server.to_owned(),
        reason,
        will_restart: false,
        error: Failure {
            code: ErrorCode::McpServerUnavailable,
            message,
            retry_after: None,
            provider: None,
        },
    }
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
