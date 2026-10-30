//! What a click does to the terminal's state (`docs/tui.md`, "Mouse and
//! hover").

use super::{App, Effect, Surface, Target};
use crate::app::rail::Spot;
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::mouse::{Pointer, TargetId};
use ratatui::layout::{Position, Rect};
use std::time::Instant;

/// A wheel step over the conversation scrolls it this many rows: a
/// starting point, not a measurement (`docs/tui.md`, "Turns").
pub(super) const WHEEL_ROWS: usize = 3;

impl App {
    /// Handles one mouse report against the last frame's `targets`:
    /// every left press clears "Copied" and ends a finished attention
    /// title, then the press feeds the drag, the wheel, the selection
    /// and the pointer in order; a click's effect wins, then the wheel's,
    /// then the selection's (`docs/tui.md`, "Mouse and hover", "Getting
    /// the person's attention").
    #[cfg(test)]
    pub(crate) fn on_mouse(
        &mut self,
        mouse: &Mouse,
        targets: &[crate::mouse::Target],
        pointer: &mut Pointer,
        hover: bool,
        now: Instant,
    ) -> Effect {
        let result = self.on_mouse_inner(mouse, targets, pointer, hover, now);
        self.settle();
        result
    }

    /// Handles the route without settling; the caller owns the layout pass.
    pub(crate) fn on_mouse_inner(
        &mut self,
        mouse: &Mouse,
        targets: &[crate::mouse::Target],
        pointer: &mut Pointer,
        hover: bool,
        now: Instant,
    ) -> Effect {
        // Every left click, on a target or not, clears "Copied" and
        // ends a finished attention title (`docs/tui.md`, "Getting the
        // person's attention"); a click on `copy` sets it again.
        if mouse.kind == MouseKind::Press(Button::Left) {
            self.clear_copied();
            self.attention_seen();
            if let Some(effect) = self.outside_press(mouse, targets, now) {
                Self::disarm_press(pointer, mouse, hover);
                return effect;
            }
        }
        self.on_drag_inner(mouse);
        let wheel = self.on_wheel_inner(mouse);
        let selected = self.on_select(mouse, targets);
        let clicked = pointer.on_mouse(mouse, targets, hover);
        // A click's effect wins, then the wheel's, then the selection's.
        let effect = clicked.map_or(Effect::None, |target| self.on_click_inner(target));
        if effect != Effect::None {
            effect
        } else if wheel != Effect::None {
            wheel
        } else {
            selected
        }
    }

    // The swallowed press holds no press: feeding the
    // pointer a release at its cell clears what no click
    // may later use, through the existing API, and always
    // returns `None`.
    // Deleting `kind` from the synthetic release replays the press with
    // no hit: both clear the same `pressed`, and the `pressed_at`
    // difference is unobservable because the next real press re-pairs
    // both fields.
    // debt: synthetic Release through the public API stays; switch to a Pointer disarm only if mouse.rs gains one (outside this ticket's fence).
    #[cfg_attr(false, mutants::skip)]
    fn disarm_press(pointer: &mut Pointer, mouse: &Mouse, hover: bool) {
        let _disarmed = pointer.on_mouse(
            &Mouse {
                kind: MouseKind::Release,
                ..*mouse
            },
            &[],
            hover,
        );
    }

