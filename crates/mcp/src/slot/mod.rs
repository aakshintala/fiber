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
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use contract::clock::Clock;
use contract::diag::DebugLog;
use contract::events::{McpServerFailed, McpServerReady, ServerFailure};
use contract::shapes::Failure;
use contract::tool::Cancel;
use contract::tool::ServerRecord;
use serde::Serialize;
use support::lock;

use crate::cache::{self, Cached};
use crate::elicit::Asker;
use crate::error::{McpError, login_sentence};
use crate::server::{CallError, Server};
use crate::server_json::ListedTool;
use crate::start::{ServerSpec, not_started};
use serde_json::Value;

/// One server's place in the session: what [`Slot::run`] starts, calls,
/// restarts or refuses, and what [`Slot::stop`] stops.
pub(crate) struct Slot {
    spec: Mutex<ServerSpec>,
    workspace: PathBuf,
    cache: PathBuf,
    clock: Arc<dyn Clock>,
    version: String,
    /// The timing context an attempt starts its connection guard from:
    /// `None` starts untimed, as before. Never logged.
    timing: Option<contract::diag::Timing>,
    /// Where the server's `http_request` lines go: `None` stays silent,
    /// as before.
    debug: Option<Arc<dyn DebugLog>>,
    /// One in-flight start at a time: the flag with its wake. The flag is
    /// set and cleared under the mutex but never held across a start,
    /// so a concurrent start or stop waits for the attempt's outcome
    /// instead of spawning beside it.
    lease: Mutex<bool>,
    wake: Condvar,
    /// Test-only: signaled once when an acquirer finds the lease held,
    /// so a test releases its hold only after the contender waits.
    #[cfg(test)]
    wait_hook: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    /// Test-only: runs after `serve` reserves the attempt and before it
    /// launches, so a test holds the replacement on the reservation.
    #[cfg(test)]
    after_reserve: Mutex<Option<Box<dyn FnOnce() + Send>>>,
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
        /// Whether the handshake advertised the tools capability: the
        /// re-list asks again for advertised sections, even when the
        /// last list came back empty.
        advertises_tools: bool,
        /// Whether the handshake advertised the prompts capability.
        advertises_prompts: bool,
        /// Whether this is the restart, so the next death is final.
        restarted: bool,
    },
    /// One death seen and its restart unused: the next call restarts.
    Down { listed: Cached },
    /// A second death, or a stop: every call fails `error`, no spawn again.
    Dead { error: McpError },
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
    pub error: McpError,
    /// The lines this call observed; empty for a call that only found the
    /// server dead.
    pub records: Vec<ServerRecord>,
}

/// Why a call through [`Slot::call`] failed.
#[allow(
    clippy::result_large_err,
    clippy::large_enum_variant,
    reason = "the death record travels with the call that observed it"
)]
pub(crate) enum Fault {
    /// Nothing answered before the call's deadline.
    Timeout,
    /// The call was cancelled.
    Cancelled,
    /// The server refused the call, with its message.
    Refused(String),
    /// This call observed the death and carries its record with the typed
    /// error it was built from.
    Died(Death),
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

/// The slot's start lease: one in-flight start at a time, so a
/// concurrent start or stop waits for the attempt's outcome instead of
/// spawning beside it.
///
/// One in-flight start's hold on its slot: clearing the flag and waking
/// the waiters on drop, so an unwind releases the lease too.
pub(super) struct LeaseGuard<'a> {
    lease: &'a Mutex<bool>,
    wake: &'a Condvar,
}

impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        *lock(self.lease) = false;
        self.wake.notify_all();
    }
}

/// The running-server re-list: the slot entry extracts what the re-list
/// needs and passes it explicitly; the child does the RPC and payload
/// work, reached through [`Slot::relist`], which the sync boundary
/// calls.
mod relist;

pub(crate) use relist::Relisted;

