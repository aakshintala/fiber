//! One server's slot (`docs/mcp.md`, "Starting servers" and "When a server
//! dies"): not started and declared from the cache, running, down after
//! one death with its restart unused, or dead for the session. The first
//! call to a server that is not started starts it under the slot's lock, so
//! concurrent first calls share one start and then see the same running
//! server or the same failure. A failed first start, or a death, is the
//! server's one death: the next call restarts it, once. A second death
//! leaves it dead, its tools still declared. Each death and each restart is
//! recorded once, by the call that observed it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{McpServerFailed, McpServerReady, ServerFailure};
use contract::shapes::Failure;
use contract::tool::ServerRecord;
use serde_json::Value;

use crate::cache;
use crate::server::{ListedTool, Server};
use crate::start::{ServerSpec, failed};

/// One server's place in the session: what [`Slot::run`] starts, calls,
/// restarts or refuses, and what [`Slot::stop`] stops.
pub(crate) struct Slot {
    spec: ServerSpec,
    workspace: PathBuf,
    cache: PathBuf,
    clock: Arc<dyn Clock>,
    version: String,
    state: Mutex<State>,
}

/// What the slot holds. `listed` is always the last raw list this slot
/// saw, for the cache rewrite check on the next start.
enum State {
    /// Declared from the cache; no process runs.
    NotStarted {
        /// The cached raw entries.
        cached: Vec<Value>,
    },
    /// The server runs; `live` is every name it listed.
    Running {
        server: Arc<Server>,
        live: HashSet<String>,
        listed: Vec<Value>,
        /// Whether this is the restart, so the next death is final.
        restarted: bool,
    },
    /// One death seen and its restart unused: the next call restarts.
    Down { listed: Vec<Value> },
    /// A second death, or a stop: every call fails `error`, no spawn again.
    Dead { error: Failure },
}

/// What [`Slot::run`] found, with the server lines the call carries.
pub(crate) enum Run {
    /// The server runs and still has the tool: call it.
    Call(Arc<Server>, Vec<ServerRecord>),
    /// The running server no longer lists the tool: fail `mcp_tool_removed`.
    Removed(Vec<ServerRecord>),
    /// The server is dead or its start failed: fail `error`.
    Failed(Box<RunFailed>),
}

