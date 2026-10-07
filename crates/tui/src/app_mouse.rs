//! What a click does to the terminal's state (`docs/tui.md`, "Mouse and
//! hover").

use super::{App, Effect};
use crate::keys::Key;
use crate::mouse::TargetId;

impl App {
    /// Handles a click on `target` (`docs/tui.md`, "Bindings"): the badge
    /// reopens the approval queue, "↓ New messages below" jumps to the end,
    /// a conversation line opens or closes what it names.
    pub(crate) fn on_click(&mut self, target: TargetId) -> Effect {
        match target {
            TargetId::Badge => self.open_first(),
            TargetId::Line(line) => {
                self.open(line);
                Effect::None
            }
            TargetId::NewBelow => {
                self.follow();
                Effect::None
            }
        }
    }

    /// Puts the request the panel shows aside, as Esc does, so it waits on
    /// the badge. The `hover` jig's way to a badge from an events file.
    pub(crate) fn put_aside(&mut self) {
        self.queue.on_key(&Key::Esc);
    }
}
