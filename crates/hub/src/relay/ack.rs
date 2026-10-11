//! What a session's `command_accepted` and `command_rejected` lines settle:
//! the kept command each one answers, and the replayed acknowledgements the
//! relay drops.

use contract::SessionId;
use serde_json::{Map, Value};

use super::{Kept, Replayed, valid_session_id};
use crate::connection::{Hub, lock};

/// Whether `kept` still holds `id`: an acknowledgement for one it does
/// not was passed on, and is answered where it went.
pub(super) fn kept_has(kept: &Kept, id: &str) -> bool {
    lock(kept).iter().any(|(kept, _, _)| kept == id)
}

/// What settling an acknowledged command decides: either the command is
/// routed again, an accepted `rewind` starts the session it names, or an
/// accepted `subscribe` becomes the kept subscription.
pub(super) enum Settled {
    Reroute(String, Map<String, Value>),
    Rewound(SessionId),
    Subscribed(Map<String, Value>),
}

/// Settles a kept command `line` acknowledges: it is no longer kept. A
/// `closing` rejection of it returns it, to be routed again, once this
/// thread has seen the exited window, whether in this answer
/// (`crate::resume::exited`) or an earlier one (`exiting`). An accepted
/// `subscribe` returns its line, to become the kept subscription.
pub(super) fn settle(
    line: &[u8],
    kept: &Kept,
    hub: &Hub,
    session: &str,
    exiting: bool,
) -> Option<Settled> {
    let ((id, command), verdict) = {
        let mut kept = lock(kept);
        if kept.is_empty() {
            return None;
        }
        let (id, verdict) = acknowledgement(line)?;
        let at = kept.iter().position(|(kept, _, _)| *kept == id)?;
        let (_, command, _) = kept.remove(at);
        ((id, command), verdict)
    };
    // The kept lock is released before the caller takes the relays lock:
    // `route` takes relays and then kept, so the reverse order deadlocks.
    match verdict {
        Verdict::Closing
            if exiting || crate::resume::exited(&hub.home, &SessionId(session.to_owned())) =>
        {
            Some(Settled::Reroute(id, command))
        }
        Verdict::Accepted
            if command.get("command").and_then(Value::as_str) == Some("subscribe") =>
        {
            Some(Settled::Subscribed(command))
        }
        Verdict::Accepted if command.get("command").and_then(Value::as_str) == Some("rewind") => {
            rewind_next(line).map(Settled::Rewound)
        }
        Verdict::Accepted | Verdict::Rejected | Verdict::Closing => None,
    }
}

/// The session an accepted `rewind` starts: `result.new_session_id` with
/// the shape the hub mints. `None` for an acknowledgement that names none
/// or names one of another shape, which starts nothing.
fn rewind_next(line: &[u8]) -> Option<SessionId> {
    let line = serde_json::from_slice::<Value>(line).ok()?;
    if line.get("kind").and_then(Value::as_str)? != "command_accepted" {
        return None;
    }
    let next = line
        .get("payload")?
        .get("result")?
        .get("new_session_id")?
        .as_str()?;
    if !valid_session_id(next) {
        return None;
    }
    Some(SessionId(next.to_owned()))
}

/// The `command_id` of a session's `command_accepted` or
/// `command_rejected`, and what the acknowledgement settles.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Accepted,
    Rejected,
    Closing,
}

pub(crate) fn acknowledgement(line: &[u8]) -> Option<(String, Verdict)> {
    let line = serde_json::from_slice::<Value>(line).ok()?;
    let kind = line.get("kind").and_then(Value::as_str)?;
    if !matches!(kind, "command_accepted" | "command_rejected") {
        return None;
    }
    let payload = line.get("payload")?;
    let id = payload.get("command_id").and_then(Value::as_str)?;
    let verdict = if kind == "command_accepted" {
        Verdict::Accepted
    } else if payload.get("code").and_then(Value::as_str) == Some("closing") {
        Verdict::Closing
    } else {
        Verdict::Rejected
    };
    Some((id.to_owned(), verdict))
}

/// Whether `line` is a session's `command_accepted` or `command_rejected`
/// for `command_id`: tests only, since the relay thread drops replays
/// through [`muted`].
#[cfg(test)]
pub(crate) fn acknowledges(line: &[u8], command_id: &str) -> bool {
    acknowledgement(line).is_some_and(|(id, _)| id == command_id)
}

/// Whether `buf` acknowledges a subscription the hub sent again on this
/// relay: its id is dropped from the shared list once, so a transferred
/// level never leaks its acknowledgement to the client, while a later
/// transfer for the same relay still drops its own.
pub(super) fn muted(buf: &[u8], replayed: &Replayed) -> bool {
    let Some((id, _)) = acknowledgement(buf) else {
        return false;
    };
    let mut replayed = lock(replayed);
    if let Some(at) = replayed.iter().position(|muted| *muted == id) {
        replayed.remove(at);
        true
    } else {
        false
    }
}
