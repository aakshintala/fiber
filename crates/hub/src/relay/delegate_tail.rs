//! Completes the tail of an accepted full subscription when a delegate exits.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use contract::{HubEventKind, SessionId};
use serde_json::{Map, Value};

use super::{ExitedFold, exited_fold};
use crate::connection::{Hub, lock};
use crate::relay::Relays;

/// What happened while the relay tried to complete an accepted full fold.
pub(super) enum DelegateTail {
    /// No accepted boundary needs replay.
    NotNeeded,
    /// The relay sent every missing line through the accepted boundary.
    Replayed,
    /// The log could not provide every required line.
    Incomplete,
    /// Writing the tail failed because the client connection ended.
    WriteFailed,
}

/// Completes the missing suffix of a delegate's accepted full fold after
/// its session socket ends. Readable contiguous lines are forwarded even
/// when a later line is missing or unreadable; an unfilled boundary then
/// selects the `stream_closed` fallback.
pub(super) fn replay_delegate_tail(
    session: &str,
    accepted_last_seq: Option<u64>,
    last_forwarded_seq: Option<u64>,
    hub: &Hub,
    writer: &Arc<Mutex<UnixStream>>,
) -> DelegateTail {
    let Some(boundary) = accepted_last_seq else {
        return DelegateTail::NotNeeded;
    };
    if last_forwarded_seq.is_some_and(|forwarded| forwarded >= boundary) {
        return DelegateTail::NotNeeded;
    }
    let session_id = SessionId(session.to_owned());
    let Some((_, dir)) = crate::recent::lookup_logged(&hub.home, &session_id) else {
        return DelegateTail::NotNeeded;
    };
    if crate::resume::delegate_job(&dir).is_none() || crate::closed::running(hub, session) {
        return DelegateTail::NotNeeded;
    }

    let ExitedFold { lines, .. } = exited_fold(&dir);
    let mut expected = last_forwarded_seq.map_or(0, |forwarded| forwarded + 1);
    let mut suffix = Vec::new();
    let mut reached_boundary = false;
    for line in &lines {
        let Some(seq) = line.seq.map(|seq| seq.0) else {
            continue;
        };
        if seq < expected {
            continue;
        }
        if seq != expected {
            break;
        }
        suffix.push(line);
        if seq == boundary {
            reached_boundary = true;
            break;
        }
        expected = seq + 1;
    }

    let mut bytes = Vec::with_capacity(suffix.len());
    for line in suffix {
        match serde_json::to_vec(line) {
            Ok(line) => bytes.push(line),
            Err(_) => {
                reached_boundary = false;
                break;
            }
        }
    }
    let mut client = lock(writer);
    for line in &bytes {
        if support::framing::write_line(&mut *client, line).is_err() {
            return DelegateTail::WriteFailed;
        }
    }
    if reached_boundary {
        DelegateTail::Replayed
    } else {
        DelegateTail::Incomplete
    }
}

/// Drops the accepted subscription and sends `stream_closed` after the
/// relay has forwarded every readable contiguous line from the tail.
pub(super) fn close_failed_delegate_tail(
    session: &str,
    hub: &Hub,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
) {
    lock(relays)
        .subscribed
        .retain(|(subscribed, _)| subscribed != session);
    let mut payload = Map::new();
    payload.insert("session_id".to_owned(), Value::String(session.to_owned()));
    crate::connection::send(writer, hub, HubEventKind::StreamClosed, payload);
}
