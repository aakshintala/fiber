//! The envelope every event line carries (`docs/events.md`, "The envelope").

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The session an event belongs to. Opaque, and unique within Fiber home.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

/// The turn an event is about. Opaque, and unique within its session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TurnId(pub String);

/// The action an event is about. Opaque, and unique within its session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActionId(pub String);

/// A durable event's position in its session log: contiguous, never reset or
/// reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub u64);

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
    /// The kind-specific body.
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
