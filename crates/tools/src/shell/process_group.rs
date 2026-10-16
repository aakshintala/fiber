//! Signalling a command's process group on a stop: SIGTERM through the
//! process-wide list's guarded helper, never a raw kill.

use rustix::process::Signal;

pub(super) fn send_term(pgid: u32, sent: bool, seen_empty: bool) -> bool {
    if sent || seen_empty {
        return sent;
    }
    match support::group::signal(pgid, Signal::TERM) {
        Ok(()) | Err(_) => {}
    }
    true
}