/// A failed [`Run`]: the call's error and the server lines it carries.
pub(crate) struct RunFailed {
    /// The call's error.
    pub error: Failure,
    /// The lines this call observed; empty for a call that only found the
    /// server dead.
    pub records: Vec<ServerRecord>,
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
        Self::new(
            spec,
            workspace,
            cache,
            clock,
            version,
            State::NotStarted { cached },
        )
    }

    /// A slot for a server [`Server::start`] already runs, holding its raw
    /// `listed` entries for the removed-tool and cache checks.
    pub(crate) fn running(
        spec: ServerSpec,
        workspace: &Path,
        cache: &Path,
        clock: &Arc<dyn Clock>,
        version: &str,
        server: Server,
        listed: Vec<Value>,
    ) -> Arc<Self> {
        Self::new(
            spec,
            workspace,
            cache,
            clock,
            version,
            State::Running {
                server: Arc::new(server),
                live: names(&listed),
                listed,
                restarted: false,
            },
        )
    }

    /// The server to call `tool` on, starting it first when the slot is not
    /// started, and restarting it when it died with its restart unused. The
    /// lock is held across [`Server::start`], so a concurrent call waits for
    /// that start (bounded by the startup deadline) and then sees the same
    /// outcome, and only the call that ran it carries its lines.
    pub(crate) fn run(&self, tool: &str) -> Run {
        let mut state = lock(&self.state);
        let mut records = Vec::new();
        let (listed, restart) = match &*state {
            State::Running {
                server,
                live,
                listed,
                restarted,
            } => {
                if !server.is_gone() {
                    return if live.contains(tool) {
                        Run::Call(Arc::clone(server), records)
                    } else {
                        Run::Removed(records)
                    };
                }
                // It died while idle: this call is the first to see it.
                let record = died(&self.spec.name, !*restarted);
                records.push(ServerRecord::Failed(record.clone()));
                if *restarted {
                    *state = State::Dead {
                        error: record.error.clone(),
                    };
                    return Run::Failed(Box::new(RunFailed {
                        error: record.error,
                        records,
                    }));
                }
                (listed.clone(), true)
            }
            State::Down { listed } => (listed.clone(), true),
            State::Dead { error } => {
                return Run::Failed(Box::new(RunFailed {
                    error: error.clone(),
                    records,
                }));
            }
            State::NotStarted { cached } => (cached.clone(), false),
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
                if open.tools != listed {
                    cache::write(
                        &self.cache,
                        &name,
                        &cache::key(&self.spec.command, &self.spec.args, &self.spec.env),
                        &open.tools,
                    );
                }
                let live = names(&open.tools);
                let present = live.contains(tool);
                let server = Arc::new(open.server);
                *state = State::Running {
                    server: Arc::clone(&server),
                    live,
                    listed: open.tools,
                    restarted: restart,
                };
                if restart {
                    records.push(ServerRecord::Ready(McpServerReady { server: name }));
                }
                if present {
                    Run::Call(server, records)
                } else {
                    Run::Removed(records)
                }
            }
            Err(error) => {
                // A failed first start is the server's one death; a failed
                // restart is its second.
                let mut record = failed(&name, &error, timeout, false);
                record.will_restart = !restart;
                let failure = record.error.clone();
                records.push(ServerRecord::Failed(record));
                *state = if restart {
                    State::Dead {
                        error: failure.clone(),
                    }
                } else {
                    State::Down { listed }
                };
                Run::Failed(Box::new(RunFailed {
                    error: failure,
                    records,
                }))
            }
        }
    }

    /// Records that `gone`, which a call found gone mid-call, died: the
    /// record only when the slot still runs that same server, so a death
    /// many calls see is recorded once, and a server the slot already
    /// replaced or stopped records nothing.
    pub(crate) fn died(&self, gone: &Arc<Server>) -> Option<McpServerFailed> {
        let mut state = lock(&self.state);
        let State::Running {
            server,
            listed,
            restarted,
            ..
        } = &*state
        else {
            return None;
        };
        if !Arc::ptr_eq(server, gone) {
            return None;
        }
        let record = died(&self.spec.name, !*restarted);
        *state = if *restarted {
            State::Dead {
                error: record.error.clone(),
            }
        } else {
            State::Down {
                listed: listed.clone(),
            }
        };
        Some(record)
    }

    /// Stops the running server, if any, and leaves the slot dead from any
    /// state, so nothing spawns or restarts after the stop. A slot an
    /// in-flight start holds is stopped after that start returns, bounded
    /// by the startup deadline.
    pub(crate) fn stop(&self) {
        let mut state = lock(&self.state);
        let error = match &*state {
            State::Dead { error } => error.clone(),
            State::NotStarted { .. } | State::Running { .. } | State::Down { .. } => Failure {
                code: ErrorCode::McpServerUnavailable,
                message: unavailable(&self.spec.name),
                retry_after_ms: None,
                provider: None,
            },
        };
        let previous = std::mem::replace(&mut *state, State::Dead { error });
        drop(state);
        if let State::Running { server, .. } = previous {
            server.stop();
        }
    }
}

/// Every name in raw `tools/list` entries.
fn names(listed: &[Value]) -> HashSet<String> {
    listed
        .iter()
        .map(|entry| ListedTool::read(entry).name)
        .collect()
}

/// The record of a server that exited: Fiber restarts it on the next call
/// when `will_restart`, and otherwise its tools fail for the session.
fn died(server: &str, will_restart: bool) -> McpServerFailed {
    let message = if will_restart {
        format!("The MCP server `{server}` exited; Fiber restarts it on the next call.")
    } else {
        format!("The MCP server `{server}` exited again; its tools fail until the next session.")
    };
    McpServerFailed {
        server: server.to_owned(),
        reason: ServerFailure::Died,
        will_restart,
        error: Failure {
            code: ErrorCode::McpServerUnavailable,
            message,
            retry_after_ms: None,
            provider: None,
        },
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
