//! Process-extension lifecycle and runtime entry points behind the package
//! seam (`docs/extensions.md`, "Process extensions").

use std::sync::{Arc, Mutex};
use std::time::Duration;

use contract::ErrorCode;
use serde_json::Value;

use crate::Error;
use crate::runtime::{FunctionRef, Runtime};

mod registry;
mod validate;
pub(crate) use registry::*;

pub(crate) mod calls;
pub(crate) mod context;
pub(crate) mod lifecycle;
pub(crate) mod oauth;
pub(crate) mod requests;
#[cfg(test)]
mod tests;
pub(crate) mod transport;

/// One process extension behind the package seam: its child on the pipes,
/// its registrations, and the requests it answers (`docs/extensions.md`,
/// "Process extensions").
///
/// The owner pumps the child's lines on one thread: registrations answer
/// through the registry before `ready` and refuse after it, supervised
/// `call`, `ready` and `reply` parse through `CommandLine`, and ordinary
/// driver commands go through the `Drive` seam. Requests ride the shared
/// request table: stream items ordered one at a time, tool calls
/// concurrent, and one retirement seam for replies, deadlines and cancels.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) struct ProcessExtension {
    shared: Arc<Shared>,
}

/// What starting one process extension takes: the program and its
/// arguments, what the child runs as, and what answers its calls. One
/// value carries the launch so the start path stays narrow.
pub(crate) struct Launch {
    /// The extension's name.
    pub owner: String,
    /// The runtime its functions resolve through.
    pub runtime: Runtime,
    /// The registry its calls register with, shared with the package.
    pub registry: Arc<Mutex<Registry>>,
    /// The program to run.
    pub program: std::path::PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// What the child runs as: the hello's fields.
    pub context: context::ProcessContext,
    /// What answers its driver commands, absent while closing.
    pub drive: Option<Arc<dyn contract::extension::Drive>>,
    /// What answers its host and state calls.
    pub services: Arc<dyn context::ProcessHostServices>,
    /// Where its refresh provider's stored pair is held.
    pub oauth: oauth::FileCredential,
    /// The state store its hook writes join and its calls read.
    pub state: Arc<crate::host::state::ExtensionState>,
    /// The clock its timeouts read.
    pub clock: Arc<dyn contract::clock::Clock>,
    /// The pipe limits; the tests inject small ones.
    pub limits: transport::Limits,
}

/// What the owner's threads share: the extension's identity, its runtime
/// and registry, its request table and pipes, and what answers its calls.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) struct Shared {
    owner: String,
    runtime: Runtime,
    registry: Arc<Mutex<Registry>>,
    pub(crate) table: Arc<requests::Table>,
    /// The live child behind a lock, so the one restart swaps it while a
    /// pump holds only its own clone and never blocks a swap.
    transport: Mutex<Arc<transport::Transport>>,
    drive: Option<Arc<dyn contract::extension::Drive>>,
    services: Arc<dyn context::ProcessHostServices>,
    pub(crate) oauth: oauth::FileCredential,
    pub(crate) state: Arc<crate::host::state::ExtensionState>,
    clock: Arc<dyn contract::clock::Clock>,
    /// What the child runs as, behind a lock so the one restart refolds
    /// the state before resending the hello.
    context: Mutex<context::ProcessContext>,
    /// Whether the child sent `ready`.
    pub(crate) ready: std::sync::atomic::AtomicBool,
    /// Bumped on every `ready`: what startup and restart waits park on.
    pub(crate) ready_park: support::clock::Parker,
    /// The lifecycle one synchronized transition owns.
    pub(crate) lifecycle: Mutex<lifecycle::Lifecycle>,
    /// Wakes waiters on every lifecycle transition.
    pub(crate) lifecycle_park: support::clock::Parker,
    /// What respawns the child, as `Transport::spawn_with` takes it.
    pub(crate) spawn: lifecycle::Spawn,
}

