//! Envelopes for events the session door sends directly.

use contract::clock::{Clock, wall_ms};
use contract::events::Event;
use contract::{Envelope, SCHEMA_VERSION, SessionId};
use serde_json::Map;

/// An envelope doors builds itself: an acknowledgement, or a control line
/// that never leaves the process.
pub(crate) fn envelope(session: &SessionId, clock: &dyn Clock, event: &Event) -> Envelope {
    Envelope {
        kind: event.kind().to_owned(),
        session_id: session.clone(),
        ts: wall_ms(clock.wall()),
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: match event.payload() {
            Ok(payload) => payload,
            Err(_) => Map::new(),
        },
    }
}
