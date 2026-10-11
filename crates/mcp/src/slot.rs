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
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{McpServerFailed, McpServerReady, ServerFailure};
use contract::shapes::Failure;
use contract::tool::Cancel;
use contract::tool::ServerRecord;
use serde::Serialize;

use crate::cache::{self, Cached};
use crate::server::{CallError, Server};
use crate::server_json::ListedTool;
use crate::start::{ServerSpec, not_started};
use serde_json::Value;

/// One server's place in the session: what [`Slot::run`] starts, calls,
/// restarts or refuses, and what [`Slot::stop`] stops.
pub(crate) struct Slot {
    spec: ServerSpec,
    workspace: PathBuf,
    cache: PathBuf,
    clock: Arc<dyn Clock>,
    version: String,
    /// Whether the server offers resources to read, fixed from the lists
    /// the slot was declared from: the session keeps its declared tools,
    /// so a server that stops advertising resources is still asked.
    resources: bool,
    state: Mutex<State>,
    /// Test-only pause after `serve` returns and before `run` checks the live server.
    #[cfg(test)]
    run_after_serve: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// What the slot holds. `listed` is always the last raw lists this slot
/// saw, for the cache rewrite check on the next start.
enum State {
    /// Declared from the cache; no process runs.
    NotStarted {
        /// The cached raw lists.
        cached: Cached,
    },
    /// The server runs; `live` is every tool name it listed.
    Running {
        server: Arc<Server>,
        live: HashSet<String>,
        listed: Cached,
        /// Whether this is the restart, so the next death is final.
        restarted: bool,
    },
    /// One death seen and its restart unused: the next call restarts.
    Down { listed: Cached },
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

/// Why a call through [`Slot::call`] failed.
#[allow(
    clippy::result_large_err,
    reason = "the death record travels with the call that observed it"
)]
pub(crate) enum Fault {
    /// Nothing answered before the call's deadline.
    Timeout,
    /// The call was cancelled.
    Cancelled,
    /// The server refused the call, with its message.
    Refused(String),
    /// This call observed the death and carries its record.
    Died(McpServerFailed),
    /// This call saw the remote session expire; nothing is recorded.
    Expired,
    /// The slot had already moved on: it was stopped or replaced, and
    /// nothing is recorded.
    Gone,
}

/// What [`Slot::serve`] found, with the server lines the call carries.
pub(crate) enum Served {
    /// The server runs: call it, or ask it for a prompt.
    Up(Arc<Server>, Vec<ServerRecord>),
    /// The server is dead or its start failed: fail `error`.
    Failed(Box<RunFailed>),
}

impl Slot {
    /// One slot from its parts, in `state`: `resources` is fixed here
    /// from the lists the slot was declared from and never changes after.
    fn new(
        spec: ServerSpec,
        workspace: &Path,
        cache: &Path,
        clock: &Arc<dyn Clock>,
        version: &str,
        state: State,
    ) -> Arc<Self> {
        let resources = match &state {
            State::NotStarted { cached } => cached.resources,
            State::Running { listed, .. } | State::Down { listed } => listed.resources,
            State::Dead { .. } => false,
        };
        Arc::new(Self {
            spec,
            workspace: workspace.to_path_buf(),
            cache: cache.to_path_buf(),
            clock: Arc::clone(clock),
            version: version.to_owned(),
            resources,
            state: Mutex::new(state),
            #[cfg(test)]
            run_after_serve: Mutex::new(None),
        })
    }

