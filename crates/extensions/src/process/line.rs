//! The sessionless Fiber-to-child line (`docs/events.md`, "The envelope"):
//! the session envelope's fields except `session_id`. Every line a run
//! outside a session exchanges, from `extension_hello` to the
//! acknowledgments and rejections, carries no session id, so no log, hub
//! connection or session reader sees them.

use serde_json::{Map, Value};

/// One line Fiber sends a process extension outside a session: `kind`,
/// `ts`, `schema_version`, the optional turn, action and sequence
/// carriers, and the kind-specific body. A key a consumer does not know
/// is ignored when reading, as for the session envelope.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProcessCallLine {
    /// The only discriminator a consumer switches on.
    pub kind: String,
    /// Milliseconds since the epoch.
    pub ts: u64,
    /// The schema version this line is written against.
    pub schema_version: u32,
    /// Present on lines about a turn, or an action in one.
    pub turn_id: Option<contract::TurnId>,
    /// Present on lines about an action.
    pub action_id: Option<contract::ActionId>,
    /// Present on durable lines only.
    pub seq: Option<contract::Seq>,
    /// The kind-specific body.
    pub payload: Map<String, Value>,
}

impl ProcessCallLine {
    /// Serializes the line, as the pipe carries it: the envelope's keys
    /// in the envelope's order, without `session_id`, and an optional
    /// carrier absent when it does not apply, never `null`.
    pub(crate) fn to_vec(&self) -> Vec<u8> {
        let mut line = Map::with_capacity(8);
        line.insert("kind".to_owned(), Value::String(self.kind.clone()));
        line.insert("ts".to_owned(), Value::from(self.ts));
        line.insert(
            "schema_version".to_owned(),
            Value::from(self.schema_version),
        );
        if let Some(turn_id) = &self.turn_id {
            line.insert(
                "turn_id".to_owned(),
                serde_json::to_value(turn_id).unwrap_or(Value::Null),
            );
        }
        if let Some(action_id) = &self.action_id {
            line.insert(
                "action_id".to_owned(),
                serde_json::to_value(action_id).unwrap_or(Value::Null),
            );
        }
        if let Some(seq) = &self.seq {
            line.insert(
                "seq".to_owned(),
                serde_json::to_value(seq).unwrap_or(Value::Null),
            );
        }
        line.insert("payload".to_owned(), Value::Object(self.payload.clone()));
        serde_json::to_vec(&line).unwrap_or_default()
    }
}