/// What one `pump_once` did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum Pumped {
    /// One line handled and answered.
    Handled,
    /// The pipe closed; no more lines follow.
    Closed(transport::CloseReason),
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
impl ProcessExtension {
    /// Starts the launched program for its owner: the child runs in the
    /// launch's workspace, and `extension_hello` is the first complete
    /// outbound line. The registry is shared with the package, so the
    /// session fold reads what the child registers. One value carries
    /// the launch so the start path stays narrow.
    pub(crate) fn start(launch: Launch) -> std::io::Result<Self> {
        let Launch {
            owner,
            runtime,
            registry,
            program,
            args,
            context,
            drive,
            services,
            oauth,
            state,
            clock,
            limits,
        } = launch;
        let transport =
            transport::Transport::spawn_with(&program, &args, &context.workspace, limits)?;
        let extension = Self {
            shared: Arc::new(Shared {
                owner,
                runtime,
                registry,
                table: Arc::new(requests::Table::new()),
                transport: Mutex::new(Arc::new(transport)),
                drive,
                services,
                oauth,
                state,
                clock,
                context: Mutex::new(context),
                ready: std::sync::atomic::AtomicBool::new(false),
                ready_park: support::clock::Parker::new(),
                lifecycle: Mutex::new(lifecycle::Lifecycle::starting()),
                lifecycle_park: support::clock::Parker::new(),
                spawn: lifecycle::Spawn {
                    program,
                    args,
                    limits,
                },
            }),
        };
        let hello = extension.hello_line().map_err(std::io::Error::other)?;
        let live = Arc::clone(&support::lock(&extension.shared.transport));
        live.send(hello).map_err(|error| {
            std::io::Error::other(format!("the first hello never sent: {error:?}"))
        })?;
        Ok(extension)
    }

    /// The extension's name.
    pub(crate) fn owner(&self) -> &str {
        &self.shared.owner
    }