    /// A slot declared from `cached`, starting nothing until the first
    /// call to one of its tools or the first run of one of its prompts.
    pub(crate) fn lazy(
        spec: ServerSpec,
        workspace: &Path,
        cache: &Path,
        clock: &Arc<dyn Clock>,
        version: &str,
        cached: Cached,
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
    /// `listed` lists for the removed-tool and cache checks.
    pub(crate) fn running(
        spec: ServerSpec,
        workspace: &Path,
        cache: &Path,
        clock: &Arc<dyn Clock>,
        version: &str,
        server: Server,
        listed: Cached,
    ) -> Arc<Self> {
        Self::new(
            spec,
            workspace,
            cache,
            clock,
            version,
            State::Running {
                server: Arc::new(server),
                live: names(&listed.tools),
                listed,
                restarted: false,
            },
        )
    }

    /// Whether the server offers resources to read.
    pub(crate) fn offers_resources(&self) -> bool {
        self.resources
    }

    /// The server's spec: its name, timeouts and transport.
    pub(crate) fn spec(&self) -> &ServerSpec {
        &self.spec
    }

    /// The running server, starting it first when the slot is not
    /// started, and restarting it when it died with its restart unused.
    /// The lock is held across [`Server::start`], so a concurrent call
    /// waits for that start (bounded by the startup deadline) and then
    /// sees the same outcome, and only the call that ran it carries its
    /// lines.
    pub(crate) fn serve(&self) -> Served {
        let mut state = lock(&self.state);
        let mut records = Vec::new();
        // What the next start is: a first start, a restart after a
        // death, or a renewal after an expiry. Only a restart is
        // recorded as ready; a renewal keeps the restart it replaces.
        let (listed, restart, ready) = match &*state {
            State::Running {
                server,
                listed,
                restarted,
                ..
            } => {
                if server.is_expired() {
                    // The session expired while idle: the next call starts
                    // a new one, keeping the restart, with no record.
                    (listed.clone(), *restarted, false)
                } else if !server.is_gone() {
                    return Served::Up(Arc::clone(server), records);
                } else {
                    // It died while idle: this call is the first to see it.
                    let record = died(&self.spec.name, !*restarted, self.spec.remote());
                    records.push(ServerRecord::Failed(record.clone()));
                    if *restarted {
                        *state = State::Dead {
                            error: record.error.clone(),
                        };
                        return Served::Failed(Box::new(RunFailed {
                            error: record.error,
                            records,
                        }));
                    }
                    (listed.clone(), true, true)
                }
            }
            State::Down { listed } => (listed.clone(), true, true),
            State::Dead { error } => {
                return Served::Failed(Box::new(RunFailed {
                    error: error.clone(),
                    records,
                }));
            }
            State::NotStarted { cached } => (cached.clone(), false, false),
        };
        let timeout = self.spec.startup_timeout;
        let name = self.spec.name.clone();
        // Every spawn runs the spec's guard first: a cached server keeps
        // its spec unspawned until its first call, so a check at
        // preparation alone would run a file created in between unapproved.
        match crate::start::launch(&self.spec, &self.workspace, &self.clock, &self.version) {
            Ok(open) => {
                // The session keeps the tools it declared, because a tool
                // set that changes mid-session misses the whole prompt
                // cache; the cache is updated for the next session.
                let live = open.listed;
                if live != listed {
                    cache::write(&self.cache, &name, &self.spec.key(), &live);
                }
                let tools = names(&live.tools);
                let server = Arc::new(open.server);
                *state = State::Running {
                    server: Arc::clone(&server),
                    live: tools,
                    listed: live,
                    restarted: restart,
                };
                if ready {
                    records.push(ServerRecord::Ready(McpServerReady { server: name }));
                }
                Served::Up(server, records)
            }
            Err(error) => {
                // A failed first start is the server's one death; a failed
                // restart is its second. A failed renewal is a death of
                // the server it replaces, keeping the restart it held.
                let mut record = not_started(&name, &error, timeout, false, self.spec.remote());
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
                Served::Failed(Box::new(RunFailed {
                    error: failure,
                    records,
                }))
            }
        }
    }

    /// The server to call `tool` on: [`Slot::serve`] plus the live-tool
    /// check, read under a second lock. When the served server is no
    /// longer the running one, its call would fail `Gone`, so the call
    /// fails without calling, exactly as that path does.
    pub(crate) fn run(&self, tool: &str) -> Run {
        match self.serve() {
            Served::Failed(failed) => Run::Failed(failed),
            Served::Up(server, records) => {
                #[cfg(test)]
                if let Some(hook) = lock(&self.run_after_serve).take() {
                    hook();
                }
                let present = match &*lock(&self.state) {
                    State::Running {
                        server: live,
                        live: tools,
                        ..
                    } if Arc::ptr_eq(live, &server) => tools.contains(tool),
                    // The served server stopped running between `serve`
                    // and this check; its call would fail `Gone`.
                    State::Running { .. }
                    | State::NotStarted { .. }
                    | State::Down { .. }
                    | State::Dead { .. } => {
                        return Run::Failed(Box::new(RunFailed {
                            error: crate::fail::failure(
                                ErrorCode::McpServerUnavailable,
                                unavailable(&self.spec.name),
                            ),
                            records,
                        }));
                    }
                };
                if present {
                    Run::Call(server, records)
                } else {
                    Run::Removed(records)
                }
            }
        }
    }

    /// Calls `method` on `server`, mapping the wire outcome to one step.
    /// `server` is the one [`Slot::run`] or [`Slot::serve`] returned. The
    /// call is never replayed.
    #[allow(
        clippy::result_large_err,
        reason = "the death record travels with the call that observed it"
    )]
    pub(crate) fn call<P: Serialize + ?Sized>(
        &self,
        server: &Arc<Server>,
        method: &str,
        params: &P,
        timeout: Duration,
        cancel: &dyn Cancel,
    ) -> Result<Value, Fault> {
        match server.call(method, params, timeout, cancel) {
            Ok(value) => Ok(value),
            Err(CallError::Timeout) => Err(Fault::Timeout),
            Err(CallError::Cancelled) => Err(Fault::Cancelled),
            Err(CallError::JsonRpc { message, .. }) => Err(Fault::Refused(message)),
            Err(CallError::Http(message)) => Err(Fault::Refused(message)),
            Err(CallError::Expired) => Err(Fault::Expired),
            Err(CallError::Gone) => match self.died(server) {
                Some(record) => Err(Fault::Died(record)),
                None => Err(Fault::Gone),
            },
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
        let record = died(&self.spec.name, !*restarted, self.spec.remote());
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
            State::NotStarted { .. } | State::Running { .. } | State::Down { .. } => {
                crate::fail::failure(
                    ErrorCode::McpServerUnavailable,
                    unavailable(&self.spec.name),
                )
            }
        };
        let previous = std::mem::replace(&mut *state, State::Dead { error });
        drop(state);
        if let State::Running { server, .. } = previous {
            server.stop();
        }
    }
}

