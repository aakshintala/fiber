//! The one clipboard image read at a time: Ctrl+V's gate, and matching a
//! finished read to the draft it started for (`docs/tui.md`, "The input
//! box").
//!
//! A child of `app`, owning its own state; App calls it from one
//! `route_key` arm and one `on_image` method.

use std::sync::Arc;

use super::Effect;

/// A read running, and the draft it started for.
#[derive(Debug)]
struct Running {
    /// The read's ticket.
    ticket: u64,
    /// The draft's serial when the read started.
    serial: u64,
}

/// Ctrl+V's gate: at most one read runs, and a result lands only for the
/// running ticket while the box holds its draft.
#[derive(Debug, Default)]
pub(super) struct Paste {
    /// The next read's ticket.
    next: u64,
    /// The read running, if any.
    running: Option<Running>,
}

/// What a read's result does.
pub(super) enum Landed {
    /// The image's base64, to insert at the cursor.
    Image(Arc<str>),
    /// A notice; the draft stays as it is.
    Notice(String),
    /// Nothing: another ticket's, or a stale draft's.
    Dropped,
}

impl Paste {
    /// Ctrl+V for the draft `serial`: `ReadImage` with a fresh ticket, and
    /// nothing while one read runs.
    pub(super) fn press(&mut self, serial: u64) -> Effect {
        if self.running.is_some() {
            return Effect::None;
        }
        let ticket = self.next;
        self.next = self.next.saturating_add(1);
        self.running = Some(Running { ticket, serial });
        Effect::ReadImage(ticket)
    }

    /// A result for `ticket` while the box holds the draft `serial`: the
    /// image, the notice, or nothing dropped. Only the running ticket's
    /// result clears the gate; a result for any other ticket changes
    /// nothing.
    pub(super) fn land(
        &mut self,
        ticket: u64,
        serial: u64,
        result: Result<String, String>,
    ) -> Landed {
        let Some(running) = &self.running else {
            return Landed::Dropped;
        };
        if running.ticket != ticket {
            return Landed::Dropped;
        }
        let serial_matches = running.serial == serial;
        self.running = None;
        if !serial_matches {
            return Landed::Dropped;
        }
        match result {
            Err(notice) => Landed::Notice(notice),
            Ok(base64) => Landed::Image(Arc::from(base64)),
        }
    }
}

#[cfg(test)]
#[path = "paste_tests.rs"]
mod tests;
