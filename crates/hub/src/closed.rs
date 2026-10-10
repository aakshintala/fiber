//! A running session closing the relayed connection
//! (`docs/invocation.md`, "What the hub speaks"): "When a running session
//! closes the connection the hub relays for a client, the hub sends that
//! client `stream_closed` (`docs/events.md`) after every line the session
//! sent on it, and keeps no subscription for that session: the client's
//! next command for it is rejected `not_subscribed` until it subscribes
//! again. The connection's other sessions are untouched."
//!
//! A relay thread that reads end of file from its session treats it as the
//! session closing the connection while running only when the relay never
//! saw the exited window, the session's log ends in neither `fiber_exited`
//! nor `rewound`, and `run/<session_id>` still accepts a connection. A
//! clean exit unlinks the socket before shutting connections, so the probe
//! is refused; a crash leaves a socket file that refuses; a rewind leaves
//! a log ending `rewound`; only a session that keeps running while shutting
//! one connection still accepts. By the time the client reads
//! `stream_closed` for a session, the connection keeps no subscription for
//! it: the drop runs before the send, both before the queue is passed on,
//! so a racing command is answered after the line and never carries a
//! replayed `subscribe`.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use contract::SessionId;
use serde_json::{Map, Value};

use crate::connection::{Hub, lock};
use crate::relay::Relays;

/// The relay's end: when `closed_by_session` (`ended && !exiting`), a
/// running session that closed the connection gets `stream_closed` and
/// loses the kept level first; otherwise nothing changes. The probe, the
/// log reads, the seam and the client write all run with no relays lock
/// held; only the drop holds it.
pub(crate) fn on_end(
    session: &str,
    closed_by_session: bool,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
) {
    if !closed_by_session {
        return;
    }
    // Peek without blocking: a routed command may hold the relays lock
    // while it waits for this thread's end, so waiting here would
    // deadlock; when the lock is held the holder releases it without
    // waiting on this thread, so the probe below runs first. A disconnect
    // mid-probe only makes the drop and the line unobservable.
    if relays.try_lock().is_ok_and(|held| held.rejoin.is_closed()) {
        return;
    }
    if !running(hub, session) {
        return;
    }
    #[cfg(test)]
    {
        let at_close = lock(relays).at_close.take();
        if let Some(at_close) = at_close {
            at_close();
        }
    }
    lock(relays).subscribed.retain(|(kept, _)| kept != session);
    crate::connection::send(writer, hub, "stream_closed", payload(session));
}

/// Whether `session` looks like a running session: its log ends in
/// neither `fiber_exited` nor `rewound`, and `run/<session>` accepts a
/// connection, dropped at once without a line. File checks first, so no
/// connection is opened for an exited or rewound session.
fn running(hub: &Hub, session: &str) -> bool {
    let id = SessionId(session.to_owned());
    !crate::resume::exited(&hub.home, &id)
        && crate::rewind::continued(&hub.home, &id).is_none()
        && UnixStream::connect(hub.home.join("run").join(session)).is_ok()
}

/// The `stream_closed` payload: the session, and nothing else.
fn payload(session: &str) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert("session_id".to_owned(), Value::String(session.to_owned()));
    payload
}

#[cfg(test)]
#[path = "closed_tests.rs"]
mod tests;