    /// A left press starting outside the overlay on top's frame closes
    /// that overlay through the same path as Esc and goes no further:
    /// the press reaches none of the drag, the wheel, the selection,
    /// the pointer or the control under it (`docs/tui.md`, "Bindings").
    /// The model picker's header row is exempt: its whole top row
    /// closes the picker outright, as a click there always did. `None`
    /// when the press is no outside click on a closing surface.
    fn outside_press(
        &mut self,
        mouse: &Mouse,
        targets: &[crate::mouse::Target],
        now: Instant,
    ) -> Option<Effect> {
        // debt: re-check stays as caller insurance; drop it only if outside_press stays single-caller and the caller keeps its guard.
        if !matches!(mouse.kind, MouseKind::Press(Button::Left)) {
            return None;
        }
        let top = self.top_surface();
        if !top.closes_on_outside_click() {
            return None;
        }
        let slab = match self.frames().slab_of(top) {
            Some(slab) => slab,
            // The offer's names query draws no frame: while it runs the
            // press lands nowhere rather than reaching the rows beneath.
            None if top == Surface::HooksOffer && self.hooks_offer_names().is_none() => {
                return Some(Effect::None);
            }
            None => return None,
        };
        if slab.contains(Position::new(mouse.col, mouse.row)) {
            return None;
        }
        if top == Surface::Picker
            && matches!(
                crate::mouse::hit(targets, mouse.col, mouse.row),
                Some(TargetId::View(crate::swapped::Spot::Close))
            )
        {
            return None;
        }
        // A shown selection never keeps the panel: Esc alone would
        // clear it first and leave a completion panel open, so the
        // click clears it before closing through the Esc path.
        self.clear_selection();
        Some(self.on_key_inner(Key::Esc, now))
    }

    /// Handles a click on `target` (`docs/tui.md`, "Bindings"): the badge
    /// reopens the approval queue, "↓ New messages below" jumps to the end,
    /// a conversation line opens or closes what it names, a code block's
    /// `copy` copies its code, a steering row is selected, its ✕ drops
    /// it, a notice opens whole, its ✕ dismisses it, "+N more" lists the
    /// notices, and a paste token opens in the editor as Ctrl+G on it does.
    /// A recall waiting for a page waits no more. A click clears "Copied".
    #[cfg(test)]
    pub(crate) fn on_click(&mut self, target: TargetId) -> Effect {
        let result = self.on_click_inner(target);
        self.settle();
        result
    }

    /// Handles the route without settling; the caller owns the layout pass.
    pub(crate) fn on_click_inner(&mut self, target: TargetId) -> Effect {
        // A click drops the rail's focus, except the ✕ that dismisses:
        // the dismiss keeps the focus until its hub answer arrives.
        if !matches!(target, TargetId::Rail(Spot::Dismiss(_))) {
            self.clear_rail_focus();
        }
        self.history.cancel();
        self.copied = false;
        match target {
            TargetId::Badge => self.next_request(),
            TargetId::Link { .. } => self.follow_link(target),
            TargetId::FindCount => {
                self.open_results();
                Effect::None
            }
            TargetId::FindResult(at) => {
                self.jump_to_match(at);
                Effect::None
            }
            TargetId::Line(copy @ Target::Copy { .. }) => self.copy(copy),
            // A failed login offers the login view (`docs/tui.md`, "Turns").
            TargetId::Line(Target::Login) => self.open_config_view(super::ConfigView::Login),
            // An image's line opens in the system viewer
            // (`docs/tui.md`, "Images").
            TargetId::Line(Target::Image(id)) => self.view_image(id),
            TargetId::Line(Target::CallView(id)) => {
                // A click on the more row opens the call's whole view.
                self.open_session_view(super::SessionView::Call(id))
            }
            TargetId::Line(line) => {
                // A click on the pinned row closes what it opened and
                // scrolls the closed row to the top; any other line
                // click only opens or closes.
                if let Some(pin) = self.pinned_row().filter(|pin| pin.target == line) {
                    self.open_inner(line);
                    // The covered row is gone with the closed output:
                    // the closed header keeps its focus stop, so land it
                    // on the top row.
                    self.jump_inner(pin.row);
                } else {
                    self.open_inner(line);
                }
                Effect::None
            }
            TargetId::NewBelow => {
                self.screen.follow();
                Effect::None
            }
            TargetId::Steering(at) => {
                self.select_steering(at);
                Effect::None
            }
            TargetId::DropSteering(at) => self.drop_steering(at),
            TargetId::Notice(id) => {
                self.open_notice(id);
                Effect::None
            }
            TargetId::DismissNotice(id) => {
                self.dismiss_notice(id);
                Effect::None
            }
            TargetId::MoreNotices => {
                self.open_more_notices();
                Effect::None
            }
            TargetId::CloseOverlay => {
                self.close_overlay();
                Effect::None
            }
            TargetId::Home(spot) => self.home_click(spot),
            TargetId::Offer(spot) => self.offer_click(spot),
            TargetId::Form(spot) => self.form_click(spot),
            TargetId::View(spot) if self.keys_screen_open() => self.keys_screen_click(spot),
            TargetId::View(spot) if self.model_picker_open() => self.model_picker_click(spot),
            TargetId::View(spot) if self.session_view_open() => self.session_view_click(spot),
            TargetId::View(spot) => self.config_view_click(spot),
            TargetId::Panel(spot) => self.panel_click(spot),
            TargetId::Item(spot) => self.item_click_inner(spot),
            TargetId::Rail(spot) => self.rail_click(spot),
            TargetId::Token(number) => self.open_token(number),
            TargetId::Turn(_) => Effect::None,
            // The working line's interrupt clicks like Esc: `cancel`,
            // only when busy and connected.
            TargetId::Interrupt => self.on_esc(),
        }
    }

