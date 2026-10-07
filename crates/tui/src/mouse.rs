//! Click targets and the pointer (`docs/tui.md`, "Mouse and hover"): the
//! one list of targets a frame draws, which clicks and hover both read.

use ratatui::layout::{Position, Rect};

use crate::keys::{Button, Mouse, MouseKind};

/// What a click target does when clicked. Each surface that draws a
/// target adds its variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetId {
    /// The approval badge: reopens the approval queue.
    Badge,
    /// "↓ New messages below": jumps to the end.
    NewBelow,
}

/// One click target as drawn: what it does and the cells it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) id: TargetId,
    pub(crate) rect: Rect,
}

/// The target drawn last whose cells hold `col`, `row`.
pub(crate) fn under(targets: &[Target], col: u16, row: u16) -> Option<&Target> {
    targets
        .iter()
        .rev()
        .find(|target| target.rect.contains(Position::new(col, row)))
}

/// The id of the target drawn last whose cells hold `col`, `row`.
pub(crate) fn hit(targets: &[Target], col: u16, row: u16) -> Option<TargetId> {
    under(targets, col, row).map(|target| target.id)
}

/// The pointer: where it last was, when hover records it, and the target
/// a left press went down on.
#[derive(Debug, Default)]
pub(crate) struct Pointer {
    /// The last cell reported; never set with hover off.
    pub(crate) at: Option<(u16, u16)>,
    /// The target under the last left press, until a release.
    pressed: Option<TargetId>,
}

impl Pointer {
    /// Takes one report against the targets on screen and returns the
    /// target clicked, if any: a left press then a release on the same
    /// target. Any other press disarms the click.
    pub(crate) fn on_mouse(
        &mut self,
        mouse: &Mouse,
        targets: &[Target],
        hover: bool,
    ) -> Option<TargetId> {
        if hover {
            self.at = Some((mouse.col, mouse.row));
        }
        let here = hit(targets, mouse.col, mouse.row);
        match mouse.kind {
            MouseKind::Press(Button::Left) => {
                self.pressed = here;
                None
            }
            MouseKind::Press(_) => {
                self.pressed = None;
                None
            }
            MouseKind::Release => self.pressed.take().filter(|pressed| Some(*pressed) == here),
            MouseKind::Motion | MouseKind::Drag(_) | MouseKind::WheelUp | MouseKind::WheelDown => {
                None
            }
        }
    }
}

#[cfg(test)]
#[path = "mouse_tests.rs"]
mod tests;
