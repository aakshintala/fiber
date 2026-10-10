//! A Fiber delegate's process group, listed from its spawn while it may
//! still hold a process (`docs/delegates.md`, "Lifetime"). The list itself
//! is `support::group`'s; this module is the delegate's extension over it:
//! every signal to a delegate's group goes only to a listed group that
//! still holds a process, under the list's lock, and the reap retires the
//! group or kills a surviving member in the same critical section. An id
//! of 1 or less is never listed, so `kill(-1)` never reaches every process
//! the user owns.

use std::process::{Child, ExitStatus};
#[cfg(test)]
use std::sync::{Mutex, PoisonError};

use rustix::process::Signal;
use support::group::Listing;

/// Every signal sent, in order. Tests read it to prove what went out and
/// what did not, without racing the kernel.
#[cfg(test)]
static SENT: Mutex<Vec<(u32, Signal)>> = Mutex::new(Vec::new());

/// Sends `signal` to `pgid` through the shared guard, which refuses an id
/// of 1 or less before any syscall: a group already gone reads as done.
fn send(pgid: u32, signal: Signal) {
    #[cfg(test)]
    SENT.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((pgid, signal));
    match support::group::signal(pgid, signal) {
        Ok(()) | Err(_) => {}
    }
}

/// Whether `pgid` is still listed. Test-only.
#[cfg(test)]
pub(crate) fn listed(pgid: u32) -> bool {
    support::group::live().contains(pgid)
}

/// Sends `signal` to `pgid`: only to a listed group that still holds a
/// process, under the list's lock. True when it was sent.
pub(crate) fn signal(pgid: u32, signal: Signal) -> bool {
    let live = support::group::live();
    if !live.contains(pgid) || !support::group::alive(pgid) {
        return false;
    }
    send(pgid, signal);
    true
}

/// Reaps the leader with `try_wait`, only under the list's lock: the reap
/// cannot race a signal to a retired group. Called on every wake, so a
/// leader still running is left as it is and gives `None`.
/// An empty group retires through its own token in the same critical
/// section; a group with a surviving member gets SIGKILL and stays listed
/// until it is empty. Without a listing there is nothing to retire or
/// kill, and the status is returned as for an unlisted group.
/// Returns the leader's status, when it was reaped here.
pub(crate) fn reap_locked(child: &mut Child, listing: &mut Option<Listing>) -> Option<ExitStatus> {
    let mut live = support::group::live();
    // Non-blocking: `None` while the leader still runs.
    let status = child.try_wait().ok().flatten();
    let pgid = match listing.as_ref() {
        Some(listing) => listing.pgid(),
        None => return status,
    };
    if !support::group::alive(pgid) {
        if let Some(listing) = listing.take() {
            live.unlist(listing);
        }
    } else if status.is_some() {
        send(pgid, Signal::KILL);
    }
    status
}

/// Retires the listing once its group is empty. True when it retired it: a
/// group that still holds a process, or no listing at all, stays as it is.
pub(crate) fn retire_if_empty(listing: &mut Option<Listing>) -> bool {
    let mut live = support::group::live();
    let pgid = match listing.as_ref() {
        Some(listing) => listing.pgid(),
        None => return false,
    };
    if support::group::alive(pgid) {
        return false;
    }
    if let Some(listing) = listing.take() {
        live.unlist(listing);
    }
    true
}

/// Every signal sent so far, in order. Test-only: the kernel cannot say
/// what was sent to a group that is already gone.
#[cfg(test)]
pub(crate) fn sent_signals() -> Vec<(u32, Signal)> {
    SENT.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
