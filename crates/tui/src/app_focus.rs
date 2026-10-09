//! Navigate mode: keyboard focus among the frame's click targets
//! (`docs/tui.md`, "Moving through the conversation"). The input box holds
//! the keyboard until Shift+Tab moves focus into the conversation; while a
//! target has focus, the input box's keys do nothing and global keys work
//! as from the input box.

use super::{App, Effect};
use crate::focus::{Area, Regions, area_of, order};
use crate::keys::Key;
use crate::mouse::{Target, TargetId};

impl App {
    /// The focused click target in navigate mode; None while the input box
    /// has focus.
    pub(crate) fn focused(&self) -> Option<TargetId> {
        self.focus
    }

    /// Takes a frame's click targets. `/resume` focuses the list first;
    /// then, when the focused target is not among them, focus returns to
    /// the input box and this returns true: the frame showed a stale
    /// focus and is drawn again.
    pub(crate) fn drawn(&mut self, targets: &[Target]) -> bool {
        self.stops = targets.to_vec();
        let resumed = self.home_drawn(targets);
        if self
            .focus
            .is_some_and(|id| !targets.iter().any(|target| target.id == id))
        {
            self.focus = None;
            self.cancel_pending_turn();
            return true;
        }
        resumed
    }

    /// Sets where the panel and the rail are drawn.
    pub(crate) fn set_regions(&mut self, regions: Regions) {
        self.regions = regions;
    }

    /// `navigate`, Shift+Tab: focus to the newest conversation item,
    /// scrolled into view; with none, the last stop of the conversation
    /// area; with no stop, nothing changes.
    pub(super) fn navigate(&mut self) -> Effect {
        let items = self.items();
        match items.last() {
            Some((line, id)) => {
                self.focus = Some(*id);
                self.reveal(*line);
            }
            None => {
                if let Some(id) = order(&self.stops, &self.regions, Area::Conversation).last() {
                    self.focus = Some(*id);
                }
            }
        }
        Effect::None
    }

