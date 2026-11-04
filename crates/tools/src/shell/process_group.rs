//! Signalling a command's process group on a stop: SIGTERM through the
//! process-wide list's guarded helper, never a raw kill.

use rustix::process::Signal;

pub(super) enum Stop<'a> {
    Group(u32),
    Host(&'a super::hosted::Hosted),
}

pub(super) fn send_term(stop: Stop<'_>, sent: bool, seen_empty: bool) -> bool {
    if sent || seen_empty {
        return sent;
    }
    match stop {
        Stop::Group(pgid) => match support::group::signal(pgid, Signal::TERM) {
            Ok(()) | Err(_) => {}
        },
        Stop::Host(hosted) => {
            hosted.host.stop(&hosted.id);
        }
    }
    true
}
