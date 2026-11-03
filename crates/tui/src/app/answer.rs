//! Command answers normalised before any feature or draft settlement.

use contract::SessionId;
use serde_json::{Map, Value};

use super::outbox::{Outbox, Waiting};
use crate::link::Line;

/// The link carrying the answer; payload readers keep their own rules.
pub(super) enum Source<'a> {
    Hub,
    Session(&'a SessionId),
}

/// One answer and the owner observed before any handler runs.
pub(super) struct Answer<'a> {
    pub(super) id: &'a str,
    pub(super) from: Source<'a>,
    pub(super) accepted: bool,
    pub(super) payload: &'a Map<String, Value>,
    pub(super) waiting: Option<Waiting>,
}

impl<'a> Answer<'a> {
    /// Reads only command answers with a string command id.
    pub(super) fn of(line: &'a Line, outbox: &Outbox) -> Option<Self> {
        let (kind, payload, from) = match line {
            Line::Hub(hub) => (hub.kind.as_str(), &hub.payload, Source::Hub),
            Line::Session(envelope) => (
                envelope.kind.as_str(),
                &envelope.payload,
                Source::Session(&envelope.session_id),
            ),
        };
        let accepted = match kind {
            "command_accepted" => true,
            "command_rejected" => false,
            _ => return None,
        };
        let id = payload.get("command_id")?.as_str()?;
        Some(Self {
            id,
            from,
            accepted,
            payload,
            waiting: outbox.waiting(id),
        })
    }
}

#[cfg(test)]
#[path = "answer_tests.rs"]
mod tests;