    /// Whether `ready` arrived: registrations stand only before it.
    pub(crate) fn is_ready(&self) -> bool {
        self.shared.ready.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Waits for the child's `ready` until `deadline`: the startup wait
    /// the session owns, bounded and cancellable, killing a wedged child
    /// instead of leaking it.
    pub(crate) fn await_ready(
        &self,
        deadline: Option<std::time::Instant>,
        cancel: &dyn contract::tool::Cancel,
    ) -> Result<(), (contract::ErrorCode, String)> {
        lifecycle::await_ready(&self.shared, &self.shared.ready, deadline, cancel)
    }

    /// Shuts the owner down: no new dispatches past this point, the
    /// in-flight requests drain first, and only then does the
    /// `exit_timeout` allowance start. The child that exits on its own
    /// or on TERM returns its status; a child that ignores TERM gets
    /// KILL after the grace. Cancel ends the wait and leaves the child
    /// for the caller; the owner stays finishing either way.
    pub(crate) fn finish(
        &self,
        exit_timeout: Duration,
        cancel: &dyn contract::tool::Cancel,
    ) -> Result<std::process::ExitStatus, (contract::ErrorCode, String)> {
        if !lifecycle::begin_finish(&self.shared) {
            return Err((
                contract::ErrorCode::Closing,
                format!(
                    "{}: the process extension is already finishing",
                    self.shared.owner
                ),
            ));
        }
        let clock = self.shared.clock.as_ref();
        if !self.shared.table.wait_idle(clock, cancel) {
            return Err((
                contract::ErrorCode::Cancelled,
                format!("{}: the shutdown wait was cancelled", self.shared.owner),
            ));
        }
        // The final stream request answered: the allowance starts now,
        // and the input closes with it.
        let live = self.transport();
        live.stop();
        let term_at = clock.now().checked_add(exit_timeout);
        if let Some(status) = lifecycle::await_exit(&live, clock, term_at, cancel)? {
            live.join();
            return Ok(status);
        }
        // A group already gone reports Ok; the reaps below, not the
        // signal call, decide the outcome.
        match live.signal(rustix::process::Signal::TERM) {
            Ok(()) | Err(_) => {}
        }
        let kill_at = term_at
            .and_then(|term| term.checked_add(lifecycle::GRACE))
            .or(term_at);
        if let Some(status) = lifecycle::await_exit(&live, clock, kill_at, cancel)? {
            live.join();
            return Ok(status);
        }
        // A group already gone reports Ok; the reap below decides the
        // outcome.
        match live.signal(rustix::process::Signal::KILL) {
            Ok(()) | Err(_) => {}
        }
        let status = live.wait_exit().map_err(|source| {
            (
                contract::ErrorCode::IoFailed,
                format!(
                    "{}: the shutdown never reaped the child: {source}",
                    self.shared.owner
                ),
            )
        })?;
        live.join();
        Ok(status)
    }

    /// Retires the generation at the process seam: pending requests end
    /// stale, and the next request mints under a new generation. Reload
    /// uses this; death uses the lifecycle, which also counts the child.
    pub(crate) fn retire(&self) {
        self.shared.table.retire();
    }

    /// The request table, for attributing calls and retiring generations.
    pub(crate) fn table(&self) -> &Arc<requests::Table> {
        &self.shared.table
    }

    /// The shared owner, for tests that mint requests, send lines and
    /// read the stores behind the seam.
    #[cfg(test)]
    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// The live pipes, for stopping, signalling and reaping the child: a
    /// clone, so a pump holds its own child while a restart swaps the
    /// live one.
    pub(crate) fn transport(&self) -> Arc<transport::Transport> {
        Arc::clone(&support::lock(&self.shared.transport))
    }

    /// The runtime, resolving generation-keyed references.
    pub(crate) fn runtime(&self) -> &Runtime {
        &self.shared.runtime
    }

    /// Dispatches the stream item behind `func`: hooks, watcher
    /// deliveries and commands, one at a time in arrival order. Resolves
    /// the reference first, so a stale generation never runs.
    pub(crate) fn dispatch_stream(
        &self,
        func: &FunctionRef,
        args: Value,
        timeout: Duration,
        state_call: Option<u64>,
        caller: &dyn contract::tool::Cancel,
    ) -> Result<Option<Value>, Error> {
        self.shared.runtime.resolve(func)?;
        match self.family_of(func) {
            // A hook delivery joins its state transaction: the run begins
            // it, success commits it, and any failure discards it, as the
            // Lua scheduler does around its hook calls. Watchers and
            // commands never join a transaction: their `state_call`, if
            // one arrived, would write into no open call. The committed
            // lines go to the session fold in Part D; the store applies
            // them here.
            Some(Family::Hook { point }) => {
                if let Some(call) = state_call {
                    self.shared.state.begin(call);
                }
                let outcome = self.send_request(
                    requests::Dispatch::stream(
                        func,
                        format!("hook {point}"),
                        args,
                        timeout,
                        state_call,
                    ),
                    caller,
                );
                match (state_call, &outcome) {
                    (Some(call), Ok(_)) => {
                        self.shared.state.take_call(call);
                    }
                    (Some(call), Err(_)) => {
                        self.shared.state.discard_call(call);
                    }
                    (None, _) => {}
                }
                outcome
            }
            Some(Family::Watcher { index }) => self.send_request(
                requests::Dispatch::stream(func, format!("watcher {index}"), args, timeout, None),
                caller,
            ),
            Some(Family::Command { name }) => self.send_request(
                requests::Dispatch::stream(func, format!("command {name}"), args, timeout, None),
                caller,
            ),
            _ => Err(Error::UnknownCallback {
                extension: self.shared.owner.clone(),
                callback: format!(
                    "`{}` (generation {}): it has since re-registered",
                    func.key, func.generation
                ),
            }),
        }
    }

    /// Dispatches the tool behind `func`: tools run as they arrive, so
    /// several can be outstanding and a long tool never holds up the
    /// stream. Resolves the reference first, so a stale generation never
    /// runs.
    pub(crate) fn dispatch_tool(
        &self,
        func: &FunctionRef,
        args: Value,
        timeout: Duration,
        caller: &dyn contract::tool::Cancel,
    ) -> Result<Option<Value>, Error> {
        self.shared.runtime.resolve(func)?;
        match self.family_of(func) {
            Some(Family::Tool { name }) => self.send_request(
                requests::Dispatch::tool(func, format!("tool {name}"), args, timeout),
                caller,
            ),
            _ => Err(Error::UnknownCallback {
                extension: self.shared.owner.clone(),
                callback: format!(
                    "`{}` (generation {}): it has since re-registered",
                    func.key, func.generation
                ),
            }),
        }
    }

    /// Sends one `dispatch` and waits for its answer, as the free
    /// `run_request` does: the stream gate first for stream items, then
    /// the line, then the reply, the deadline, the cancel or the hang
    /// guard, whichever is first.
    fn send_request(
        &self,
        dispatch: requests::Dispatch,
        caller: &dyn contract::tool::Cancel,
    ) -> Result<Option<Value>, Error> {
        requests::run_request(&self.shared, dispatch, caller)
    }

    /// The `extension_hello` line: the session's identity, why it
    /// started, its workspace, the data directories and the folded state.
    fn hello_line(&self) -> Result<Vec<u8>, serde_json::Error> {
        let payload = serde_json::to_value(support::lock(&self.shared.context).hello())?;
        Ok(envelope_line(
            &self.shared,
            "extension_hello",
            payload.as_object().cloned().unwrap_or_default(),
        ))
    }

    /// Answers one command accepted, carrying the call's return: `value`
    /// absent when the call returns nothing. A result that does not fit
    /// under the framing cap is answered `too_large` in its place.
    fn answer_accepted(
        &self,
        id: &contract::CommandId,
        result: Option<contract::events::CommandResult>,
    ) {
        let payload = serde_json::to_value(&contract::events::CommandAccepted {
            command_id: id.clone(),
            result,
        })
        .ok()
        .and_then(|payload| payload.as_object().cloned())
        .unwrap_or_default();
        let line = envelope_line(&self.shared, "command_accepted", payload);
        if line.len() > support::lock(&self.shared.transport).max_line() {
            self.answer_rejected(
                id,
                ErrorCode::TooLarge,
                "the result does not fit the 64 MiB wire cap".to_owned(),
            );
            return;
        }
        // The child may be gone: its answer dies with it, and the
        // table already settled what the answer would settle.
        match support::lock(&self.shared.transport).send(line) {
            Ok(()) | Err(_) => {}
        }
    }

    /// Answers one command rejected with its code and message.
    fn answer_rejected(&self, id: &contract::CommandId, code: ErrorCode, message: String) {
        let payload = serde_json::to_value(&contract::events::CommandRejected {
            command_id: Some(id.clone()),
            code,
            message,
        })
        .ok()
        .and_then(|payload| payload.as_object().cloned())
        .unwrap_or_default();
        let line = envelope_line(&self.shared, "command_rejected", payload);
        // The child may be gone: its answer dies with it, and the
        // table already settled what the answer would settle.
        match support::lock(&self.shared.transport).send(line) {
            Ok(()) | Err(_) => {}
        }
    }

    /// Rejects a call or reply naming a request that is no longer
    /// pending: it ran nothing and changes nothing.
    fn answer_stale(&self, id: &contract::CommandId) {
        self.answer_rejected(
            id,
            ErrorCode::StaleRequest,
            "the request is no longer pending".to_owned(),
        );
    }

    /// Sends one ordinary driver command through the `Drive` seam: the
    /// door runs it elsewhere and answers on the ack, which writes the
    /// command's answer. With no driver the session is closing, and the
    /// call raises `closing`.
    fn drive_command(
        &self,
        id: contract::CommandId,
        command: String,
        args: serde_json::Map<String, Value>,
    ) {
        let Some(driver) = self.shared.drive.clone() else {
            self.answer_rejected(
                &id,
                ErrorCode::Closing,
                "host.drive: the session is closing".to_owned(),
            );
            return;
        };
        let shared = Arc::clone(&self.shared);
        let owner = shared.owner.clone();
        let answer_id = id.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("process drive {owner}"))
            .spawn(move || {
                let ack = contract::inbox::Ack(Box::new(move |answer| match answer {
                    Ok(result) => answer_on(&shared, &answer_id, result),
                    Err(rejection) => answer_off(&shared, &answer_id, rejection),
                }));
                driver.drive(&owner, &command, args, ack);
            });
        if spawned.is_err() {
            self.answer_rejected(
                &id,
                ErrorCode::IoFailed,
                "host.drive: Fiber could not start the driver call".to_owned(),
            );
        }
    }

