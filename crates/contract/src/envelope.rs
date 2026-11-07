//! The envelope every event line carries (`docs/events.md`, "The envelope").

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{ActionId, Seq, SessionId, TurnId};

/// The `schema_version` every line this build writes carries
/// (`docs/events.md`, "Versioning").
pub const SCHEMA_VERSION: u32 = 1;

/// Adapters between the typed carrier fields below and the wire, which
/// carries every `kind` as a string (`docs/events.md`, "The envelope").
/// Serializing writes [`EventKind::name`]; deserializing reads a string and
/// parses it once, keeping an unfamiliar name in `Unknown` verbatim. A
/// non-string `kind` is rejected, as a missing required field is.
mod kind_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::HubEventKind;
    use crate::events::EventKind;

    pub(super) fn serialize_session<S: Serializer>(
        kind: &EventKind,
        out: S,
    ) -> Result<S::Ok, S::Error> {
        out.serialize_str(kind.name())
    }

    pub(super) fn deserialize_session<'de, D: Deserializer<'de>>(
        input: D,
    ) -> Result<EventKind, D::Error> {
        String::deserialize(input).map(|name| EventKind::from_name(&name))
    }

    pub(super) fn serialize_hub<S: Serializer>(
        kind: &HubEventKind,
        out: S,
    ) -> Result<S::Ok, S::Error> {
        out.serialize_str(kind.name())
    }

    pub(super) fn deserialize_hub<'de, D: Deserializer<'de>>(
        input: D,
    ) -> Result<HubEventKind, D::Error> {
        String::deserialize(input).map(HubEventKind::from)
    }
}

/// One event line. The fields serialize in the order `docs/events.md` lists
/// them, and an optional field is absent when it does not apply, never
/// `null`. Fields a consumer does not know are ignored when reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// The only discriminator a consumer switches on.
    #[serde(
        serialize_with = "kind_serde::serialize_session",
        deserialize_with = "kind_serde::deserialize_session"
    )]
    pub kind: crate::events::EventKind,
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
    /// The current discriminator, as stored: assigning [`Envelope::kind`]
    /// updates what this returns.
    pub fn kind(&self) -> crate::events::EventKind {
        self.kind.clone()
    }

    /// Whether this line is durable. A line is durable if and only if it
    /// carries `seq` (`docs/events.md`, "Durable and ephemeral").
    pub fn is_durable(&self) -> bool {
        self.seq.is_some()
    }
}

/// One line the hub sends on a connection to the hub
/// (`docs/events.md`, "The envelope"): `hub_hello`, `command_accepted`
/// and `command_rejected` for a hub command. It carries `kind`, `ts`,
/// `schema_version` and `payload`, and no `session_id` in the envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HubLine {
    /// The only discriminator a consumer switches on.
    #[serde(
        serialize_with = "kind_serde::serialize_hub",
        deserialize_with = "kind_serde::deserialize_hub"
    )]
    pub kind: crate::HubEventKind,
    /// Milliseconds since the epoch.
    pub ts: u64,
    /// The schema version this line is written against.
    pub schema_version: u32,
    /// The kind-specific body.
    pub payload: Map<String, Value>,
}

impl HubLine {
    /// The current discriminator, as stored: assigning [`HubLine::kind`]
    /// updates what this returns.
    pub fn kind(&self) -> crate::HubEventKind {
        self.kind.clone()
    }
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod tests;