    /// Puts the request the panel shows aside, as Esc does, so it waits on
    /// the badge. The `hover` jig's way to a badge from an events file.
    pub(crate) fn put_aside(&mut self) {
        self.queue.on_key(&Key::Esc);
        self.settle();
    }

    /// The mouse wheel scrolls what it is over (`docs/tui.md`, "Turns",
    /// "The panel", "The rail", "Layout", "Swapped views"): over the panel
    /// it scrolls only the panel, or over the Delegates card only that
    /// card; over the rail, only the rail; over a swapped view's rows it
    /// shifts that view's visible rows three a step, never the hidden
    /// conversation under it. Over the conversation's visible rows with
    /// no view over them it scrolls the conversation by [`WHEEL_ROWS`]
    /// rows, but only while a session is on screen. Over the header or
    /// below the conversation it scrolls not at all. The one send a
    /// paged view asks for comes back for the loop to send.
    #[cfg(test)]
    pub(crate) fn on_wheel(&mut self, mouse: &Mouse) -> Effect {
        let result = self.on_wheel_inner(mouse);
        self.settle();
        result
    }

    /// Handles the route without settling; the caller owns the layout pass.
    pub(crate) fn on_wheel_inner(&mut self, mouse: &Mouse) -> Effect {
        let up = match mouse.kind {
            MouseKind::WheelUp => true,
            MouseKind::WheelDown => false,
            MouseKind::Press(_) | MouseKind::Release | MouseKind::Motion | MouseKind::Drag(_) => {
                return Effect::None;
            }
        };
        let at = Position::new(mouse.col, mouse.row);
        let order = self.top_surfaces();
        let covered = |covers: &[Surface]| order.iter().any(|open| covers.contains(open));
        // The open report owns the wheel, ahead of the panel, the
        // conversation, home and the rail.
        if order.contains(&Surface::Report) {
            self.scroll_report_rows(!up, WHEEL_ROWS);
            return Effect::None;
        }
        // The reviewer setup takes the wheel and does nothing with it: the
        // conversation under the panel stays where it is.
        if order.contains(&Surface::ReviewerSetup) {
            return Effect::None;
        }
        // The hooks offer consumes the wheel while it is on top: its
        // names query draws no frame, so the frame-only guard below
        // would let the wheel reach the home list, the panel or the
        // rail.
        if self.top_surface() == Surface::HooksOffer {
            return Effect::None;
        }
        // The quit, hooks offer and clear questions and the notice
        // overlay have nothing to scroll, and their frames cover the
        // rows beneath: the wheel over a frame is theirs and moves
        // nothing.
        if [
            Surface::Quit,
            Surface::HooksOffer,
            Surface::Clear,
            Surface::Notice,
        ]
        .into_iter()
        .any(|frame| order.contains(&frame) && crate::view::frame_of(self, frame).contains(at))
        {
            return Effect::None;
        }
        if self
            .chrome()
            .layout()
            .and_then(|layout| layout.panel)
            .is_some_and(|panel| panel.contains(at))
        {
            self.wheel_panel(mouse.row, up);
        } else if let Some(effect) = self.wheel_swapped(up, at) {
            return effect;
        } else if self.session().is_some()
            && !covered(&[Surface::Keymap])
            && self.over_conversation(at)
        {
            self.scroll_conversation_inner(up);
        } else if self.on_home() && !covered(&[Surface::DeleteQuestion, Surface::WorkspacePicker]) {
            self.wheel_home_inner(up, at);
        }
        let over_rail = self
            .chrome()
            .regions()
            .rail
            .is_some_and(|rail| rail.contains(Position::new(mouse.col, mouse.row)));
        if over_rail {
            self.scroll_rail(up);
        }
        Effect::None
    }