    /// Which registered family `func` names, for dispatching it as a
    /// stream item, a tool or a nested function.
    fn family_of(&self, func: &FunctionRef) -> Option<Family> {
        let registry = support::lock(&self.shared.registry);
        for ((point, _), reference) in registry.hook_runs() {
            if reference == func {
                return Some(Family::Hook {
                    point: point.clone(),
                });
            }
        }
        for (index, reference) in registry.watcher_runs() {
            if reference == func {
                return Some(Family::Watcher { index: *index });
            }
        }
        for (name, entry) in registry.commands() {
            if entry.run == *func {
                return Some(Family::Command { name: name.clone() });
            }
        }
        for (name, entry) in registry.tools() {
            if entry.run == *func {
                return Some(Family::Tool { name: name.clone() });
            }
            if entry.effects.as_ref() == Some(func) {
                return Some(Family::Effects { name: name.clone() });
            }
        }
        for (name, (timeout, reference)) in registry.search() {
            if reference == func {
                let _ = timeout;
                return Some(Family::Search { name: name.clone() });
            }
        }
        for (name, provider) in registry.providers() {
            for (key, reference) in &provider.runs {
                if reference == func {
                    return Some(Family::Provider {
                        name: name.clone(),
                        func: key.clone(),
                    });
                }
            }
        }
        for (name, harness) in registry.harness() {
            for (key, reference) in &harness.runs {
                if reference == func {
                    return Some(Family::Harness {
                        name: name.clone(),
                        func: key.clone(),
                    });
                }
            }
        }
        None
    }
}