impl Slot {
    /// Takes the slot's start lease, waiting while another attempt holds
    /// it: one start runs at a time, and a stop waits for the in-flight
    /// one. The mutex is never held across a start, only across the flag;
    /// the guard releases it on unwind too, so a panic never wedges the
    /// slot.
    fn acquire(&self) -> LeaseGuard<'_> {
        let mut busy = lock(&self.lease);
        while *busy {
            #[cfg(test)]
            self.note_lease_wait();
            busy = self.wake.wait(busy).unwrap_or_else(PoisonError::into_inner);
        }
        *busy = true;
        LeaseGuard {
            lease: &self.lease,
            wake: &self.wake,
        }
    }

    /// Whether both locks are free right now: the state lock and the
    /// lease lock. Test-only: a test's timing guard calls it while the
    /// attempt's guard drops, proving no diagnostic drop holds either
    /// lock. A mutant that moves the drop under one fails the test.
    #[cfg(test)]
    pub(crate) fn locks_free(&self) -> bool {
        self.state.try_lock().is_ok() && self.lease.try_lock().is_ok()
    }

    /// Signals once when an acquirer finds the lease held. Test-only:
    /// installed before a contender starts, taken on its first wait.
    #[cfg(test)]
    fn note_lease_wait(&self) {
        if let Some(hook) = lock(&self.wait_hook).take() {
            hook.send(()).unwrap_or(());
        }
    }

    /// Installs the one-shot [`Slot::note_lease_wait`] signal. Test-only.
    #[cfg(test)]
    pub(crate) fn on_lease_wait(&self, hook: std::sync::mpsc::Sender<()>) {
        *lock(&self.wait_hook) = Some(hook);
    }

    /// Re-lists the running server under the slot lease, bounded by the
    /// spec's `startup_timeout`: installs nothing — the caller stages the
    /// outcome, and the prompt payload feeds the cache write only — and
    /// any transport failure signals the replace path instead.
    pub(crate) fn relist(&self) -> Relisted {
        // The lease, held across the extraction as `serve` holds it
        // across a launch: the edge against a concurrent serve or stop.
        // The child never touches the slot: the target below is the
        // whole interface, so a death racing the RPC still finds the
        // slot exactly as the extraction left it.
        let _flight = self.acquire();
        let target = {
            let state = lock(&self.state);
            match &*state {
                State::Running {
                    server,
                    restarted,
                    advertises_tools,
                    advertises_prompts,
                    ..
                } if !server.is_gone() && !server.is_expired() => {
                    // The handshake lists only advertised capabilities: the
                    // slot holds what it advertised, so an advertised-but-
                    // empty section re-lists on reload, and a server that
                    // advertised an empty list picks up a tool added since.
                    // Listing an unadvertised section would hang a server
                    // that answers it with malformed JSON.
                    relist::RelistTarget {
                        server: Arc::clone(server),
                        restarted: *restarted,
                        list_tools: *advertises_tools,
                        list_prompts: *advertises_prompts,
                        timeout: lock(&self.spec).startup_timeout,
                        resources: self.resources,
                    }
                }
                State::Running { restarted, .. } => {
                    // A Running slot that fails the first arm is gone: `expired`
                    // is written at exactly one site, with `gone`, under the
                    // same lock (`wait.rs::expire_if_awaiting`), so an expired
                    // server is gone too and the guard above covered the live
                    // case. It died while idle: this re-list is the first to see
                    // it, as the entry of `serve` is for a call.
                    let snapshot = lock(&self.spec).clone();
                    let death = died(&snapshot.name, !*restarted, snapshot.remote());
                    return Relisted::Replace {
                        record: Some(ServerRecord::Failed(death.record)),
                    };
                }
                State::NotStarted { .. } | State::Down { .. } | State::Dead { .. } => {
                    return Relisted::Replace { record: None };
                }
            }
        };
        // The state lock is released for the RPC, as `serve` releases it
        // for a launch: a concurrent serve or stop moves the slot while
        // the listing runs, and the commit revalidates under its own
        // lock before installing anything.
        let restarted = target.restarted;
        match relist::fetch(&target, &self.clock) {
            relist::Fetched::Up { listed } => Relisted::Up { listed },
            relist::Fetched::Failed {
                error: Some(CallError::Timeout),
            } => {
                // The bound outlived the listing: the startup deadline
                // the handshake's listing shares, restartable as a death
                // the next call would restart. Built without moving the
                // slot.
                let snapshot = lock(&self.spec).clone();
                let mut death = not_started(
                    &snapshot.name,
                    &crate::server::StartError::Deadline,
                    snapshot.startup_timeout,
                    snapshot.required,
                    snapshot.remote(),
                );
                death.record.will_restart = true;
                Relisted::Replace {
                    record: Some(ServerRecord::Failed(death.record)),
                }
            }
            relist::Fetched::Failed {
                error: Some(CallError::Gone),
            } => {
                let snapshot = lock(&self.spec).clone();
                let death = died(&snapshot.name, !restarted, snapshot.remote());
                Relisted::Replace {
                    record: Some(ServerRecord::Failed(death.record)),
                }
            }
            relist::Fetched::Failed { .. } => Relisted::Replace { record: None },
        }
    }
}

