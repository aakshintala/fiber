//! An emitter of ephemeral events (`docs/architecture.md`, "Streaming").
//! Any thread may hold one. Durable events stay the loop's to write, so
//! [`Emit::emit`] refuses them.

use crate::events::Event;

/// Sends ephemeral events to whoever is watching. A durable event is not
/// written: `seq` stays minted in one place.
pub trait Emit: Send + Sync {
    /// Emits `event` when it is ephemeral. A durable event is ignored, and
    /// nothing is written for it.
    fn emit(&self, event: &Event);
}
