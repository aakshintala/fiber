//! The envelope every event line carries (`docs/events.md`, "The envelope").

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{ActionId, Seq, SessionId, TurnId};

/// The `schema_version` every line this build writes carries
/// (`docs/events.md`, "Versioning").
pub const SCHEMA_VERSION: u32 = 1;

/// One event line. The fields serialize in the order `docs/events.md` lists
/// them, and an optional field is absent when it does not apply, never
/// `null`. Fields a consumer does not know are ignored when reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// The only discriminator a consumer switches on.
    pub kind: String,
    /// The session this event belongs to.
    pub session_id: SessionId,
    /// Milliseconds since the epoch.
    pub ts: u64,
    /// The schema version this line is written against.
    pub schema_version: u32,
    /// Present on lines about a turn, or an action in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    /// Present on lines about an action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<ActionId>,
    /// Present on durable lines only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<Seq>,
    /// The kind-specific body. Its keys serialize sorted, because serde_json's
    /// `preserve_order` is never enabled (`docs/prompt-cache.md`).
    pub payload: Map<String, Value>,
}

impl Envelope {
    /// Whether this line is durable. A line is durable if and only if it
    /// carries `seq` (`docs/events.md`, "Durable and ephemeral").
    pub fn is_durable(&self) -> bool {
        self.seq.is_some()
    }
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod tests;
