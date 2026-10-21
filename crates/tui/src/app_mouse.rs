//! What a click does to the terminal's state (`docs/tui.md`, "Mouse and
//! hover").

use super::{App, Effect, Target};
use crate::app::rail::Spot;
use crate::keys::{Key, Mouse, MouseKind};
use crate::mouse::TargetId;
use ratatui::layout::{Position, Rect};

/// A wheel step over the conversation scrolls it this many rows: a
/// starting point, not a measurement (`docs/tui.md`, "Turns").
pub(super) const WHEEL_ROWS: usize = 3;

impl App {
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
                // The import report's ✕ closes the report: the generic
                // close below stays for every other overlay.
                if self.import_report_open() {
                    self.close_import_report();
                } else {
                    self.close_overlay();
                }
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
        // The open report owns the wheel, ahead of the panel, the
        // conversation, home and the rail.
        if self.import_report_open() {
            self.scroll_report_rows(!up, WHEEL_ROWS);
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
            && self.keymap_top().is_none()
            && !self.offer_open()
            && !self.results_open()
            && self.over_conversation(at)
        {
            self.scroll_conversation(up);
        } else if self.on_home() && !self.home_modal() {
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