    /// Routes a wheel to the top surface actually drawn at `at`: a swapped
    /// view's rows shift that view's visible rows three a step, Search
    /// and Completion swallow only inside their drawn slabs, the approval
    /// panel swallows inside its drawn rectangle, and Results and Offer
    /// take the wheel on their drawn rows. The wheel never reaches a
    /// surface not drawn at that cell, and never the hidden conversation
    /// under a covering view (`docs/tui.md`, "Swapped views"). `None`
    /// when no swapped, item, Offer or Results surface covers `at`, so
    /// the existing chain continues; `Some(Effect::None)` when the top
    /// drawn surface deliberately swallows the wheel.
    fn wheel_swapped(&mut self, up: bool, at: Position) -> Option<Effect> {
        // The rail keeps its owner ahead of every swapped route.
        if self
            .chrome()
            .regions()
            .rail
            .is_some_and(|rail| rail.contains(at))
        {
            return None;
        }
        // The approval panel owns its full drawn rectangle: the wheel
        // over it changes no choice and reaches neither the Offer rows
        // below it nor the conversation.
        if self
            .frames()
            .slab_of(Surface::Panel)
            .is_some_and(|slab| slab.contains(at))
        {
            return Some(Effect::None);
        }
        // Search and Completion draw after swapped views, so their slabs
        // own the wheel before anything underneath them.
        if self
            .frames()
            .slab_of(Surface::Search)
            .is_some_and(|slab| slab.contains(at))
            || self
                .frames()
                .slab_of(Surface::Completion)
                .is_some_and(|slab| slab.contains(at))
        {
            return Some(Effect::None);
        }
        // The key map replaces the swapped view over the full content
        // area and swallows there: the whole area on home, the
        // conversation column attached. Its noted slab lies inside the
        // conversation it is drawn in, so testing the slab adds only
        // equivalent mutants on equality (removed).
        if self.keymap_top().is_some() {
            // Each arm swallows alike, so no compound condition remains
            // to mutate: on home the whole screen is the map's, attached
            // its conversation column is (removed: the `home ||`
            // disjunction's arms share one outcome).
            if self.home.is_some() {
                return Some(Effect::None);
            }
            if self.over_conversation(at) {
                return Some(Effect::None);
            }
            return None;
        }
        // Attached, a swapped view takes the conversation area; on home
        // it takes the whole screen. Home is set with no session, even
        // while a covering surface makes `on_home` false.
        let home = self.home.is_some() && self.session().is_none();
        let panel_covers_home = home && self.frames().slab_of(Surface::Panel).is_some();
        let over_swapped = if panel_covers_home {
            self.over_conversation(at)
        } else {
            home || self.over_conversation(at)
        };
        if self.swapped_view_open() && over_swapped {
            // The notice overlay's slab wins over the `/keys` screen:
            // the wheel over it is swallowed, never the list under it.
            if self.keys_screen_open()
                && let Some(slab) = self.frames().slab_of(Surface::Notice)
                && slab.contains(at)
            {
                return Some(Effect::None);
            }
            let effect = if self.model_picker_open() {
                self.model_picker_wheel(up)
            } else if self.config_view_open() {
                self.config_view_wheel(up)?
            } else if self.keys_screen_open() {
                self.keys_screen_wheel(up)
            } else if self.session_view_open() {
                self.session_view_wheel(up)?
            } else {
                return None;
            };
            return Some(effect);
        }
        // The item view lies below swapped views in the draw stack. Its
        // non-scrolling header swallows the wheel; a Fiber transcript
        // scrolls only over its body. Other item states swallow it too.
        if self.item_open() {
            if !self.over_conversation(at) {
                return None;
            }
            let content_top = self.chrome().layout().map_or(0, |layout| layout.column.y);
            let header_rows =
                crate::view::item::ITEM_HEADER_ROWS.min(self.conversation_area_height());
            if usize::from(at.y.saturating_sub(content_top)) < header_rows {
                return Some(Effect::None);
            }
            if self.item_transcript() {
                self.scroll_conversation_inner(up);
            }
            return Some(Effect::None);
        }
        // Offer is drawn before Results and therefore owns the wheel
        // when both states are open; otherwise Results takes its rows.
        if self.offer_open() && self.over_conversation(at) {
            return self.offer_wheel(up);
        }
        if self.results_open() && self.over_conversation(at) {
            return self.results_wheel(up);
        }
        None
    }

