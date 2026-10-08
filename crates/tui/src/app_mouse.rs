//! What a click does to the terminal's state (`docs/tui.md`, "Mouse and
//! hover").

use super::{App, Effect, Target};
use crate::keys::Key;
use crate::mouse::TargetId;

impl App {
    /// Handles a click on `target` (`docs/tui.md`, "Bindings"): the badge
    /// reopens the approval queue, "↓ New messages below" jumps to the end,
    /// a conversation line opens or closes what it names, a code block's
    /// `copy` copies its code, a steering row is selected, its ✕ drops
    /// it, a notice opens whole, its ✕ dismisses it, "+N more" lists the
    /// notices, and a paste token opens in the editor as Ctrl+G on it does.
    /// A recall waiting for a page waits no more. A click clears "Copied".
    pub(crate) fn on_click(&mut self, target: TargetId) -> Effect {
        self.history.cancel();
        self.copied = false;
        let effect = match target {
            TargetId::Badge => self.open_first(),
            TargetId::Link { .. } => self.follow_link(target),
            TargetId::Line(copy @ Target::Copy { .. }) => self.copy(copy),
            TargetId::Line(line) => {
                self.open(line);
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
            TargetId::Token(number) => self.open_token(number),
            TargetId::Turn(_) => Effect::None,
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
}
