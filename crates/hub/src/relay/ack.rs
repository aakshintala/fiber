//! What a session's `command_accepted` and `command_rejected` lines settle:
//! the kept command each one answers, and the replayed acknowledgements the
//! relay replaces with an opening boundary at full.

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
    ack: Option<&Ack>,
    kept: &Kept,
    hub: &Hub,
    session: &str,
    exiting: bool,
) -> Option<Settled> {
    let ack = ack?;
    let (id, command) = {
        let mut kept = lock(kept);
        if kept.is_empty() {
            return None;
        }
        let at = kept.iter().position(|(kept, _, _)| *kept == ack.id)?;
        let (_, command, _) = kept.remove(at);
        (ack.id.clone(), command)
    };
    // The kept lock is released before the caller takes the relays lock:
    // `route` takes relays and then kept, so the reverse order deadlocks.
    match &ack.verdict {
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
            ack.rewound.clone().map(Settled::Rewound)
        }
        Verdict::Accepted | Verdict::Rejected | Verdict::Closing => None,
    }
}

/// The `command_id` of a session's `command_accepted` or
/// `command_rejected`, and what the acknowledgement settles.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Accepted,
    Rejected,
    Closing,
}

/// What one parse of a relayed session line settles: the acknowledged
/// command's id and verdict, and the session an accepted `rewind` starts
/// (`docs/events.md`:1308): `result.new_session_id` with the shape the hub
/// mints. `None` for an acknowledgement that names none or names one of
/// another shape, which starts nothing. `settle` still starts one only for
/// a kept `rewind`, so a `rewound` value on any other command is ignored.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Ack {
    pub(crate) id: String,
    pub(crate) verdict: Verdict,
    pub(crate) rewound: Option<SessionId>,
    pub(crate) last_seq: Option<u64>,
}

pub(crate) fn acknowledgement(line: &[u8]) -> Option<Ack> {
    let line = parse_line(line)?;
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
    let rewound = if kind == "command_accepted" {
        payload
            .get("result")
            .and_then(|result| result.get("new_session_id"))
            .and_then(Value::as_str)
            .filter(|next| valid_session_id(next))
            .map(|next| SessionId(next.to_owned()))
    } else {
        None
    };
    let last_seq = match verdict {
        Verdict::Accepted => payload
            .get("result")
            .and_then(|result| result.get("last_seq"))
            .and_then(Value::as_u64),
        Verdict::Rejected | Verdict::Closing => None,
    };
    Some(Ack {
        id: id.to_owned(),
        verdict,
        rewound,
        last_seq,
    })
}

/// One map-only typed pass over the relayed line's bytes into `Ack`, with
/// no `Value` tree. It returns `Some` exactly when `acknowledgement`
/// returns `Some`, with equal fields.
#[cfg(test)]
pub(crate) fn parse_typed(line: &[u8]) -> Option<Ack> {
    let _ = line;
    None
}

/// The one full parse of a relayed session line: every full parse in the
/// relay module and `retire.rs` goes through it, so the counted-work test
/// counts through it.
fn parse_line(line: &[u8]) -> Option<Value> {
    #[cfg(test)]
    ACK_PARSES.with(|count| count.set(count.get().saturating_add(1)));
    serde_json::from_slice(line).ok()
}

// Counts the full parses of relayed lines, on the calling thread: the
// parse's result is consumed inside the relay thread, and no client-visible
// line differs between one parse and five, so nothing outside can count it.
// Test-only; non-test builds never call it.
#[cfg(test)]
thread_local! {
    static ACK_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many full parses of relayed lines the calling thread has made
/// (`docs/testing.md`, "a test of how much work something does counts the
/// work"). The counted-work test reads it on the relay thread.
#[cfg(test)]
pub(crate) fn ack_parses() -> usize {
    ACK_PARSES.with(|count| count.get())
}

/// Whether `line` is a session's `command_accepted` or `command_rejected`
/// for `command_id`: tests only, since the relay thread drops replays
/// through [`muted`].
#[cfg(test)]
pub(crate) fn acknowledges(line: &[u8], command_id: &str) -> bool {
    acknowledgement(line).is_some_and(|ack| ack.id == command_id)
}

/// Whether `ack` is a subscription the hub sent again on this relay:
/// its id is dropped from the shared list once, so a transferred
/// level never leaks its acknowledgement to the client, while a later
/// transfer for the same relay still drops its own.
pub(super) fn muted(ack: Option<&Ack>, replayed: &Replayed) -> bool {
    let Some(ack) = ack else {
        return false;
    };
    let mut replayed = lock(replayed);
    if let Some(at) = replayed.iter().position(|muted| *muted == ack.id) {
        replayed.remove(at);
        true
    } else {
        false
    }
}
