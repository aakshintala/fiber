//! The session rail's state: each card's number, the wall time its
//! elapsed times read, and the glue between the app and the rail
//! (`docs/tui.md`, "The rail"). Drawing lives in `crate::view::rail`.

use contract::events::SessionState;
use contract::{Envelope, RequestId, SessionId};
use serde_json::{Map, Value, json};

use super::{App, Effect, Link, Phase, mint};
use crate::home::{Left, Row, State};
use crate::keys::Key;
use crate::view::rail::{CARD_ROWS, rows};

/// A rail item that does something when clicked (`docs/tui.md`, "The
/// rail").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spot {
    /// A card, by its row's key: switches to its session.
    Card(u64),
    /// A crashed card's ✕, by its row's key: dismisses it through the
    /// hub.
    Dismiss(u64),
    /// A project header's "+", by its first card's key: home, with that
    /// card's workspace chosen for the next `start`.
    Start(u64),
}

/// The rail's state behind a small interface.
#[derive(Debug, Default)]
pub(crate) struct RailState {
    /// Each card's number, by its row's key.
    numbers: Vec<(u64, usize)>,
    /// The wall time the loop last set, in milliseconds since the epoch.
    wall: u64,
    /// How many rows the rail has scrolled.
    scroll: usize,
    /// A card to scroll into view once the rail is drawn, by its row's
    /// key.
    reveal: Option<u64>,
    /// The `dismiss` commands waiting for their answers: command id and
    /// session.
    dismissing: Vec<(String, SessionId)>,
    /// The request to show when it arrives: the session a switch opened
    /// while it waited, and what it waits on.
    arrival: Option<(SessionId, RequestId)>,
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

