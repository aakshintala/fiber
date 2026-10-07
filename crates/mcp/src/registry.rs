//! The process-wide MCP server registry: child pids, shutdown state and the
//! stop-everything entry points (`docs/mcp.md`, "Starting servers").

use std::sync::{Mutex, MutexGuard, PoisonError, Weak};

use contract::clock::Wake;
use contract::tool::Cancel;
use rustix::process::{Pid, Signal};

/// Every server child's pid, from its spawn until its reap: what
/// [`kill_every_server`] reaches.
pub(crate) static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Stops every server still starting, and every start after it: the flag is
/// sticky for the life of the process. It sets the flag and wakes the parked
/// handshakes; the stop and reap run on each start's own thread. Idempotent.
pub fn stop_every_start() {
    *lock(&STOPPED) = true;
    // Dropped before any wake: a wake takes the shared lock.
    let waiting = std::mem::take(&mut *lock(&WAITING));
    for waker in waiting {
        if let Some(waker) = waker.upgrade() {
            waker.wake();
        }
    }
}

static STOPPED: Mutex<bool> = Mutex::new(false);
static WAITING: Mutex<Vec<Weak<dyn Wake>>> = Mutex::new(Vec::new());

/// The handshake's cancel, fired by [`stop_every_start`].
pub(crate) struct Stopping;

impl Cancel for Stopping {
    fn is_cancelled(&self) -> bool {
        *lock(&STOPPED)
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        let mut waiting = lock(&WAITING);
        // A finished start drops its bridge and leaves a dead entry: pruned
        // here, so the list holds only the starts still running.
        waiting.retain(|listed| listed.upgrade().is_some());
        waiting.push(waker);
    }
}

/// Sends SIGKILL to every server child not yet reaped, at once
/// (`docs/invocation.md`, "Shutdown": the bound). A pid of 1 or less is
/// never signalled.
pub fn kill_every_server() {
    // Held while `kill` runs: a reap unlists its pid under this lock before
    // it waits, so every pid signalled here is still unreaped and cannot
    // have been reused.
    let live = lock(&LIVE);
    before_signal();
    signal(&live, Signal::KILL);
}

/// Runs while a signaller holds the lock that keeps its pids unreaped, just
/// before `kill`: the seam a test pauses on to force a reap against it.
#[cfg(test)]
pub(crate) fn before_signal() {
    crate::server::tests::before_signal();
}

#[cfg(not(test))]
pub(crate) fn before_signal() {}

/// Runs as a reap is about to take `which` lock (`child` or `live`): the
/// seam a test waits on to know the reap contends before it asserts.
#[cfg(test)]
pub(crate) fn before_lock(which: &'static str) {
    crate::server::tests::before_lock(which);
}

#[cfg(not(test))]
pub(crate) fn before_lock(_which: &'static str) {}

/// Sends `signal` to each of `pids`, leaving out every id [`refused`]
/// names.
pub(crate) fn signal(pids: &[u32], signal: Signal) {
    for pid in pids.iter().filter(|pid| !refused(**pid)) {
        if let Some(pid) = i32::try_from(*pid).ok().and_then(Pid::from_raw) {
            // A server already gone refuses the signal; its reap still runs.
            match rustix::process::kill_process(pid, signal) {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

/// A pid of 1 or less is never a server's: `kill(-1)` reaches every
/// process the user owns, and `kill(0)` this process's own group. Tested
/// as a function, so a mutant of it signals nothing.
fn refused(pid: u32) -> bool {
    pid <= 1
}

pub(crate) fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
