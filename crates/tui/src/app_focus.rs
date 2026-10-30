//! Navigate mode: keyboard focus among the frame's click targets
//! (`docs/tui.md`, "Moving through the conversation"). The input box holds
//! the keyboard until Shift+Tab moves focus into the conversation; while a
//! target has focus, the input box's keys do nothing, except on home,
//! where a focused chip keeps them and typing returns to the entry bar.
//! Global keys work as from the input box.

use super::rail::Spot as RailSpot;
use super::{App, Effect, Surface};
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
    /// focus and is drawn again. A chip stays focused behind the model
    /// picker, whose swapped view carries no home targets.
    pub(crate) fn drawn(&mut self, targets: &[Target]) -> bool {
        // The inline images the frame drew queue their display fetches,
        // once each however often the frame redraws (`docs/tui.md`,
        // "Images").
        self.images_drawn(targets);
        // A renderer's failure falls back to the built-in row and names
        // itself once per extension and slot, so the frame with a new
        // notice draws again with it.
        let failed = self.drain_renderer_failures();
        self.stops = targets.to_vec();
        let resumed = self.home_drawn(targets);
        let chip_held = self.on_home()
            && self.model_picker_open()
            && matches!(
                self.focus,
                Some(TargetId::Home(spot)) if spot.is_chip()
            );
        if !resumed
            && !chip_held
            && self
                .focus
                .is_some_and(|id| !targets.iter().any(|target| target.id == id))
        {
            self.focus = None;
            self.cancel_pending_turn();
            return true;
        }
        resumed || failed
    }

    /// The attached screen's renderer failures since the last frame:
    /// one `hook_failed` notice per extension and slot not yet named,
    /// whether one was pushed. A repeated failure of a cached input
    /// finds its pair in the set and adds nothing. The surviving batch
    /// is grouped by code, so one frame raises one notice per code
    /// (`docs/errors.md`, "Notices").
    fn drain_renderer_failures(&mut self) -> bool {
        let cache = &self.screen.pages().ext.cache;
        let mut failures = cache.take_failures();
        let mut queued = cache.take_notices();
        if let Some(stashed) = self
            .items
            .open
            .as_ref()
            .and_then(|open| open.stashed.as_ref())
        {
            failures.extend(stashed.pages().ext.cache.take_failures());
            queued.extend(stashed.pages().ext.cache.take_notices());
        }
        let mut batch: Vec<contract::events::Notice> = queued
            .into_iter()
            .map(|(code, text)| contract::events::Notice {
                // The code as the log spells it, so a queued notice
                // groups with the same code the failures carry.
                code: serde_json::from_value::<contract::ErrorCode>(serde_json::Value::String(
                    code,
                ))
                .unwrap_or(contract::ErrorCode::Other("notice".to_owned())),
                message: text,
                extension: None,
            })
            .collect();
        for (slot, failure) in failures {
            let (code, identity) = match failure.kind {
                crate::ext::FailureKind::HookFailed => {
                    (contract::ErrorCode::HookFailed, slot.clone())
                }
                crate::ext::FailureKind::ExtensionFailed => {
                    (contract::ErrorCode::ExtensionFailed, String::new())
                }
            };
            if self
                .renderer_reported
                .insert((failure.extension.clone(), identity))
            {
                batch.push(contract::events::Notice {
                    code,
                    message: format!(
                        "extension {} could not draw {slot}: {}; the built-in row is drawn",
                        failure.extension, failure.message
                    ),
                    extension: Some(failure.extension),
                });
            }
        }
        if batch.is_empty() {
            return false;
        }
        for notice in contract::events::group_notices(batch, contract::events::grouped_lead) {
            self.notices
                .push(&crate::format::code(&notice.code), notice.message);
        }
        true
    }

    /// Sets where the panel and the rail are drawn.
    pub(crate) fn set_regions(&mut self, regions: Regions) {
        self.regions = regions;
    }

    /// `navigate`, Shift+Tab: focus to the newest conversation item,
    /// scrolled into view; with none, the last stop of the conversation
    /// area; with no stop, nothing changes.
    pub(super) fn navigate(&mut self) -> Effect {
        self.clear_rail_focus();
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
                // The overlay closes first: cancelling drops the pending
                // turn, and with it the notice the overlay shows, so a
                // later close would find nothing and drop the focus.
                let overlay = self.notices.close();
                self.cancel_pending_turn();
                if !overlay {
                    self.focus = None;
                }
                Some(Effect::None)
            }
            Key::Char(_) | Key::Backspace | Key::CtrlR | Key::CtrlV | Key::CtrlL | Key::CtrlF => {
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
        self.tab_to(from);
    }

    /// Tab from `from`: the next area's first stop, or the conversation's
    /// newest item back in the conversation. The rail takes no `App`
    /// focus: Tab onto it focuses the on-screen session's card, else the
    /// rail's first card, starting the rail's Tab walk, and a cardless
    /// rail walks on as if from the rail (`docs/tui.md`, "The rail").
    pub(super) fn tab_to(&mut self, from: Area) {
        let Some(area) = crate::focus::next_area(&self.stops, &self.regions, from) else {
            return;
        };
        match area {
            Area::Conversation => {
                self.navigate();
            }
            Area::Panel => {
                if let Some(first) = order(&self.stops, &self.regions, area).first() {
                    self.focus = Some(*first);
                }
            }
            Area::Rail => {
                let stops = order(&self.stops, &self.regions, Area::Rail);
                // The attached card wins even scrolled out of view: the
                // focus carries a reveal, so the settle shows it. Only a
                // session with no card falls back to the first stop.
                let key = self.attached_card_key().or_else(|| {
                    stops.iter().find_map(|id| {
                        if let TargetId::Rail(RailSpot::Card(key)) = id {
                            Some(*key)
                        } else {
                            None
                        }
                    })
                });
                match key {
                    Some(key) => {
                        self.set_rail_focus(RailSpot::Card(key));
                        self.focus = None;
                    }
                    None => self.tab_to(Area::Rail),
                }
            }
        }
    }

    /// Enter: `on_click` on `id`; a steering row also returns focus to the
    /// input box. Enter on a focused, already-open call opens its whole
    /// view instead; a click on the same row keeps the inline toggle.
    fn open_focused(&mut self, id: TargetId) -> Effect {
        if let TargetId::Line(super::Target::Call(call)) = id
            && self.call_open(call)
        {
            return self.open_call_view_inner(call);
        }
        let effect = self.on_click_inner(id);
        if matches!(id, TargetId::Steering(_)) {
            self.focus = None;
        }
        effect
    }

    /// The ✕ on an open overlay closes the one on top: the import report,
    /// the key map, the Ctrl+R panel, the notice overlay, or the search
    /// results. The notice overlay closes ahead of the results, as it
    /// takes Esc ahead of them.
    pub(super) fn close_overlay(&mut self) {
        for surface in self.top_surfaces() {
            if surface == Surface::Report {
                self.close_import_report();
                return;
            }
            if surface == Surface::Keymap {
                self.keymap_key(&Key::Esc);
                return;
            }
            if surface == Surface::ReviewerSetup {
                self.reviewer_setup_close();
                return;
            }
            if surface == Surface::Search {
                self.close_search();
                return;
            }
            if surface == Surface::Results {
                if !self.notices.close() {
                    self.close_results();
                }
                return;
            }
            if surface == Surface::Notice {
                self.notices.close();
                return;
            }
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
            | TargetId::Interrupt
            | TargetId::DropSteering(_)
            | TargetId::DismissNotice(_)
            | TargetId::MoreNotices
            | TargetId::CloseOverlay
            | TargetId::Offer(_)
            | TargetId::Panel(_)
            | TargetId::Rail(_)
            | TargetId::Form(_)
            | TargetId::Item(_)
            | TargetId::View(_) => None,
        }
    }

    /// The rows carrying conversation line `target`: a code block's code
    /// for its `copy` cells, else each row's text with trailing spaces
    /// trimmed, joined by `\n`; None when no row carries it.
    fn line_text(&self, target: super::Target) -> Option<String> {
        // An image line copies whole, never cut to its drawn width.
        if let super::Target::Image(id) = target {
            return self.image_label(id);
        }
        if matches!(target, super::Target::Copy { .. }) {
            return self
                .screen
                .pages()
                .copy_target(target)
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
            self.notices
                .push("history_loading", "Loading history…".to_owned());
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
            // The wait is over, landed or abandoned: the notice the first
            // press pushed goes with the pending turn.
            self.notices
                .clear_code("history_loading", "Loading history…");
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