    /// Whether `at` is over the conversation's visible rows: the
    /// conversation column's columns, from its top for the conversation's
    /// height, the same rows the draw keeps for the conversation
    /// (`view.rs`, `render`). An attached item's header rows count: the
    /// wheel over them is the item's, never the conversation's.
    fn over_conversation(&self, at: Position) -> bool {
        let height = self.conversation_area_height();
        let (left, width, top) = match self.chrome().layout() {
            Some(layout) => (layout.column.x, layout.column.width, layout.column.y),
            None => (0, self.screen.width(), 0),
        };
        let in_column = usize::from(at.x) >= usize::from(left)
            && usize::from(at.x).saturating_sub(usize::from(left)) < usize::from(width);
        let in_rows = usize::from(at.y) >= usize::from(top)
            && usize::from(at.y).saturating_sub(usize::from(top)) < height;
        in_column && in_rows
    }

    /// Scrolls the conversation by [`WHEEL_ROWS`] rows, as PageUp and
    /// PageDown scroll by a screen (`docs/tui.md`, "Turns"): up pauses
    /// following, down resumes it at the bottom, and past the top the next
    /// frame pages history in, as `settle` and `needs` already do for a
    /// page (`docs/tui.md`, "History and paging").
    fn scroll_conversation_inner(&mut self, up: bool) {
        let height = self.conversation_height();
        self.screen.scroll_rows(up, WHEEL_ROWS, height);
    }

    /// Scrolls home's session list by [`WHEEL_ROWS`] rows when `at` is
    /// over its rows, clamped to the rows past its window, a modal
    /// question or picker winning over the list (`docs/tui.md`,
    /// "The session list"). The focus stays where it is: the frame
    /// pulls a focused row outside the window into view.
    fn wheel_home_inner(&mut self, up: bool, at: Position) {
        let Some(screen) = self.home_screen() else {
            return;
        };
        let area = Rect::new(0, 0, self.screen.width(), self.screen.height());
        let (rect, max) = crate::view::list_window(self, &screen, area);
        if !rect.contains(at) {
            return;
        }
        let next = if up {
            self.home_list_scroll().saturating_sub(WHEEL_ROWS)
        } else {
            self.home_list_scroll().saturating_add(WHEEL_ROWS).min(max)
        };
        self.set_home_list_scroll(next);
    }
}
