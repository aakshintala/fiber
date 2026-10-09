//! The session rail's state: each card's number, the wall time its
//! elapsed times read, and the glue between the app and the rail
//! (`docs/tui.md`, "The rail"). Drawing lives in `crate::view::rail`.

use super::App;
use crate::home::Row;

/// The rail's state behind a small interface.
#[derive(Debug, Default)]
pub(crate) struct RailState {
    /// Each card's number, by its row's key.
    numbers: Vec<(u64, usize)>,
    /// The wall time the loop last set, in milliseconds since the epoch.
    wall: u64,
}

impl RailState {
    /// Numbers the cards: a key no longer a card gives its number up,
    /// then each card without one, in card order, takes the smallest
    /// number from 1 no other card holds. A number never moves while its
    /// card stays, so a card that leaves leaves a gap until a later card
    /// takes it.
    pub(crate) fn sync(&mut self, cards: &[&Row]) {
        self.numbers
            .retain(|(key, _)| cards.iter().any(|card| card.key == *key));
        for card in cards {
            if self.number(card.key).is_some() {
                continue;
            }
            let free = (1..)
                .find(|number| !self.numbers.iter().any(|(_, held)| held == number))
                .unwrap_or(1);
            self.numbers.push((card.key, free));
        }
    }

    /// The number of the card whose row has `key`.
    pub(crate) fn number(&self, key: u64) -> Option<usize> {
        self.numbers
            .iter()
            .find(|(card, _)| *card == key)
            .map(|(_, number)| *number)
    }

    /// The wall time the loop last set, in milliseconds since the epoch.
    pub(crate) fn wall(&self) -> u64 {
        self.wall
    }
}

impl App {
    /// The rail's state.
    pub(crate) fn rail_state(&self) -> &RailState {
        &self.rail_state
    }

    /// The cards and the launch project, or None without home.
    pub(crate) fn rail_cards(&self) -> Option<(Vec<&Row>, &str)> {
        let home = self.home.as_ref()?;
        Some((home.sessions.cards(), home.launch.project.as_str()))
    }

    /// Sets the wall time a card's elapsed time reads, in milliseconds
    /// since the epoch. A frame reads it when it draws; nothing ticks.
    pub(crate) fn set_wall(&mut self, ms: u64) {
        self.rail_state.wall = ms;
    }

    /// Keeps the rail's numbers in step with the cards, on every settle.
    /// Without home there are no cards and nothing changes.
    pub(super) fn sync_rail(&mut self) {
        let Some(home) = self.home.as_ref() else {
            return;
        };
        self.rail_state.sync(&home.sessions.cards());
    }
}

#[cfg(test)]
#[path = "rail_tests.rs"]
mod tests;
