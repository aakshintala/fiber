//! Every command's process group that may still hold a process, listed for
//! the whole process (`docs/invocation.md`, "Shutdown"): a second SIGTERM or
//! SIGINT during a shutdown, or the shutdown's bound, kills them all at once.

use std::sync::{Mutex, MutexGuard, PoisonError};

use rustix::process::Signal;

use super::command::{group_alive, signal_group};

/// The listed groups, by id.
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn live() -> MutexGuard<'static, Vec<u32>> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Lists `pgid`, a command's group that was just spawned.
pub(super) fn register(pgid: u32) {
    live().push(pgid);
}

/// A run has finished with `pgid`: it leaves the list once the run saw the
/// group empty. A group still occupied stays listed, so a later kill still
/// reaches it.
pub(super) fn finished(pgid: u32, seen_empty: bool) {
    if seen_empty {
        live().retain(|listed| *listed != pgid);
    }
}

/// Sends SIGKILL to every listed group still holding a process, all at once,
/// and drops the empty ones from the list. A group id of 1 or less is never
/// signalled. The runs are not woken: each sees its group empty at its next
/// pass.
pub fn kill_every_group() {
    let mut live = live();
    live.retain(|pgid| group_alive(*pgid));
    for pgid in live.iter() {
        signal_group(*pgid, Signal::KILL);
    }
}

/// The groups listed now.
#[cfg(test)]
pub(super) fn listed() -> Vec<u32> {
    live().clone()
}

#[cfg(test)]
#[path = "groups_tests.rs"]
mod tests;
