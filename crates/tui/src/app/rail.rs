//! The session rail's state: each card's number, the wall time its
//! elapsed times read, and the glue between the app and the rail
//! (`docs/tui.md`, "The rail"). Drawing lives in `crate::view::rail`.

use std::time::Instant;

use contract::events::SessionState;
use contract::{Envelope, RequestId, SessionId};
use serde_json::{Map, Value, json};

use super::{App, Effect, Link, Phase, mint};
use crate::focus::Area;
use crate::home::{Left, Row, State};
use crate::keys::{Event, Key, default_event};
use crate::keyset::Context;
use crate::stroke::{Code, Mods, Stroke};
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
    /// The focused card's row key; None while the input box has focus.
    /// Only one focus exists: while this is set, [`App::focus`] holds
    /// no rail target (`docs/tui.md`, "The rail").
    focus: Option<u64>,
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
            let mut held: Vec<usize> = self.numbers.iter().map(|(_, held)| *held).collect();
            held.sort_unstable();
            let free = held
                .iter()
                .fold(1, |next, held| if *held == next { next + 1 } else { next });
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
    pub(crate) fn rail_cards(&self) -> Option<(Vec<&Row>, Option<&str>)> {
        let home = self.home.as_ref()?;
        Some((home.sessions.cards(), home.launch_project()))
    }

    /// Sets the wall time a card's elapsed time reads, in milliseconds
    /// since the epoch. A frame reads it when it draws; nothing ticks.
    pub(crate) fn set_wall(&mut self, ms: u64) {
        self.rail_state.wall = ms;
    }

    /// Keeps the rail's numbers in step with the cards, on every settle,
    /// and scrolls a card waiting to be revealed into view once the rail
    /// is drawn, moving the least that shows all its rows. Without home
    /// there are no cards and nothing changes. A focus whose card left,
    /// whose rail shed, or whose screen went home clears first, so focus
    /// is set only while the layout draws its card
    /// (`docs/tui.md`, "The rail").
    pub(super) fn sync_rail(&mut self) {
        let held = self.rail_state.focus.is_some_and(|focus| {
            self.chrome
                .layout()
                .and_then(|layout| layout.rail)
                .is_some()
                && self
                    .home
                    .as_ref()
                    .is_some_and(|home| home.sessions.cards().iter().any(|card| card.key == focus))
        });
        if !held {
            self.rail_state.focus = None;
        }
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
            self.rail_state.scroll = scroll.min(start).max(end.saturating_sub(height));
        }
        self.rail_state.reveal = None;
    }

    /// Focuses the rail on the card with `key`, scrolling it into view:
    /// the only way rail focus is set, so the reveal stays with it.
    pub(super) fn set_rail_focus(&mut self, key: u64) {
        self.rail_state.focus = Some(key);
        self.rail_state.reveal = Some(key);
    }

    /// Drops rail focus, leaving the draft where it is.
    pub(super) fn clear_rail_focus(&mut self) {
        self.rail_state.focus = None;
    }

    /// The on-screen session's card key, when its session has a card.
    fn attached_card_key(&self) -> Option<u64> {
        let (cards, _) = self.rail_cards()?;
        let attached = self.session()?;
        cards
            .iter()
            .find(|row| &row.id == attached)
            .map(|row| row.key)
    }

    /// The on-screen session's home row key.
    fn attached_row_key(&self) -> Option<u64> {
        let attached = self.session().cloned()?;
        self.home
            .as_ref()?
            .sessions
            .row(&attached)
            .map(|row| row.key)
    }

    /// Moves rail focus one card down the drawn order, or up it,
    /// staying on the last and first cards. Only the drawn order
    /// moves, never feed order (`docs/tui.md`, "The rail").
    fn move_rail_focus(&mut self, down: bool) {
        let Some(rail) = self.chrome.layout().and_then(|layout| layout.rail) else {
            return;
        };
        let mut cards = Vec::new();
        for row in rows(self, rail.width) {
            if let Some(key) = row.start
                && cards.last() != Some(&key)
            {
                cards.push(key);
            }
        }
        let Some(focus) = self.rail_state.focus else {
            return;
        };
        let Some(at) = cards.iter().position(|key| *key == focus) else {
            return;
        };
        let next = if down {
            at.saturating_add(1).min(cards.len().saturating_sub(1))
        } else {
            at.saturating_sub(1)
        };
        let Some(next) = cards.get(next).copied() else {
            return;
        };
        if next != focus {
            self.set_rail_focus(next);
        }
    }

    /// One stroke before the bindings resolve, for the rail's keys
    /// (`docs/tui.md`, "The rail"): the `focus_rail` stroke in an empty
    /// box focuses the rail or widens it home, and plain Up, Down,
    /// Enter, Esc and Tab move a focused rail ahead of any rebound
    /// binding, with Right giving focus back. Typing goes on to the
    /// input box. None for every other stroke.
    pub(super) fn rail_stroke(&mut self, stroke: &Stroke, now: Instant) -> Option<Effect> {
        if self.key_context() != Context::Input
            || self.item_open()
            || self.notice_overlay().is_some()
            || self.home.is_none()
            || self.chrome.layout().is_none()
        {
            return None;
        }
        if self.keys().current("focus_rail").contains(stroke) {
            if !self.draft.is_empty() {
                if stroke.mods == Mods::NONE && matches!(stroke.code, Code::Left) {
                    return None;
                }
                return Some(Effect::None);
            }
            if self.rail_state.focus.is_some() {
                let Some(key) = self.attached_row_key() else {
                    return Some(self.leave());
                };
                let effect = self.leave();
                if self.on_home() {
                    self.clear_rail_focus();
                    self.focus_row(key);
                }
                return Some(effect);
            }
            if self
                .chrome
                .layout()
                .is_some_and(|layout| layout.rail.is_some())
            {
                if let Some(key) = self.attached_card_key() {
                    self.set_rail_focus(key);
                    return Some(Effect::None);
                }
                let Some(key) = self.attached_row_key() else {
                    return Some(self.leave());
                };
                let effect = self.leave();
                if self.on_home() {
                    self.focus_row(key);
                }
                return Some(effect);
            }
            return Some(self.leave());
        }
        if self.rail_state.focus.is_some() && stroke.mods == Mods::NONE {
            if matches!(
                stroke.code,
                Code::Up | Code::Down | Code::Enter | Code::Esc | Code::Tab
            ) {
                if let Some(Event::Key(key)) = default_event(stroke) {
                    return Some(self.on_key(key, now));
                }
            } else if matches!(stroke.code, Code::Right) {
                self.clear_rail_focus();
                self.edited();
                self.settle();
                return Some(Effect::None);
            }
        }
        None
    }

    /// The rail's keys (`docs/tui.md`, "Bindings"), ahead of every
    /// overlay: ⌥R hides a drawn rail and shows one that is not, and does
    /// nothing without the session screen's layout; ⌥1 to ⌥9 switch to
    /// the card with that number, and do nothing when no card has it, as
    /// without home, where no card is numbered. While the rail has focus,
    /// and nothing is on top, Up and Down move between cards, Enter
    /// switches to the focused card, Tab walks on from the rail, and Esc
    /// or Right gives focus back to the input box. `None` for every other
    /// key.
    pub(super) fn rail_key(&mut self, key: &Key) -> Option<Effect> {
        // A running `tty` job's view owns Alt+R and Alt+digits: they type
        // into the job ahead of the rail.
        if matches!(key, Key::AltR | Key::AltDigit(_)) && self.item_job_target().is_some() {
            return None;
        }
        if *key == Key::AltR {
            let Some(_) = self.chrome.layout() else {
                return Some(Effect::None);
            };
            return Some(self.toggle_rail());
        }
        if let Key::AltDigit(digit) = key {
            self.clear_rail_focus();
            let number = usize::from(*digit);
            let card = self
                .rail_state
                .numbers
                .iter()
                .find(|(_, held)| *held == number)
                .map(|(key, _)| *key);
            return Some(card.map_or(Effect::None, |key| self.switch_to(key)));
        }
        if *key == Key::AltA {
            self.clear_rail_focus();
            return None;
        }
        if self.rail_state.focus.is_none()
            || self.key_context() != Context::Input
            || self.item_open()
            || self.notice_overlay().is_some()
        {
            return None;
        }
        if *key == Key::Up {
            self.move_rail_focus(false);
            return Some(Effect::None);
        }
        if *key == Key::Down {
            self.move_rail_focus(true);
            return Some(Effect::None);
        }
        if *key == Key::Enter {
            if self.link != Link::Up || matches!(self.phase, Phase::Pending { .. }) {
                return Some(Effect::None);
            }
            let Some(key) = self.rail_state.focus else {
                return Some(Effect::None);
            };
            self.clear_rail_focus();
            return Some(self.switch_to(key));
        }
        if *key == Key::Esc {
            self.clear_rail_focus();
            return Some(Effect::None);
        }
        if *key == Key::Tab {
            self.clear_rail_focus();
            self.tab_to(Area::Rail);
            return Some(Effect::None);
        }
        None
    }

    /// Hides the rail and saves the choice as `tui.rail.shown` off, so
    /// it stays hidden across launches until shown (`docs/tui.md`, "The
    /// rail"). The caller settles.
    pub(super) fn hide_rail(&mut self) {
        self.clear_rail_focus();
        self.chrome.hide_rail();
        self.queue_save("tui.rail.shown", "false".to_owned());
    }

    /// Shows or hides the rail and saves the choice as
    /// `tui.rail.shown` (`docs/tui.md`, "The rail"): nothing without
    /// the session screen's layout; a drawn rail hides; otherwise the
    /// rail shows.
    pub(super) fn toggle_rail(&mut self) -> Effect {
        let Some(layout) = self.chrome.layout() else {
            return Effect::None;
        };
        if layout.rail.is_some() {
            self.hide_rail();
            self.settle();
            return Effect::None;
        }
        self.show_rail()
    }

    /// A person-caused show of the rail (⌥R on a hidden rail, `/rail`,
    /// a click on "N waiting", a drag of the grip past the floor): the
    /// rail shows and the choice saves as `tui.rail.shown` on, so it
    /// stays shown across launches. When the screen has no room for it
    /// beside a drawn panel, the panel hides as ⌥P hides it
    /// (`docs/tui.md`, "Shedding"). ⌥P brings the panel back, and the
    /// rail sheds again.
    pub(super) fn show_rail(&mut self) -> Effect {
        self.chrome.show_rail();
        self.queue_save("tui.rail.shown", "true".to_owned());
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
                self.notices
                    .push("stale_request", "No requests waiting.".to_owned());
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
            let code = super::home::refusal_code(payload);
            let message = super::home::refusal(payload);
            if let Some(home) = self.home.as_mut() {
                home.sessions.note(&session, message.clone());
            }
            self.notices.push(&code, message);
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
        self.choose_workspace(workspace);
        self.focus = None;
        effect
    }

    /// Switches the conversation and the panel to the card with `key`:
    /// the session on screen is left, lowered to `summary` when held at
    /// `full`, then the card's session opens as one from home does, and
    /// the card scrolls into view. The approval panel and the key map do
    /// not stop it; a crashed card's session resumes on its `subscribe`.
    /// Nothing happens with the link not up, while a `start` waits, for a
    /// key naming no row, or for the session already on screen. A switch
    /// drops rail focus: the rail shows the new session on screen.
    fn switch_to(&mut self, key: u64) -> Effect {
        self.clear_rail_focus();
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