    /// How many rows the rail has scrolled.
    pub(crate) fn scroll(&self) -> usize {
        self.scroll
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

    /// Keeps the rail's numbers in step with the cards, on every settle,
    /// and scrolls a card waiting to be revealed into view once the rail
    /// is drawn, moving the least that shows all its rows. Without home
    /// there are no cards and nothing changes.
    pub(super) fn sync_rail(&mut self) {
        let Some(home) = self.home.as_ref() else {
            return;
        };
        self.rail_state.sync(&home.sessions.cards());
        let Some(key) = self.rail_state.reveal else {
            return;
        };
        let Some(rail) = self.chrome.layout().and_then(|layout| layout.rail) else {
            return;
        };
        let rows = rows(self, rail.width);
        let height = usize::from(rail.height);
        let scroll = self
            .rail_state
            .scroll
            .min(rows.len().saturating_sub(height));
        if let Some(start) = rows.iter().position(|row| row.start == Some(key)) {
            let end = start.saturating_add(CARD_ROWS);
            self.rail_state.scroll = if start < scroll {
                start
            } else if end > scroll.saturating_add(height) {
                end.saturating_sub(height)
            } else {
                scroll
            };
        }
        self.rail_state.reveal = None;
    }

    /// The rail's keys (`docs/tui.md`, "Bindings"), ahead of every
    /// overlay: ⌥R hides a drawn rail and shows one that is not, and does
    /// nothing without the session screen's layout; ⌥1 to ⌥9 switch to
    /// the card with that number, and do nothing when no card has it, as
    /// without home, where no card is numbered. `None` for every other
    /// key.
    pub(super) fn rail_key(&mut self, key: &Key) -> Option<Effect> {
        if *key == Key::AltR {
            let Some(layout) = self.chrome.layout() else {
                return Some(Effect::None);
            };
            if layout.rail.is_some() {
                self.chrome.hide_rail();
                self.settle();
                return Some(Effect::None);
            }
            return Some(self.show_rail());
        }
        let Key::AltDigit(digit) = key else {
            return None;
        };
        let number = usize::from(*digit);
        let card = self
            .rail_state
            .numbers
            .iter()
            .find(|(_, held)| *held == number)
            .map(|(key, _)| *key);
        Some(card.map_or(Effect::None, |key| self.switch_to(key)))
    }

    /// ⌥R on a hidden rail, or a click on "N waiting": the rail shows,
    /// and when the screen has no room for it beside a drawn panel, the
    /// panel hides as ⌥P hides it (`docs/tui.md`, "Shedding"). ⌥P brings
    /// the panel back, and the rail sheds again.
    pub(super) fn show_rail(&mut self) -> Effect {
        self.chrome.show_rail();
        self.settle();
        if self
            .chrome
            .layout()
            .is_some_and(|layout| layout.rail.is_none() && layout.panel.is_some())
        {
            self.chrome.toggle_panel();
            self.settle();
        }
        Effect::None
    }

    /// "N waiting" on the Session card while the rail is wanted and not
    /// drawn: how many live cards wait, the one on screen left out. `None`
    /// while the rail is drawn or not wanted.
    pub(crate) fn rail_waiting(&self) -> Option<usize> {
        self.chrome.layout()?.grip?;
        let attached = self.session();
        let home = self.home.as_ref()?;
        Some(
            home.sessions
                .cards()
                .into_iter()
                .filter(|row| {
                    row.left.is_none() && row.state == State::Waiting && Some(&row.id) != attached
                })
                .count(),
        )
    }

    /// A click on a rail item.
    pub(super) fn rail_click(&mut self, spot: Spot) -> Effect {
        match spot {
            Spot::Card(key) => self.switch_to(key),
            Spot::Dismiss(key) => self.dismiss_card(key),
            Spot::Start(key) => self.start_in(key),
        }
    }

    /// ⌥A, the badge and `/approvals` (`docs/tui.md`, "Bindings"): a
    /// put-aside offer opens again, else the approval panel opens at the
    /// first request waiting. With neither, the oldest card waiting other
    /// than the one on screen is switched to, and its request opens when
    /// it arrives; with none, a notice says so.
    pub(super) fn next_request(&mut self) -> Effect {
        // The offer holds its session before its first request, so it
        // reopens first.
        if self.offer.reopen() || self.queue.open_first() {
            return Effect::None;
        }
        let attached = self.session();
        let oldest = self.home.as_ref().and_then(|home| {
            home.sessions
                .cards()
                .into_iter()
                .filter(|row| {
                    row.left.is_none() && row.state == State::Waiting && Some(&row.id) != attached
                })
                .min_by_key(|row| row.status.as_ref().map_or(u64::MAX, |status| status.since))
                .map(|row| row.key)
        });
        match oldest {
            Some(key) => self.switch_to(key),
            None => {
                self.notices.push("No requests waiting.".to_owned());
                Effect::None
            }
        }
    }

    /// A request line from any session, already folded into the queue:
    /// the request a switch waits for opens on the approval panel, once.
    /// Its resolution, folded first, has left the queue, so it ends the
    /// wait and opens nothing.
    pub(super) fn request_arrived(&mut self, envelope: &Envelope) {
        let request = envelope.payload.get("request_id").and_then(Value::as_str);
        let awaited = self
            .rail_state
            .arrival
            .as_ref()
            .is_some_and(|(session, id)| {
                *session == envelope.session_id && request == Some(id.0.as_str())
            });
        if awaited {
            self.queue
                .open_request(&envelope.session_id, request.unwrap_or_default());
            self.rail_state.arrival = None;
        }
    }

    /// The hub's answer to a `dismiss` this rail sent; `None` for any
    /// other command. An accepted dismiss drops the card, leaving a gap
    /// in the numbers; a refusal becomes the card's note and a notice.
    pub(super) fn rail_answered(
        &mut self,
        id: &str,
        accepted: bool,
        payload: &Map<String, Value>,
    ) -> Option<Vec<String>> {
        let at = self
            .rail_state
            .dismissing
            .iter()
            .position(|(sent, _)| sent == id)?;
        let (_, session) = self.rail_state.dismissing.remove(at);
        if accepted {
            if let Some(home) = self.home.as_mut() {
                home.sessions.remove(&session);
            }
        } else {
            let message = super::home::refusal(payload);
            if let Some(home) = self.home.as_mut() {
                home.sessions.note(&session, message.clone());
            }
            self.notices.push(message);
        }
        Some(Vec::new())
    }

    /// A crashed card's ✕: `dismiss` through the hub, so every client
    /// agrees (`docs/tui.md`, "The rail"). Nothing goes out with the link
    /// not up or for a card that is not crashed.
    fn dismiss_card(&mut self, key: u64) -> Effect {
        if self.link != Link::Up {
            return Effect::None;
        }
        let Some(session) = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
            .filter(|row| row.left == Some(Left::Crashed))
            .map(|row| row.id.clone())
        else {
            return Effect::None;
        };
        let id = mint();
        let line = json!({"id": id, "command": "dismiss", "args": {"session": session.0}});
        self.rail_state.dismissing.push((id, session));
        Effect::Send(vec![line.to_string()])
    }

    /// A project header's "+": home, with the workspace of the card with
    /// `key` chosen for the next `start` and focus in the input box.
    fn start_in(&mut self, key: u64) -> Effect {
        let Some(workspace) = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
            .map(|row| row.workspace.clone())
        else {
            return Effect::None;
        };
        let effect = self.leave();
        if let Some(home) = self.home.as_mut() {
            home.choose(workspace);
        }
        self.focus = None;
        effect
    }

    /// Switches the conversation and the panel to the card with `key`:
    /// the session on screen is left, lowered to `summary` when held at
    /// `full`, then the card's session opens as one from home does, and
    /// the card scrolls into view. The approval panel and the key map do
    /// not stop it; a crashed card's session resumes on its `subscribe`.
    /// Nothing happens with the link not up, while a `start` waits, for a
    /// key naming no row, or for the session already on screen.
    fn switch_to(&mut self, key: u64) -> Effect {
        if self.link != Link::Up || matches!(self.phase, Phase::Pending { .. }) {
            return Effect::None;
        }
        let Some(row) = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
        else {
            return Effect::None;
        };
        if self.session() == Some(&row.id) {
            return Effect::None;
        }
        // Opening an unreadable row gives home's notice and opens
        // nothing, so the session on screen stays.
        if row.state == State::Unreadable {
            return self.open_entry(key);
        }
        // A switch to a waiting card opens its request when it arrives.
        let arrival = row.status.as_ref().and_then(|status| {
            if let SessionState::Waiting { waiting } = &status.state {
                Some((row.id.clone(), waiting.request_id.clone()))
            } else {
                None
            }
        });
        let mut lines = sent(self.leave());
        lines.extend(sent(self.open_entry(key)));
        self.rail_state.reveal = Some(key);
        self.rail_state.arrival = arrival;
        Effect::Send(lines)
    }

    /// Scrolls the rail by one card: the stored offset clamps to the rows
    /// past the rail's height first, so a grown screen moves on the first
    /// wheel; down goes to the next card's top, up to the previous one's,
    /// both clamped to that end.
    pub(super) fn scroll_rail(&mut self, up: bool) {
        let Some(rail) = self.chrome.layout().and_then(|layout| layout.rail) else {
            return;
        };
        let rows = rows(self, rail.width);
        let max = rows.len().saturating_sub(usize::from(rail.height));
        let clamped = self.rail_state.scroll.min(max);
        let mut starts = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.start.is_some())
            .map(|(at, _)| at);
        let next = if up {
            starts.rfind(|at| *at < clamped).unwrap_or(0)
        } else {
            starts.find(|at| *at > clamped).unwrap_or(max)
        };
        self.rail_state.scroll = next.min(max);
    }
}

/// The lines an effect sends; none for any other effect.
fn sent(effect: Effect) -> Vec<String> {
    if let Effect::Send(lines) = effect {
        lines
    } else {
        Vec::new()
    }
}

#[cfg(test)]
#[path = "rail_tests.rs"]
mod tests;
