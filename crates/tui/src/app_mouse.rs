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
    /// and the pointer in order; a click's effect wins over the
    /// selection's (`docs/tui.md`, "Mouse and hover", "Getting the
    /// person's attention").
    pub(crate) fn on_mouse(
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
        self.on_drag(mouse);
        self.on_wheel(mouse);
        let selected = self.on_select(mouse, targets);
        let clicked = pointer.on_mouse(mouse, targets, hover);
        // A click's effect, else the selection's.
        let effect = clicked.map_or(Effect::None, |target| self.on_click(target));
        if effect == Effect::None {
            selected
        } else {
            effect
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
        let slab = self.frames().slab_of(top)?;
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
        Some(self.on_key(Key::Esc, now))
    }

    /// Handles a click on `target` (`docs/tui.md`, "Bindings"): the badge
    /// reopens the approval queue, "↓ New messages below" jumps to the end,
    /// a conversation line opens or closes what it names, a code block's
    /// `copy` copies its code, a steering row is selected, its ✕ drops
    /// it, a notice opens whole, its ✕ dismisses it, "+N more" lists the
    /// notices, and a paste token opens in the editor as Ctrl+G on it does.
    /// A recall waiting for a page waits no more. A click clears "Copied".
    pub(crate) fn on_click(&mut self, target: TargetId) -> Effect {
        // A click drops the rail's focus, except the ✕ that dismisses:
        // the dismiss keeps the focus until its hub answer arrives.
        if !matches!(target, TargetId::Rail(Spot::Dismiss(_))) {
            self.clear_rail_focus();
        }
        self.history.cancel();
        self.copied = false;
        let effect = match target {
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
                    self.open(line);
                    // The covered row is gone with the closed output:
                    // the closed header keeps its focus stop, so land it
                    // on the top row.
                    self.jump(pin.row);
                } else {
                    self.open(line);
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
            TargetId::Item(spot) => self.item_click(spot),
            TargetId::Rail(spot) => self.rail_click(spot),
            TargetId::Token(number) => self.open_token(number),
            TargetId::Turn(_) => Effect::None,
            // The working line's interrupt clicks like Esc: `cancel`,
            // only when busy and connected.
            TargetId::Interrupt => self.on_esc(),
        };
        self.settle();
        effect
    }

    /// Puts the request the panel shows aside, as Esc does, so it waits on
    /// the badge. The `hover` jig's way to a badge from an events file.
    pub(crate) fn put_aside(&mut self) {
        self.queue.on_key(&Key::Esc);
        self.settle();
    }

    /// The mouse wheel scrolls what it is over (`docs/tui.md`, "Turns",
    /// "The panel", "The rail", "Layout"): over the panel it scrolls only
    /// the panel, or over the Delegates card only that card; over the rail,
    /// only the rail; over the conversation's visible rows it scrolls the
    /// conversation by [`WHEEL_ROWS`] rows, but only while a session is on
    /// screen and no swapped view covers the conversation. Over the header
    /// or below the conversation it scrolls the conversation not at all.
    pub(crate) fn on_wheel(&mut self, mouse: &Mouse) {
        let up = match mouse.kind {
            MouseKind::WheelUp => true,
            MouseKind::WheelDown => false,
            MouseKind::Press(_) | MouseKind::Release | MouseKind::Motion | MouseKind::Drag(_) => {
                return;
            }
        };
        let at = Position::new(mouse.col, mouse.row);
        let order = self.top_surfaces();
        let covered = |covers: &[Surface]| order.iter().any(|open| covers.contains(open));
        // The open report owns the wheel, ahead of the panel, the
        // conversation, home and the rail.
        if order.contains(&Surface::Report) {
            self.scroll_report_rows(!up, WHEEL_ROWS);
            return;
        }
        // The reviewer setup takes the wheel and does nothing with it: the
        // conversation under the panel stays where it is.
        if order.contains(&Surface::ReviewerSetup) {
            return;
        }
        // The quit question and the notice overlay have nothing to
        // scroll, and their frames cover the rows beneath: the wheel
        // over a frame is theirs and moves nothing.
        if [Surface::Quit, Surface::Notice]
            .into_iter()
            .any(|frame| order.contains(&frame) && crate::view::frame_of(self, frame).contains(at))
        {
            return;
        }
        if self
            .chrome()
            .layout()
            .and_then(|layout| layout.panel)
            .is_some_and(|panel| panel.contains(at))
        {
            self.wheel_panel(mouse.row, up);
        } else if self.session().is_some()
            && !covered(&[Surface::Keymap, Surface::Offer, Surface::Results])
            && self.over_conversation(at)
        {
            self.scroll_conversation(up);
        } else if self.on_home() && !covered(&[Surface::DeleteQuestion, Surface::WorkspacePicker]) {
            self.wheel_home(up, at);
        }
        let over_rail = self
            .chrome()
            .regions()
            .rail
            .is_some_and(|rail| rail.contains(Position::new(mouse.col, mouse.row)));
        if over_rail {
            self.scroll_rail(up);
        }
    }

    /// Whether `at` is over the conversation's visible rows: the
    /// conversation column's columns, from its top for the conversation's
    /// height, the same rows the draw keeps for the conversation
    /// (`view.rs`, `render`).
    fn over_conversation(&self, at: Position) -> bool {
        let height = self.conversation_height();
        let (left, width, top) = match self.chrome().layout() {
            Some(layout) => {
                // The item view's header takes the conversation's top
                // rows, so the wheel over it scrolls nothing.
                let item = if self.item_open() {
                    u16::try_from(crate::view::item::ITEM_HEADER_ROWS).unwrap_or(u16::MAX)
                } else {
                    0
                };
                (
                    layout.column.x,
                    layout.column.width,
                    layout.column.y.saturating_add(item),
                )
            }
            None => {
                let item = if self.item_open() {
                    u16::try_from(crate::view::item::ITEM_HEADER_ROWS).unwrap_or(u16::MAX)
                } else {
                    0
                };
                (0, self.screen.width(), item)
            }
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
    fn scroll_conversation(&mut self, up: bool) {
        let height = self.conversation_height();
        self.screen.scroll_rows(up, WHEEL_ROWS, height);
        self.settle();
    }

    /// Scrolls home's session list by [`WHEEL_ROWS`] rows when `at` is
    /// over its rows, clamped to the rows past its window, a modal
    /// question or picker winning over the list (`docs/tui.md`,
    /// "The session list"). The focus stays where it is: the frame
    /// pulls a focused row outside the window into view.
    fn wheel_home(&mut self, up: bool, at: Position) {
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
        self.settle();
    }
}
