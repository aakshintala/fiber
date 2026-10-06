//! One server's slot (`docs/mcp.md`, "Starting servers"): not started and
//! declared from the cache, running, or dead for the session. The first
//! call to a server that is not started starts it under the slot's lock, so
//! concurrent first calls share one start and then see the same running
//! server or the same dead slot. A failed start leaves the slot dead, and
//! only the call that triggered it carries the `mcp_server_failed` record.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::McpServerFailed;
use contract::shapes::Failure;
use serde_json::Value;

use crate::cache;
use crate::server::{ListedTool, Server};
use crate::start::{ServerSpec, failed};

/// One server's place in the session: what [`Slot::run`] starts, calls or
/// refuses, and what [`Slot::stop`] stops.
pub(crate) struct Slot {
    spec: ServerSpec,
    workspace: PathBuf,
    cache: PathBuf,
    clock: Arc<dyn Clock>,
    version: String,
    state: Mutex<State>,
}

/// What the slot holds: the cached list until the first call starts the
/// server, the running server with the names it listed, or the failure
/// every later call repeats without spawning.
enum State {
    /// Declared from the cache; no process runs.
    NotStarted {
        /// The cached raw entries, for the rewrite check on start.
        cached: Vec<Value>,
    },
    /// The server runs; `live` is every name it listed.
    Running {
        server: Arc<Server>,
        live: HashSet<String>,
    },
    /// A failed start, or a stop before any start: no spawn again.
    Dead { error: Failure },
}

/// What [`Slot::run`] found.
pub(crate) enum Run {
    /// The server runs and still has the tool: call it.
    Call(Arc<Server>),
    /// The running server no longer lists the tool: fail `mcp_tool_removed`.
    Removed,
    /// The server is dead or its start failed: fail `error`, carrying the
    /// `mcp_server_failed` record only for the call that triggered the
    /// failed start.
    Failed(Box<RunFailed>),
}

/// A failed [`Run`]: the call's error and, only for the call that
/// triggered the failed start, its `mcp_server_failed` record.
pub(crate) struct RunFailed {
    /// The call's error.
    pub error: Failure,
    /// The record, set only for the triggering call.
    pub record: Option<McpServerFailed>,
}

impl Slot {
    /// One slot from its parts, in `state`.
    fn new(
        spec: ServerSpec,
        workspace: &Path,
        cache: &Path,
        clock: &Arc<dyn Clock>,
        version: &str,
        state: State,
    ) -> Arc<Self> {
        Arc::new(Self {
            spec,
            workspace: workspace.to_path_buf(),
            cache: cache.to_path_buf(),
            clock: Arc::clone(clock),
            version: version.to_owned(),
            state: Mutex::new(state),
        })
    }

    /// A slot declared from `cached`, starting nothing until the first
    /// call to one of its tools.
    pub(crate) fn lazy(
        spec: ServerSpec,
        workspace: &Path,
        cache: &Path,
        clock: &Arc<dyn Clock>,
        version: &str,
        cached: Vec<Value>,
    ) -> Arc<Self> {
        Self::new(spec, workspace, cache, clock, version, State::NotStarted { cached })
    }

    /// A slot for a server [`Server::start`] already runs, holding its full
    /// listed tools for the removed-tool check.
    pub(crate) fn running(
        spec: ServerSpec,
        workspace: &Path,
        cache: &Path,
        clock: &Arc<dyn Clock>,
        version: &str,
        server: Server,
        tools: Vec<ListedTool>,
    ) -> Arc<Self> {
        let live = tools.iter().map(|tool| tool.name.clone()).collect();
        Self::new(
            spec,
            workspace,
            cache,
            clock,
            version,
            State::Running {
                server: Arc::new(server),
                live,
            },
        )
    }

    /// The server to call `tool` on, starting it first when the slot is not
    /// started. The lock is held across [`Server::start`], so the second of
    /// two concurrent first calls waits for the first's start (bounded by
    /// the startup deadline) and then sees the same outcome.
    pub(crate) fn run(&self, tool: &str) -> Run {
        let mut state = lock(&self.state);
        let cached = match &*state {
            State::Running { server, live } => {
                return if live.contains(tool) {
                    Run::Call(Arc::clone(server))
                } else {
                    Run::Removed
                };
            }
            State::Dead { error } => {
                return Run::Failed(Box::new(RunFailed {
                    error: error.clone(),
                    record: None,
                }));
            }
            State::NotStarted { cached } => cached.clone(),
        };
        let timeout = self.spec.startup_timeout;
        let name = self.spec.name.clone();
        match Server::start(
            &self.spec.command,
            &self.spec.args,
            &self.spec.env,
            &self.workspace,
            &self.clock,
            timeout,
            &self.version,
        ) {
            Ok(open) => {
                // The session keeps the tools it declared, because a tool
                // set that changes mid-session misses the whole prompt
                // cache; the cache is updated for the next session.
                if open.tools != cached {
                    cache::write(
                        &self.cache,
                        &name,
                        &cache::key(&self.spec.command, &self.spec.args, &self.spec.env),
                        &open.tools,
                    );
                }
                let live: HashSet<String> = open
                    .tools
                    .iter()
                    .map(|entry| ListedTool::read(entry).name)
                    .collect();
                let present = live.contains(tool);
                let server = Arc::new(open.server);
                *state = State::Running {
                    server: Arc::clone(&server),
                    live,
                };
                if present {
                    Run::Call(server)
                } else {
                    Run::Removed
                }
            }
            Err(error) => {
                let record = failed(&name, &error, timeout, false);
                let failure = record.error.clone();
                *state = State::Dead {
                    error: failure.clone(),
                };
                Run::Failed(Box::new(RunFailed {
                    error: failure,
                    record: Some(record),
                }))
            }
        }
    }

    /// Stops the running server, if any. A server that never started is
    /// marked dead instead, so nothing spawns after the stop; a slot an
    /// in-flight start holds is stopped after that start returns, bounded
    /// by the startup deadline.
    pub(crate) fn stop(&self) {
        let mut state = lock(&self.state);
        if let State::Running { server, .. } = &*state {
            let server = Arc::clone(server);
            drop(state);
            server.stop();
            return;
        }
        if matches!(&*state, State::Dead { .. }) {
            return;
        }
        *state = State::Dead {
            error: Failure {
                code: ErrorCode::McpServerUnavailable,
                message: unavailable(&self.spec.name),
                retry_after: None,
                provider: None,
            },
        };
    }
}

/// The message of a server that is gone: no spawn was tried, or its start
/// failed, or it has since exited.
pub(crate) fn unavailable(server: &str) -> String {
    format!("The MCP server `{server}` did not start, or it has since exited.")
}

/// The `mcp_tool_removed` message: the call never reaches the server, and
/// the tool stays declared until the next session.
pub(crate) fn removed(server: &str, tool: &str) -> String {
    format!(
        "The MCP server `{server}` no longer has the tool `{tool}`; it stays declared until the next session.",
    )
}

fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "slot_tests.rs"]
mod tests;