/// Which registered family a reference names.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
enum Family {
    Hook { point: String },
    Watcher { index: usize },
    Command { name: String },
    Tool { name: String },
    Effects { name: String },
    Search { name: String },
    Provider { name: String, func: String },
    Harness { name: String, func: String },
}

/// The hang guard one request waits under: the callback's deadline with
/// slack for the settlement to land, or a backstop without one. This is no
/// timer call's timeout, only the bound that fails a wedged wait instead
/// of hanging it: the reply, the deadline or the cancel always ends the
/// wait first.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) fn hang_guard(
    clock: &dyn contract::clock::Clock,
    deadline: Option<std::time::Instant>,
) -> Duration {
    deadline
        .map(|end| end.saturating_duration_since(clock.now()) + Duration::from_secs(300))
        .unwrap_or_else(|| Duration::from_secs(3600))
}

/// One wire line: the envelope around `payload`, with the session's id,
/// the clock's wall time and the build's schema version.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) fn envelope_line(
    shared: &Shared,
    kind: &str,
    payload: serde_json::Map<String, Value>,
) -> Vec<u8> {
    let envelope = contract::Envelope {
        kind: kind.to_owned(),
        session_id: support::lock(&shared.context).session_id.clone(),
        ts: contract::clock::wall_ms(shared.clock.wall()),
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    };
    serde_json::to_vec(&envelope).unwrap_or_default()
}

/// Answers a driven command accepted, on the pump's thread.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) fn answer_on(
    shared: &Shared,
    id: &contract::CommandId,
    result: Option<contract::events::CommandResult>,
) {
    let payload = serde_json::to_value(&contract::events::CommandAccepted {
        command_id: id.clone(),
        result,
    })
    .ok()
    .and_then(|payload| payload.as_object().cloned())
    .unwrap_or_default();
    let line = envelope_line(shared, "command_accepted", payload);
    if line.len() > support::lock(&shared.transport).max_line() {
        answer_off(
            shared,
            id,
            contract::inbox::Rejection {
                code: ErrorCode::TooLarge,
                message: "the result does not fit the 64 MiB wire cap".to_owned(),
            },
        );
        return;
    }
    // The child may be gone: its answer dies with it, and the
    // table already settled what the answer would settle.
    match support::lock(&shared.transport).send(line) {
        Ok(()) | Err(_) => {}
    }
}

/// Answers a driven command rejected, on the pump's thread.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) fn answer_off(
    shared: &Shared,
    id: &contract::CommandId,
    rejection: contract::inbox::Rejection,
) {
    let payload = serde_json::to_value(&contract::events::CommandRejected {
        command_id: Some(id.clone()),
        code: rejection.code,
        message: rejection.message,
    })
    .ok()
    .and_then(|payload| payload.as_object().cloned())
    .unwrap_or_default();
    // The child may be gone: its answer dies with it.
    match support::lock(&shared.transport).send(envelope_line(shared, "command_rejected", payload))
    {
        Ok(()) | Err(_) => {}
    }
}
