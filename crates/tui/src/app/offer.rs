//! The repository offer's keys, clicks and reply on the app (`docs/tui.md`,
//! "Approving what a repository ships"). The offer's state lives in
//! [`crate::offer`]; this module routes input to it and sends its answer.

use super::{App, Effect, Kind, Link, mint};
use crate::keys::{Edit, Key};
use crate::offer::{OfferKey, Row, Spot};

impl App {
    /// A key for the open offer, after the key map and the approval panel:
    /// Esc closes the notice list when it is open, else puts the offer
    /// aside. `None` while the offer is closed, and for the keys it passes
    /// on.
    pub(super) fn offer_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.offer.open() {
            return None;
        }
        if *key == Key::Esc {
            if !self.notices.close() {
                self.offer.put_aside();
            }
            return Some(Effect::None);
        }
        let height = self.conversation_height();
        match self.offer.on_key(key, self.screen.width(), height)? {
            OfferKey::Handled => Some(Effect::None),
            OfferKey::Send => Some(self.send_offer()),
        }
    }

    /// An edit for the open offer; `None` while the approval panel is open,
    /// which takes edits as its feedback, or while the offer is closed.
    pub(super) fn offer_edit(&mut self, edit: &Edit) -> Option<Effect> {
        (self.panel().is_none() && self.offer.on_edit(edit)).then_some(Effect::None)
    }

    /// A click on one of the offer's targets.
    pub(super) fn offer_click(&mut self, spot: Spot) -> Effect {
        match self.offer.click(spot) {
            OfferKey::Handled => Effect::None,
            OfferKey::Send => self.send_offer(),
        }
    }

    /// Sends the offer's answer. With the link down nothing goes out and
    /// the offer stays open.
    fn send_offer(&mut self) -> Effect {
        if self.link != Link::Up {
            return Effect::None;
        }
        let Some(session) = self.session().cloned() else {
            return Effect::None;
        };
        let id = mint();
        let Some(line) = self.offer.answer(&id, &session) else {
            return Effect::None;
        };
        self.pending.insert(id, (Kind::Reply, String::new()));
        Effect::Send(vec![line])
    }

    /// Whether the offer's view is shown.
    pub(crate) fn offer_open(&self) -> bool {
        self.offer.open()
    }

    /// The offer's rows at `width` and its top row, while it is open.
    pub(crate) fn offer_rows(&self, width: u16) -> Option<(Vec<Row>, usize)> {
        self.offer.rows(width)
    }
}

#[cfg(test)]
#[path = "offer_tests.rs"]
mod tests;
