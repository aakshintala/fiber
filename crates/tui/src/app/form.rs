//! Answering and declining the request the panel shows (`docs/tui.md`,
//! "Approvals and questions", "A question form").

use contract::Envelope;
use contract::events::{CommandAccepted, CommandRejected};

use super::{App, Effect, Kind, Link, mint, read, session_command};
use crate::approvals::form::Spot;
use crate::approvals::{PanelKey, Queue};
use crate::input::Draft;

impl App {
    /// Sends the shown request's answer. With the link down nothing goes
    /// out and the request stays.
    pub(super) fn answer(&mut self) -> Effect {
        self.reply(Queue::answer)
    }

    /// Declines the shown form with `reply` `declined` (`docs/tui.md`, "A
    /// question form"). With the link down nothing goes out and the form
    /// stays.
    pub(super) fn decline(&mut self) -> Effect {
        self.reply(Queue::decline)
    }

    /// A click on the shown form's `spot` (`docs/tui.md`, "A question
    /// form"): it answers or declines as the key it stands for does.
    pub(super) fn form_click(&mut self, spot: Spot) -> Effect {
        match self.queue.click(spot) {
            Some(PanelKey::Answer) => self.answer(),
            Some(PanelKey::Decline) => self.decline(),
            Some(PanelKey::Handled) | None => Effect::None,
        }
    }

    /// Sends the `reply` line `make` builds for the shown request under a
    /// new command id, pending until the hub answers it.
    fn reply(&mut self, make: fn(&mut Queue, &str) -> Option<String>) -> Effect {
        if self.link != Link::Up {
            return Effect::None;
        }
        let id = mint();
        let Some(line) = make(&mut self.queue, &id) else {
            return Effect::None;
        };
        self.pending.insert(id, (Kind::Reply, Draft::default()));
        Effect::Send(vec![line])
    }

    /// A `reply`'s acknowledgement, from any session, whether or not it is
    /// on screen: a rejection puts its request back with a notice; an
    /// accepted decline returns the `cancel` that ends its session's turn
    /// (`docs/tui.md`, "A question form"). Nothing for any other line.
    pub(super) fn reply_ack(&mut self, envelope: &Envelope) -> Vec<String> {
        let (id, rejection) = match envelope.kind.as_str() {
            "command_accepted" => {
                let Some(accepted) = read!(envelope, CommandAccepted) else {
                    return Vec::new();
                };
                (accepted.command_id.0, None)
            }
            "command_rejected" => {
                let Some((Some(id), message)) = read!(envelope, CommandRejected)
                    .map(|rejected| (rejected.command_id, rejected.message))
                else {
                    return Vec::new();
                };
                (id.0, Some(message))
            }
            _ => return Vec::new(),
        };
        if !matches!(self.pending.get(&id), Some((Kind::Reply, _))) {
            return Vec::new();
        }
        if let Some(message) = rejection {
            self.rejected(&id, message);
            return Vec::new();
        }
        self.pending.remove(&id);
        let Some(session) = self.queue.declined(&id) else {
            return Vec::new();
        };
        let cancel = mint();
        let line = session_command(&cancel, "cancel", &session, None).to_string();
        self.pending.insert(cancel, (Kind::Cancel, Draft::default()));
        vec![line]
    }
}

#[cfg(test)]
#[path = "form_tests.rs"]
mod tests;