/// Every name in the listed tools.
fn names(listed: &[ListedTool]) -> HashSet<String> {
    listed.iter().map(|tool| tool.name.clone()).collect()
}

/// The record of a server that exited: Fiber restarts it on the next call
/// when `will_restart`, and otherwise its tools fail for the session. A
/// remote server stopped answering instead of exiting.
fn died(server: &str, will_restart: bool, remote: bool) -> McpServerFailed {
    let message = if remote {
        if will_restart {
            format!(
                "The MCP server `{server}` stopped answering; Fiber reconnects on the next call."
            )
        } else {
            format!(
                "The MCP server `{server}` stopped answering again; its tools fail until the next session."
            )
        }
    } else if will_restart {
        format!("The MCP server `{server}` exited; Fiber restarts it on the next call.")
    } else {
        format!("The MCP server `{server}` exited again; its tools fail until the next session.")
    };
    McpServerFailed {
        server: server.to_owned(),
        reason: ServerFailure::Died,
        will_restart,
        error: crate::fail::failure(ErrorCode::McpServerUnavailable, message),
    }
}

/// The message of a server that is gone: no spawn was tried, or its start
/// failed, or it has since exited.
pub(crate) fn unavailable(server: &str) -> String {
    format!("The MCP server `{server}` did not start, or it has since exited.")
}

/// The message of a call that saw the remote session expire: the next
/// call starts a new session, with no record (`docs/mcp.md`, "When a
/// server dies").
pub(crate) fn expired(server: &str) -> String {
    format!("The MCP server `{server}` ended its session; Fiber starts a new one on the next call.")
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