    /// A key while navigating; None while the input box has focus, and for
    /// a global key, which the caller handles as it would from the input
    /// box.
    pub(super) fn focus_key(&mut self, key: &Key) -> Option<Effect> {
        let id = self.focus?;
        match key {
            Key::Up | Key::Char('k') => {
                self.step(false);
                Some(Effect::None)
            }
            Key::Down | Key::Char('j') => {
                self.step(true);
                Some(Effect::None)
            }
            Key::Enter => Some(self.open_focused(id)),
            Key::Char('y') => Some(match self.item_text(id) {
                Some(text) => {
                    self.copied = true;
                    Effect::Copy(text)
                }
                None => Effect::None,
            }),
            Key::CtrlG => Some(if let TargetId::Token(number) = id {
                self.open_token(number)
            } else {
                match self.item_text(id) {
                    Some(text) => Effect::Editor {
                        target: crate::editor::Target::Item,
                        text,
                    },
                    None => Effect::None,
                }
            }),
            Key::Tab => {
                self.next_area();
                Some(Effect::None)
            }
            Key::BackTab => {
                self.navigate();
                Some(Effect::None)
            }
            Key::Esc => {
                self.cancel_pending_turn();
                if !self.notices.close() {
                    self.focus = None;
                }
                Some(Effect::None)
            }
            Key::Char(_) | Key::Backspace | Key::CtrlR | Key::CtrlV | Key::CtrlF => {
                Some(Effect::None)
            }
            Key::CtrlC
            | Key::CtrlO
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::AltA
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_)
            | Key::F1 => None,
        }
    }

    /// The conversation's items in order, each with the index of its first
    /// line in [`Self::lines`]: each turn at its first line, each line
    /// target once at its first line, a turn before a line target on the
    /// same line.
    fn items(&self) -> Vec<(usize, TargetId)> {
        let mut out = Vec::new();
        for (row, _, id) in self.screen.pages().focus_items() {
            if !out.iter().any(|(_, seen)| *seen == id) {
                out.push((row, id));
            }
        }
        // The sort is stable, so a turn stays before a line target on the
        // same row.
        out.sort_by_key(|(row, _)| *row);
        out
    }

    /// ↓ (down) or ↑: when the focused item's neighbour in conversation
    /// order is off screen, focus it and scroll to it; otherwise the next
    /// stop before or after in focus order, holding at either end.
    fn step(&mut self, down: bool) {
        let Some(id) = self.focus else {
            return;
        };
        let Some(area) = area_of(&self.stops, &self.regions, id) else {
            return;
        };
        let stops = order(&self.stops, &self.regions, area);
        let items = self.items();
        if let Some(at) = items.iter().position(|(_, item)| *item == id) {
            let neighbour = if down {
                items.get(at.saturating_add(1))
            } else {
                at.checked_sub(1).and_then(|prev| items.get(prev))
            };
            if let Some((line, item)) = neighbour
                && !self.stops.iter().any(|target| target.id == *item)
            {
                self.focus = Some(*item);
                self.reveal(*line);
                return;
            }
        }
        let Some(at) = stops.iter().position(|stop| *stop == id) else {
            return;
        };
        let neighbour = if down {
            stops.get(at.saturating_add(1))
        } else {
            at.checked_sub(1).and_then(|prev| stops.get(prev))
        };
        if let Some(stop) = neighbour {
            self.focus = Some(*stop);
        }
    }

    /// Tab: the next area's first stop, or the conversation's newest item
    /// back in the conversation; nothing when no other area has a stop.
    fn next_area(&mut self) {
        let from = self
            .focus
            .and_then(|id| area_of(&self.stops, &self.regions, id))
            .unwrap_or(Area::Conversation);
        let Some(area) = crate::focus::next_area(&self.stops, &self.regions, from) else {
            return;
        };
        match area {
            Area::Conversation => {
                self.navigate();
            }
            Area::Panel | Area::Rail => {
                if let Some(first) = order(&self.stops, &self.regions, area).first() {
                    self.focus = Some(*first);
                }
            }
        }
    }

    /// Enter: `on_click` on `id`; a steering row also returns focus to the
    /// input box.
    fn open_focused(&mut self, id: TargetId) -> Effect {
        let effect = self.on_click(id);
        if matches!(id, TargetId::Steering(_)) {
            self.focus = None;
        }
        effect
    }

    /// The ✕ on an open overlay: closes the key map when open, else the
    /// notice overlay, else the search results.
    pub(super) fn close_overlay(&mut self) {
        if self.keymap_top().is_some() {
            self.keymap_key(&Key::Esc);
        } else if !self.notices.close() {
            self.close_results();
        }
    }

    /// The text y copies and Ctrl+G opens: a conversation line's rows, a
    /// code block's code, a paste token's text, a notice's whole text, a
    /// steering row's text, a link's URL; None for a control. A turn spanning dropped
    /// pages loads them first: the first press asks for them and says so,
    /// and the next press copies the whole turn.
    fn item_text(&mut self, id: TargetId) -> Option<String> {
        match id {
            TargetId::Home(spot) => self.home_text(spot),
            TargetId::Line(target) => self.line_text(target),
            TargetId::Link { .. } => self.link_text(id),
            TargetId::Token(number) => self.draft.token_text(number).map(str::to_owned),
            TargetId::Notice(id) => self.notices.text(id).map(str::to_owned),
            TargetId::FindCount | TargetId::FindResult(_) => None,
            TargetId::Steering(at) => self.steering.text(at).map(str::to_owned),
            TargetId::Turn(at) => self.turn_text(at),
            TargetId::Badge
            | TargetId::NewBelow
            | TargetId::DropSteering(_)
            | TargetId::DismissNotice(_)
            | TargetId::MoreNotices
            | TargetId::CloseOverlay
            | TargetId::Offer(_)
            | TargetId::Form(_) => None,
        }
    }

    /// The rows carrying conversation line `target`: a code block's code
    /// for its `copy` cells, else each row's text with trailing spaces
    /// trimmed, joined by `\n`; None when no row carries it.
    fn line_text(&self, target: super::Target) -> Option<String> {
        if matches!(target, super::Target::Copy { .. }) {
            return self
                .screen
                .pages()
                .copy_target(target, self.column_width())
                .map(|copy| copy.code);
        }
        let rows: Vec<String> = self
            .screen
            .pages()
            .rows()
            .into_iter()
            .filter(|(_, own)| *own == Some(target))
            .map(|(line, _)| line.to_string())
            .map(|text| text.trim_end().to_owned())
            .collect();
        (!rows.is_empty()).then(|| rows.join("\n"))
    }

    /// Turn `at`'s rows with no target of their own: the prompt bubble, a
    /// right-aligned row, trimmed at both ends, every other row trimmed
    /// at the end, joined by `\n`; None when that leaves nothing, and None
    /// with a notice while its dropped pages load.
    fn turn_text(&mut self, at: usize) -> Option<String> {
        // A new request replaces the pending one: its pages page out again.
        // A request for a turn that is gone flows through the same branch.
        if self
            .pending_turn
            .as_ref()
            .is_some_and(|pending| pending.turn != at)
        {
            self.cancel_pending_turn();
        }
        // A pending copy pins each page once: a second press pins only the
        // pages it does not hold, and the copy unpins exactly its own.
        let mut held = self
            .pending_turn
            .take()
            .map(|pending| pending.pages)
            .unwrap_or_default();
        let missing = crate::turn_text::request_turn(self.screen.pages_mut(), at, &held);
        for page in missing.iter().copied() {
            if !held.contains(&page) {
                held.push(page);
            }
        }
        self.pending_turn = Some(crate::turn_text::PendingTurn {
            turn: at,
            pages: held,
        });
        if !missing.is_empty() {
            self.notices.push("Loading history…".to_owned());
            return None;
        }
        let text = self.screen.pages().turn_text(at);
        self.cancel_pending_turn();
        text
    }

    /// Drops a whole-turn copy waiting on dropped pages, letting them page
    /// out again.
    pub(super) fn cancel_pending_turn(&mut self) {
        if let Some(pending) = self.pending_turn.take() {
            for at in pending.pages {
                self.screen.pages_mut().unwant(at);
            }
        }
    }

    /// Releases a whole-turn copy whose focus moved on, so the window
    /// bounds memory again (`docs/tui.md`, "History and paging").
    pub(super) fn settle_pending_turn(&mut self) {
        if self
            .pending_turn
            .as_ref()
            .is_some_and(|pending| self.focus != Some(TargetId::Turn(pending.turn)))
        {
            self.cancel_pending_turn();
        }
    }

    /// Scrolls so focus row `row` shows, keeping a wrapped target together
    /// when it fits and following again at the bottom.
    /// With no conversation rows there is nothing to show, and the
    /// scroll stays as it was.
    fn reveal(&mut self, row: usize) {
        let height = self.conversation_height();
        self.screen.reveal(row, height);
    }
}

#[cfg(test)]
#[path = "app_focus_tests.rs"]
mod tests;
