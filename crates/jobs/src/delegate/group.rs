//! A Fiber delegate's process group, listed from its spawn while it may
//! still hold a process (`docs/delegates.md`, "Lifetime"). Every signal to
//! a delegate's group goes only to a listed group that still holds a
//! process, under this list's lock, and no id of 1 or less is ever
//! signalled: `kill(-1)` reaches every process the user owns.

use std::io;
use std::process::{Child, Command, ExitStatus};
use std::sync::{Mutex, MutexGuard, PoisonError};
#[cfg(test)]
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use rustix::process::{Pid, Signal};

/// The delegate groups that may still hold a process.
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Serializes the test that kills every group against every test with a
/// live child: `kill_every_group` reaches the whole list, so it runs
/// alone while the others share the lock. Production code never takes it.
#[cfg(test)]
static SERIAL: RwLock<()> = RwLock::new(());

/// Every signal sent, in order. Tests read it to prove what went out and
/// what did not, without racing the kernel.
#[cfg(test)]
static SENT: Mutex<Vec<(u32, Signal)>> = Mutex::new(Vec::new());

fn live() -> MutexGuard<'static, Vec<u32>> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Group 1 or 0 is never a delegate's group: `kill(-1)` reaches every
/// process the user owns, and `kill(0)` this process's own group.
pub(crate) fn refused(pgid: u32) -> bool {
    pgid <= 1
}

fn pid(pgid: u32) -> Option<Pid> {
    // Refused before conversion: no id of 1 or less ever reaches a
    // syscall, on any platform, whatever the caller probed first.
    if refused(pgid) {
        return None;
    }
    Pid::from_raw(i32::try_from(pgid).ok()?)
}

/// Whether the group still holds a process, through kill(-group, 0).
/// Refused ids read empty, so a mutant of that check sends nothing.
fn is_alive(pgid: u32) -> bool {
    pid(pgid).is_some_and(|pid| rustix::process::test_kill_process_group(pid).is_ok())
}

fn send(pgid: u32, signal: Signal) {
    #[cfg(test)]
    SENT.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((pgid, signal));
    let Some(pid) = pid(pgid) else {
        return;
    };
    match rustix::process::kill_process_group(pid, signal) {
        Ok(()) | Err(_) => {}
    }
}

/// Spawns `cmd`, the leader of its own group, and lists the group. The
/// list stays locked across the spawn, so no kill or read of the list can
/// see the child before its group is listed.
pub(crate) fn spawn(cmd: &mut Command) -> io::Result<Child> {
    let mut live = live();
    let child = cmd.spawn()?;
    live.push(child.id());
    Ok(child)
}

/// Lists `pgid`, a group spawned outside [`spawn`]. Tests use it for a
/// group that holds nothing. An id of 1 or less is never listed.
#[cfg(test)]
pub(crate) fn insert(pgid: u32) {
    if refused(pgid) {
        return;
    }
    live().push(pgid);
}

/// Whether `pgid` is still listed.
pub(crate) fn listed(pgid: u32) -> bool {
    live().contains(&pgid)
}

/// Whether `pgid` is a delegate group that still holds a process.
#[cfg(test)]
pub(crate) fn group_alive(pgid: u32) -> bool {
    !refused(pgid) && is_alive(pgid)
}

/// Sends `signal` to `pgid`: only to a listed group that still holds a
/// process, under the list's lock. True when it was sent.
pub(crate) fn signal(pgid: u32, signal: Signal) -> bool {
    if refused(pgid) {
        return false;
    }
    let live = live();
    if !live.contains(&pgid) || !is_alive(pgid) {
        return false;
    }
    send(pgid, signal);
    true
}

/// Reaps the leader with `try_wait`, only under the list's lock: the reap
/// cannot race a signal to a retired group. Called on every wake, so a
/// leader still running is left as it is and gives `None`.
/// An empty group retires in the same critical section; a group with a
/// surviving member gets SIGKILL and stays listed until it is empty.
/// Returns the leader's status, when it was reaped here.
pub(crate) fn reap_locked(child: &mut Child, pgid: u32) -> Option<ExitStatus> {
    let mut live = live();
    // Non-blocking: `None` while the leader still runs.
    let status = child.try_wait().ok().flatten();
    if !live.contains(&pgid) {
        return status;
    }
    if !is_alive(pgid) {
        live.retain(|listed| *listed != pgid);
    } else if status.is_some() {
        send(pgid, Signal::KILL);
    }
    status
}

/// Retires `pgid` once its group is empty. True when it retired it: a
/// listed group that still holds a process, or an unknown group, stays as
/// it is.
pub(crate) fn retire_if_empty(pgid: u32) -> bool {
    let mut live = live();
    if !live.contains(&pgid) || is_alive(pgid) {
        return false;
    }
    live.retain(|listed| *listed != pgid);
    true
}

/// Sends SIGKILL to every listed group still holding a process, all at
/// once, and drops the empty ones from the list. Refused ids never reach
/// the list, and probe empty through [`pid`], so every id sent here is
/// safe to signal; the send itself refuses them again through [`pid`].
pub fn kill_every_group() {
    let mut live = live();
    live.retain(|pgid| is_alive(*pgid));
    for pgid in live.iter() {
        send(*pgid, Signal::KILL);
    }
}

/// Holds the serial lock shared: other holders keep running while the
/// group killer waits. Test-only.
#[cfg(test)]
pub(crate) fn serial_shared() -> RwLockReadGuard<'static, ()> {
    SERIAL.read().unwrap_or_else(PoisonError::into_inner)
}

/// Holds the serial lock alone: every other holder is done first.
/// Test-only, for the one test that kills every group.
#[cfg(test)]
pub(crate) fn serial_exclusive() -> RwLockWriteGuard<'static, ()> {
    SERIAL.write().unwrap_or_else(PoisonError::into_inner)
}

/// Every signal sent so far, in order. Test-only: the kernel cannot say
/// what was sent to a group that is already gone.
#[cfg(test)]
pub(crate) fn sent_signals() -> Vec<(u32, Signal)> {
    SENT.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Holds the list's lock until the guard drops. Tests use it to hold the
/// lock while a leader exits, so the reap below cannot run until it is
/// released.
#[cfg(test)]
pub(crate) fn with_lock() -> MutexGuard<'static, Vec<u32>> {
    live()
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
