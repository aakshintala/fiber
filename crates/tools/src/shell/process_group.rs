//! Signalling a command's process group and refusing unsafe group ids.

use rustix::process::{Pid, Signal};

pub(super) fn send_term(pgid: u32, sent: bool, seen_empty: bool) -> bool {
    if suppress_term(sent, seen_empty) {
        return sent;
    }
    signal_group(pgid, Signal::TERM);
    true
}

/// Do not signal a group that was already signalled, or one seen empty.
pub(super) fn suppress_term(sent: bool, seen_empty: bool) -> bool {
    sent || seen_empty
}

pub(super) fn group_alive(pgid: u32) -> bool {
    // Waiting on group 1 or 0 would loop: nothing of this run is there.
    if refused_group(pgid) {
        return false;
    }
    pid(pgid).is_some_and(|pid| rustix::process::test_kill_process_group(pid).is_ok())
}

pub(super) fn signal_group(pgid: u32, signal: Signal) {
    if refused_group(pgid) {
        return;
    }
    let Some(pid) = pid(pgid) else {
        return;
    };
    match rustix::process::kill_process_group(pid, signal) {
        Ok(()) | Err(_) => {}
    }
}

/// Group 1 or 0 is not a command's group: `kill(-1)` reaches every process
/// the user owns, and `kill(0)` this process's own group. No test passes such
/// an id to [`signal_group`], so a mutant of this check sends nothing; its own
/// test pins it.
pub(super) fn refused_group(pgid: u32) -> bool {
    pgid <= 1
}

fn pid(raw: u32) -> Option<Pid> {
    Pid::from_raw(i32::try_from(raw).ok()?)
}
