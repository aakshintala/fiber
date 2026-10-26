//! Process lifecycle: startup, death and restart bookkeeping behind the
//! package seam (`docs/extensions.md`, "Process extensions").
//!
//! One synchronized state transition covers deaths, restart admission
//! and pending retirement: the next caller after a death starts the sole
//! restart while other callers await that same generation, and a second
//! death allows no third spawn. Hook timeout retires only its request
//! and never increments the child-death counter. Shutdown closes input
//! only after the final stream request answers, then allows the exit
//! timeout before TERM, GRACE and KILL.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use contract::ErrorCode;
use support::cancel::Cancel;
use support::lock;

use super::Shared;

/// How long the shutdown waits between TERM and KILL: a child that
/// ignores TERM gets 800ms before KILL ends it.
pub(crate) const GRACE: Duration = Duration::from_millis(800);

/// What the child is doing, under one lock so deaths, restarts and
/// retirement are a single transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunState {
    /// Spawned, waiting for `ready`.
    Starting,
    /// `ready` arrived; dispatches run.
    Running,
    /// The child died; the next dispatch restarts it once.
    Dead,
    /// A restart is in progress; other callers await it.
    Restarting,
    /// Shutting down: no new dispatches, in-flight work drains.
    Finishing,
    /// No child and no restart left; dispatches fail.
    Gone,
}

/// The lifecycle one synchronized transition owns: the run state, how
/// many child deaths were observed, how many restarts ran, and which
/// child spawn the death count belongs to. A repeated observation of
/// the same dead spawn counts once.
pub(crate) struct Lifecycle {
    state: RunState,
    deaths: u32,
    restarts: u32,
    dead_spawn: Option<u64>,
}