impl Slot {
    /// One slot from its parts, in `state`: `resources` is fixed here
    /// from the lists the slot was declared from and never changes after.
    #[allow(
        clippy::too_many_arguments,
        reason = "one slot's parts plus the attempt's Timing and sink"
    )]
    fn new(
        spec: ServerSpec,
        workspace: &Path,
        cache: &Path,
        clock: &Arc<dyn Clock>,
        version: &str,
        state: State,
        timing: Option<contract::diag::Timing>,
        debug: Option<Arc<dyn DebugLog>>,
    ) -> Arc<Self> {
        let resources = match &state {
            State::NotStarted { cached } => cached.resources,
            State::Running { listed, .. } | State::Down { listed } => listed.resources,
            State::Dead { .. } => false,
        };
        Arc::new(Self {
            spec: Mutex::new(spec),
            workspace: workspace.to_path_buf(),
            cache: cache.to_path_buf(),
            clock: Arc::clone(clock),
            version: version.to_owned(),
            resources,
            timing,
            debug,
            lease: Mutex::new(false),
            wake: Condvar::new(),
            #[cfg(test)]
            wait_hook: Mutex::new(None),
            #[cfg(test)]
            after_reserve: Mutex::new(None),
            state: Mutex::new(state),
            #[cfg(test)]
            run_after_serve: Mutex::new(None),
        })
    }

    /// A slot declared from `cached`, starting nothing until the first
    /// call to one of its tools or the first run of one of its prompts.
    /// Prompt tests only: production declares through `lazy_observed`.
    #[cfg(test)]
    pub(crate) fn lazy(
        spec: ServerSpec,
        workspace: &Path,
        cache: &Path,
        clock: &Arc<dyn Clock>,
        version: &str,
        cached: Cached,
    ) -> Arc<Self> {
        Self::lazy_observed(spec, workspace, cache, clock, version, cached, None, None)
    }

    /// As [`Slot::lazy`], keeping `timing` for the first attempt's
    /// connection guard: `None` starts untimed, as before. `debug`
    /// observes the attempt's requests as `mcp`; `None` stays silent.
    #[allow(
        clippy::too_many_arguments,
        reason = "the observed companion adds Timing and the sink"
    )]
    pub(crate) fn lazy_observed(
        spec: ServerSpec,
        workspace: &Path,
        cache: &Path,
        clock: &Arc<dyn Clock>,
        version: &str,
        cached: Cached,
        timing: Option<contract::diag::Timing>,
        debug: Option<Arc<dyn DebugLog>>,
    ) -> Arc<Self> {
        Self::new(
            spec,
            workspace,
            cache,
            clock,
            version,
            State::NotStarted { cached },
            timing,
            debug,
        )
    }

    /// A slot for a server already running, holding its raw `listed`
    /// lists for the removed-tool and cache checks, keeping `timing` for
    /// a later restart or renewal attempt's connection guard: `None` stays
    /// untimed. `debug` observes that attempt's requests as `mcp`;
    /// `None` stays silent.
    #[allow(
        clippy::too_many_arguments,
        reason = "the observed companion adds Timing and the sink plus the advertises_tools and advertises_prompts flags"
    )]
    pub(crate) fn running_observed(
        spec: ServerSpec,
        workspace: &Path,
        cache: &Path,
        clock: &Arc<dyn Clock>,
        version: &str,
        server: Server,
        listed: Cached,
        advertises_tools: bool,
        advertises_prompts: bool,
        timing: Option<contract::diag::Timing>,
        debug: Option<Arc<dyn DebugLog>>,
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
                advertises_tools,
                advertises_prompts,
                restarted: false,
            },
            timing,
            debug,
        )
    }

    /// Whether the server offers resources to read.
    pub(crate) fn offers_resources(&self) -> bool {
        self.resources
    }

    /// The server's spec: its name, timeouts and transport. Cloned under
    /// a short lock, never held across a launch or an RPC, so a reload
    /// that swaps the spec cannot wedge against a concurrent call.
    pub(crate) fn spec(&self) -> ServerSpec {
        lock(&self.spec).clone()
    }

    /// Persists the reload's new spec on a kept slot, so the slot, the
    /// next classify and the call timeouts read the re-declared values.
    /// The staged declaration already used this spec; the swap lands at
    /// apply, beside the re-listed install.
    pub(crate) fn set_spec(&self, spec: ServerSpec) {
        *lock(&self.spec) = spec;
    }

    /// Whether the slot served before and serves no more: `Down`
    /// after a first death, `Dead` after a second or a stop. The
    /// reload replaces those beside the old slot (shadow-start first,
    /// commit swaps, the name lands in `restarted`); a `NotStarted`
    /// slot never served and starts fresh through the forced stage
    /// path.
    pub(crate) fn served_before(&self) -> bool {
        matches!(&*lock(&self.state), State::Down { .. } | State::Dead { .. })
    }

    /// Whether the server runs now: the slot holds a live server. The
    /// hub's `mcp_servers` answer reads this live, so a server that
    /// died mid-session reads not-running and its row falls back to the
    /// failure the log holds (`docs/mcp.md`, "Servers, their state and
    /// the per-server commands").
    pub(crate) fn is_running(&self) -> bool {
        matches!(*lock(&self.state), State::Running { .. })
    }

    /// Whether the slot answers a re-list: running with a live, unexpired
    /// server. A server that died or expired while idle replaces: the
    /// classification reads this, so a dead slot never takes the keep
    /// path.
    pub(crate) fn is_live(&self) -> bool {
        match &*lock(&self.state) {
            State::Running { server, .. } => !server.is_gone() && !server.is_expired(),
            State::NotStarted { .. } | State::Down { .. } | State::Dead { .. } => false,
        }
    }

    /// The bare tool names live now: the running server's list, else
    /// empty. A restart drops the qualified names this set no longer
    /// holds.
    pub(crate) fn live_names(&self) -> HashSet<String> {
        match &*lock(&self.state) {
            State::Running { live, .. } => live.clone(),
            State::Down { .. } | State::Dead { .. } | State::NotStarted { .. } => HashSet::new(),
        }
    }

    /// The last raw tool list this slot saw, for declaring after a
    /// restart: the running or reserved list, else empty.
    pub(crate) fn listed_tools(&self) -> Vec<ListedTool> {
        match &*lock(&self.state) {
            State::Running { listed, .. }
            | State::Down { listed }
            | State::NotStarted { cached: listed } => listed.tools.clone(),
            State::Dead { .. } => Vec::new(),
        }
    }

    /// Installs re-listed `listed` on a still-running slot, refreshing the
    /// live set the removed-tool check reads, and answers whether the
    /// install landed. A slot whose server died, stopped or was replaced
    /// between the re-list and the commit keeps what it has and answers
    /// false — the check and the install hold the same lock, so the
    /// answer also covers a death racing the commit — and the commit
    /// writes the cache only on true, so a dead keep never persists lists
    /// its server never served. Holds only the state lock, never the
    /// lease, so it cannot wedge against a concurrent serve or stop.
    pub(crate) fn install_relisted(&self, listed: Cached) -> bool {
        let mut state = lock(&self.state);
        if let State::Running {
            server,
            live,
            listed: held,
            ..
        } = &mut *state
        {
            // Revalidated under the same lock: a keep that died after
            // the stage keeps what it has.
            if server.is_gone() || server.is_expired() {
                return false;
            }
            *live = names(&listed.tools);
            *held = listed;
            return true;
        }
        false
    }

    /// The running server, starting it first when the slot is not
    /// started, and restarting it when it died with its restart unused.
    /// One attempt runs at a time under the slot's lease: a concurrent
    /// call waits for that attempt's outcome, bounded by the startup
    /// deadline, and then sees the same running server or the same
    /// failure, and only the call that ran it carries its lines. The
    /// state lock is released for the launch, and the attempt is
    /// reserved as down meanwhile, so a late death report for the server
    /// being replaced finds no running server and records nothing; no
    /// diagnostic guard is created or dropped with either lock held.
    pub(crate) fn serve(&self) -> Served {
        // The lease, held across the attempt and released on unwind too:
        // a second start never spawns beside this one, and a stop waits
        // for its result.
        let _flight = self.acquire();
        let mut records = Vec::new();
        // What the next start is: a first start, a restart after a
        // death, or a renewal after an expiry. Only a restart is
        // recorded as ready; a renewal keeps the restart it replaces.
        let (listed, restart, ready) = {
            let mut state = lock(&self.state);
            match &*state {
                State::Running {
                    server,
                    listed,
                    restarted,
                    ..
                } => {
                    if server.is_expired() {
                        // The session expired while idle: the next call starts
                        // a new one, keeping the restart, with no record.
                        let listed = listed.clone();
                        let restart = *restarted;
                        *state = State::Down {
                            listed: listed.clone(),
                        };
                        (listed, restart, false)
                    } else if !server.is_gone() {
                        return Served::Up(Arc::clone(server), records);
                    } else {
                        // It died while idle: this call is the first to see it.
                        let snapshot = lock(&self.spec).clone();
                        let death = died(&snapshot.name, !*restarted, snapshot.remote());
                        records.push(ServerRecord::Failed(death.record.clone()));
                        if *restarted {
                            *state = State::Dead {
                                error: death.error.clone(),
                            };
                            return Served::Failed(Box::new(RunFailed {
                                error: death.error,
                                records,
                            }));
                        }
                        let listed = listed.clone();
                        *state = State::Down {
                            listed: listed.clone(),
                        };
                        (listed, true, true)
                    }
                }
                State::Down { listed } => (listed.clone(), true, true),
                State::Dead { error } => {
                    return Served::Failed(Box::new(RunFailed {
                        error: error.clone(),
                        records,
                    }));
                }
                // A first start: reserved as down for the launch, so a
                // late report during it records nothing, exactly as for a
                // replacement. A failed first start is still the server's
                // one death.
                State::NotStarted { cached } => {
                    let listed = cached.clone();
                    *state = State::Down {
                        listed: listed.clone(),
                    };
                    (listed, false, false)
                }
            }
        };
        #[cfg(test)]
        if let Some(hook) = lock(&self.after_reserve).take() {
            hook();
        }
        let snapshot = lock(&self.spec).clone();
        let timeout = snapshot.startup_timeout;
        let name = snapshot.name.clone();
        // The slot's own context follows the attempt; the launch owns
        // the guard and consumes it at the tool-list boundary, dropping
        // what remains on the failure path here, with no lock held.
        let timing = self.timing.clone();
        // Every spawn runs the spec's guard first: a cached server keeps
        // its spec unspawned until its first call, so a check at
        // preparation alone would run a file created in between unapproved.
        match crate::start::launch_observed(
            &snapshot,
            &self.workspace,
            &self.clock,
            &self.version,
            timing,
            self.debug.clone(),
        ) {
            Ok(open) => {
                // The session keeps the tools it declared, because a tool
                // set that changes mid-session misses the whole prompt
                // cache; the cache is updated for the next session.
                let live = open.listed;
                let advertises_tools = open.advertises_tools;
                let advertises_prompts = open.advertises_prompts;
                if live != listed {
                    cache::write(&self.cache, &name, &snapshot.key(), &live);
                }
                let tools = names(&live.tools);
                let server = Arc::new(open.server);
                // Published under the lock before the lease is released,
                // so a waiter sees the running server, never the
                // reservation.
                *lock(&self.state) = State::Running {
                    server: Arc::clone(&server),
                    live: tools,
                    listed: live,
                    advertises_tools,
                    advertises_prompts,
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
                let mut death = not_started(&name, &error, timeout, false, snapshot.remote());
                death.record.will_restart = !restart;
                let error = death.error.clone();
                records.push(ServerRecord::Failed(death.record));
                *lock(&self.state) = if restart {
                    State::Dead {
                        error: error.clone(),
                    }
                } else {
                    State::Down { listed }
                };
                Served::Failed(Box::new(RunFailed { error, records }))
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
                            error: McpError::ServerUnavailable {
                                server: lock(&self.spec).name.clone(),
                            },
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

    /// The death this check is the first to see, as the entry of `serve`
    /// is for a call: a running server that exited while idle, built
    /// without moving the slot. Nothing for an expiry, which restarts
    /// with no record, or a live server: the stage reads this to carry
    /// the old death into the replacement's records.
    pub(crate) fn idle_death(&self) -> Option<ServerRecord> {
        let state = lock(&self.state);
        let State::Running {
            server, restarted, ..
        } = &*state
        else {
            return None;
        };
        if !server.is_gone() {
            return None;
        }
        let snapshot = lock(&self.spec).clone();
        let death = died(&snapshot.name, !*restarted, snapshot.remote());
        Some(ServerRecord::Failed(death.record))
    }

    /// Maps a wire outcome to its fault: the death record only when the
    /// slot still runs that same server, so a death many calls see is
    /// recorded once. Both [`Slot::call`] and [`Slot::call_asking`] map
    /// through here, so the mapping cannot drift between them.
    fn fault(&self, server: &Arc<Server>, error: CallError) -> Fault {
        match error {
            CallError::Timeout => Fault::Timeout,
            CallError::Cancelled => Fault::Cancelled,
            CallError::JsonRpc { message, .. } => Fault::Refused(message),
            CallError::Http(message) => Fault::Refused(message),
            CallError::Login(detail) => {
                Fault::Refused(login_sentence(&lock(&self.spec).name, &detail))
            }
            CallError::Expired => Fault::Expired,
            CallError::Gone => match self.died(server) {
                Some(death) => Fault::Died(death),
                None => Fault::Gone,
            },
        }
    }

    /// Calls `method` on `server`, mapping the wire outcome to one step.
    /// `server` is the one [`Slot::run`] or [`Slot::serve`] returned. The
    /// call is never replayed. The wrapper keeps no asker: only the tool
    /// and `mcp_resources` ask, through [`Slot::call_asking`].
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
            Err(error) => Err(self.fault(server, error)),
        }
    }

    /// Calls `method` on `server` as [`Slot::call`] does, asking
    /// elicitations through `asker`: the call registers before its line
    /// is sent, without bypassing the fault and death mapping here.
    #[allow(
        clippy::result_large_err,
        reason = "the death record travels with the call that observed it"
    )]
    pub(crate) fn call_asking<P: Serialize + ?Sized>(
        &self,
        server: &Arc<Server>,
        method: &str,
        params: &P,
        timeout: Duration,
        cancel: &dyn Cancel,
        asker: Asker<'_>,
    ) -> Result<Value, Fault> {
        match server.call_asking(method, params, timeout, cancel, Some(asker)) {
            Ok(value) => Ok(value),
            Err(error) => Err(self.fault(server, error)),
        }
    }

    /// Records that `gone`, which a call found gone mid-call, died: the
    /// record only when the slot still runs that same server, so a death
    /// many calls see is recorded once, and a server the slot already
    /// replaced or stopped records nothing.
    pub(crate) fn died(&self, gone: &Arc<Server>) -> Option<Death> {
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
        let snapshot = lock(&self.spec).clone();
        let death = died(&snapshot.name, !*restarted, snapshot.remote());
        *state = if *restarted {
            State::Dead {
                error: death.error.clone(),
            }
        } else {
            State::Down {
                listed: listed.clone(),
            }
        };
        Some(death)
    }

    /// Runs `hook` after the next `serve` reserves its attempt and
    /// before it launches. Test-only: holds the replacement on the
    /// reservation so a late death report lands inside it.
    #[cfg(test)]
    pub(crate) fn on_after_reserve(&self, hook: Box<dyn FnOnce() + Send>) {
        *lock(&self.after_reserve) = Some(hook);
    }

    /// Stops the running server, if any, and leaves the slot dead from any
    /// state, so nothing spawns or restarts after the stop. A slot with an
    /// in-flight start is stopped after that start returns, bounded by the
    /// startup deadline: the stop takes the same lease the start holds.
    pub(crate) fn stop(&self) {
        let _flight = self.acquire();
        let mut state = lock(&self.state);
        let error = match &*state {
            State::Dead { error } => error.clone(),
            State::NotStarted { .. } | State::Running { .. } | State::Down { .. } => {
                McpError::ServerUnavailable {
                    server: lock(&self.spec).name.clone(),
                }
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

/// A server death: the contract record the call carries, and the typed
/// error the record was built from, so a death and a restart keep one
/// error in both forms.
pub(crate) struct Death {
    /// The `mcp_server_failed` record.
    pub record: McpServerFailed,
    /// The typed error the record was built from.
    pub error: McpError,
}

/// The death of a server that exited: Fiber restarts it on the next call
/// when `will_restart`, and otherwise its tools fail for the session. A
/// remote server stopped answering instead of exiting.
fn died(server: &str, will_restart: bool, remote: bool) -> Death {
    let error = if remote {
        if will_restart {
            McpError::ServerStoppedAnswering {
                server: server.to_owned(),
            }
        } else {
            McpError::ServerStoppedAnsweringAgain {
                server: server.to_owned(),
            }
        }
    } else if will_restart {
        McpError::ServerExited {
            server: server.to_owned(),
        }
    } else {
        McpError::ServerExitedAgain {
            server: server.to_owned(),
        }
    };
    Death {
        record: McpServerFailed {
            server: server.to_owned(),
            reason: ServerFailure::Died,
            will_restart,
            error: Failure::from(&error),
        },
        error,
    }
}

#[cfg(test)]
mod tests;