impl Lifecycle {
    pub(crate) fn starting() -> Self {
        Self {
            state: RunState::Starting,
            deaths: 0,
            restarts: 0,
            dead_spawn: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> RunState {
        self.state
    }

    /// How many child deaths were observed, excluding hook timeouts.
    #[cfg(test)]
    pub(crate) fn deaths(&self) -> u32 {
        self.deaths
    }

    /// How many restarts ran.
    #[cfg(test)]
    pub(crate) fn restarts(&self) -> u32 {
        self.restarts
    }
}

/// What respawns the child: its program, arguments and pipe limits, as
/// `Transport::spawn_with` takes them.
pub(crate) struct Spawn {
    pub(crate) program: std::path::PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) limits: super::transport::Limits,
}

/// Moves startup, running, death or restart into finishing for the
/// shutdown: no new dispatches past this point. A finished owner stays
/// as it is. True when the transition ran.
pub(crate) fn begin_finish(shared: &Shared) -> bool {
    let mut lifecycle = lock(&shared.lifecycle);
    match lifecycle.state {
        RunState::Starting | RunState::Running | RunState::Dead | RunState::Restarting => {
            lifecycle.state = RunState::Finishing;
            true
        }
        RunState::Finishing | RunState::Gone => false,
    }
}

/// Moves startup or restart into running on the child's `ready`: a late
/// `ready` after a terminal state wakes waiters but resurrects nothing.
/// The flip and its wake stay under the one transition, so a waiter
/// either sees both or neither.
pub(crate) fn note_ready(shared: &Shared) {
    {
        let mut lifecycle = lock(&shared.lifecycle);
        if lifecycle.state == RunState::Starting || lifecycle.state == RunState::Restarting {
            lifecycle.state = RunState::Running;
        }
    }
    shared.lifecycle_park.bump();
}

/// Makes one dispatch safe to send: running proceeds at once, the next
/// caller after a death runs the sole restart while the others await
/// the same generation, and a gone or finishing owner stops. A waiter
/// whose deadline passes or whose caller cancels leaves the restart to
/// its winner instead of disturbing it.
pub(crate) fn ensure_running(
    shared: &Shared,
    deadline: Option<Instant>,
    cancel: &dyn Cancel,
) -> Result<(), (contract::ErrorCode, String)> {
    loop {
        let winner = {
            let mut lifecycle = lock(&shared.lifecycle);
            match lifecycle.state {
                RunState::Starting | RunState::Running => return Ok(()),
                RunState::Gone | RunState::Finishing => {
                    return Err((
                        contract::ErrorCode::Closing,
                        format!("{}: the process extension is gone", shared.owner),
                    ));
                }
                RunState::Dead => {
                    lifecycle.state = RunState::Restarting;
                    lifecycle.restarts += 1;
                    true
                }
                RunState::Restarting => false,
            }
        };
        if winner {
            return restart(shared, deadline, cancel);
        }
        let seen = shared.lifecycle_park.generation();
        shared
            .lifecycle_park
            .park(shared.clock.as_ref(), deadline, seen);
        {
            let lifecycle = lock(&shared.lifecycle);
            match lifecycle.state {
                RunState::Starting | RunState::Running => return Ok(()),
                RunState::Gone | RunState::Finishing => {
                    return Err((
                        contract::ErrorCode::Closing,
                        format!("{}: the process extension is gone", shared.owner),
                    ));
                }
                RunState::Dead | RunState::Restarting => {}
            }
        }
        if cancel.is_cancelled() {
            return Err((
                contract::ErrorCode::Cancelled,
                format!("{}: the restart wait was cancelled", shared.owner),
            ));
        }
        if deadline.is_some_and(|end| shared.clock.as_ref().now() >= end) {
            return Err((
                contract::ErrorCode::Timeout,
                format!(
                    "{}: the restart did not finish before the deadline",
                    shared.owner
                ),
            ));
        }
    }
}

/// Runs the sole restart the death admitted: respawns the child, swaps
/// it in, refolds the hello from the store as it stands, re-registers
/// the owner so the dead generation resolves no longer, resets the
/// registry for the child's fresh registrations, and waits for `ready`
/// under the dispatch's own deadline. A failed respawn leaves the owner
/// gone: the one restart is spent either way, and no third spawn runs.
/// The old child is killed, reaped and joined before the swap, so no
/// orphan or pipe leaks past its generation.
fn restart(
    shared: &Shared,
    deadline: Option<Instant>,
    cancel: &dyn Cancel,
) -> Result<(), (contract::ErrorCode, String)> {
    let gone = |message: String| -> Result<(), (contract::ErrorCode, String)> {
        lock(&shared.lifecycle).state = RunState::Gone;
        shared.table.retire();
        shared.lifecycle_park.bump();
        Err((contract::ErrorCode::Closing, message))
    };
    let (workspace, program, args, limits) = {
        let context = lock(&shared.context);
        (
            context.workspace.clone(),
            shared.spawn.program.clone(),
            shared.spawn.args.clone(),
            shared.spawn.limits,
        )
    };
    let spawned = match super::transport::Transport::spawn_with(&program, &args, &workspace, limits)
    {
        Ok(spawned) => spawned,
        Err(source) => {
            return gone(format!(
                "{}: the restart could not respawn the child: {source}",
                shared.owner
            ));
        }
    };
    // One observation counts once: the new child moves to the next id,
    // so its death cannot read as the old spawn's again.
    let old = Arc::clone(&lock(&shared.transport));
    spawned.set_spawn_id(old.spawn_id() + 1);
    *lock(&shared.transport) = Arc::new(spawned);
    // The old generation ends here: its group dies even when the death
    // observation raced a live child, its zombie reaps, and its threads
    // join, so no orphan, pipe or listing leaks past the swap. Each step
    // ends on its own: the stop ends the reader, the kill ends a live
    // child, and the ended threads join.
    old.stop();
    match old.signal(rustix::process::Signal::KILL) {
        Ok(()) | Err(_) => {}
    }
    old.poll_exit();
    old.join();
    // The restart hello refolds the store as it stands: committed hook
    // writes survive the death, and nothing older does.
    let folded: serde_json::Map<String, serde_json::Value> = shared
        .state
        .keys()
        .into_iter()
        .filter_map(|key| shared.state.get(&key).map(|value| (key, value)))
        .collect();
    lock(&shared.context).refold(folded);
    // The dead generation resolves no longer: re-registering the owner
    // bumps it, and the registry resets for the child's fresh
    // registrations, which reinstall under the new generation. A failed
    // package stays failed: the reset keeps it.
    shared.runtime.register_owner(&shared.owner, None);
    lock(&shared.registry).restart();
    shared.ready.store(false, Ordering::SeqCst);
    // A match holds its scrutinee's temporaries through its arms, so the
    // hello serializes in its own statement: the context guard drops
    // before the envelope below locks the context again.
    let payload = match serde_json::to_value(lock(&shared.context).hello()) {
        Ok(hello) => hello.as_object().cloned().unwrap_or_default(),
        Err(source) => {
            return gone(format!(
                "{}: the restart hello never serialized: {source}",
                shared.owner
            ));
        }
    };
    let hello = super::envelope_line(shared, "extension_hello", payload);
    match lock(&shared.transport).send(hello) {
        Ok(()) => {}
        Err(super::transport::SendError::TooLarge) => {
            return gone(format!(
                "{}: the restart hello does not fit the 64 MiB wire cap",
                shared.owner
            ));
        }
        Err(super::transport::SendError::Gone) => {
            return gone(format!(
                "{}: the respawned child is already gone",
                shared.owner
            ));
        }
    }
    // The dispatch's own deadline bounds the new child's startup: the
    // restart waits under it, and a miss leaves the owner gone. Startup
    // never restarts, so the wait kills a wedged child itself.
    await_ready(shared, &shared.ready, deadline, cancel)
}

/// Records an observed death of `spawn`: retires the generation so late
/// calls and replies read stale, and moves the lifecycle once per spawn.
/// A repeated observation of the same spawn, a death the restart owns,
/// and any observation past finishing change nothing.
pub(crate) fn note_death(shared: &Shared, spawn: u64) {
    {
        let mut lifecycle = lock(&shared.lifecycle);
        match lifecycle.state {
            RunState::Starting | RunState::Running => {
                // The first observation always moves on: a repeated one
                // arrives under `Dead` or later, where the guard below
                // catches it.
                lifecycle.dead_spawn = Some(spawn);
                lifecycle.deaths += 1;
                lifecycle.state = if lifecycle.restarts == 0 {
                    RunState::Dead
                } else {
                    RunState::Gone
                };
            }
            RunState::Dead | RunState::Restarting => {
                if lifecycle.dead_spawn == Some(spawn) {
                    return;
                }
                lifecycle.dead_spawn = Some(spawn);
                lifecycle.deaths += 1;
                lifecycle.state = RunState::Gone;
            }
            RunState::Finishing | RunState::Gone => return,
        }
    }
    shared.table.retire();
    shared.lifecycle_park.bump();
}

/// Waits for the child's `ready`: Ok once it arrives, before `deadline`.
/// A missed deadline or a cancelled wait kills the wedged child,
/// retires its generation and leaves the extension gone: startup never
/// restarts, it leaves the extension out. Registering alone, without
/// `ready`, does not meet the deadline.
pub(crate) fn await_ready(
    shared: &Shared,
    ready: &AtomicBool,
    deadline: Option<Instant>,
    cancel: &dyn Cancel,
) -> Result<(), (ErrorCode, String)> {
    let clock = shared.clock.as_ref();
    loop {
        if ready.load(Ordering::SeqCst) {
            // A late `ready` after a terminal state answers the wait but
            // resurrects nothing: only startup and restart become running.
            let mut lifecycle = lock(&shared.lifecycle);
            if lifecycle.state == RunState::Starting || lifecycle.state == RunState::Restarting {
                lifecycle.state = RunState::Running;
            }
            return Ok(());
        }
        // A finished owner never readies: the restart already failed or
        // the shutdown already owns the child, so the wait ends without
        // killing or retiring again.
        if matches!(
            lock(&shared.lifecycle).state,
            RunState::Finishing | RunState::Gone
        ) {
            return Err((
                ErrorCode::Closing,
                format!("{}: the process extension is gone", shared.owner),
            ));
        }
        if cancel.is_cancelled() {
            kill_gone(shared);
            return Err((
                ErrorCode::Cancelled,
                format!("{}: startup was cancelled", shared.owner),
            ));
        }
        if deadline.is_some_and(|end| clock.now() >= end) {
            kill_gone(shared);
            return Err((
                ErrorCode::Timeout,
                format!(
                    "{}: the process did not send ready before the startup deadline",
                    shared.owner
                ),
            ));
        }
        let seen = shared.ready_park.generation();
        shared.ready_park.park(clock, deadline, seen);
    }
}

/// Kills the wedged child, retires its generation and leaves the
/// extension gone: every await_ready exit funnels here, so no wedged
/// child or pending host work leaks.
fn kill_gone(shared: &Shared) {
    lock(&shared.lifecycle).state = RunState::Gone;
    shared.table.retire();
    shared.lifecycle_park.bump();
    let live = Arc::clone(&lock(&shared.transport));
    live.stop();
    match live.signal(rustix::process::Signal::KILL) {
        Ok(()) | Err(_) => {}
    }
}
