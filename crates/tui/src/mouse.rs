//! Click targets and the pointer (`docs/tui.md`, "Mouse and hover"): the
//! one list of targets a frame draws, which clicks and hover both read.

use ratatui::layout::{Position, Rect};

use crate::ext::cache::ClickKey;
use crate::keys::{Button, Mouse, MouseKind};

/// What a click target does when clicked. Each surface that draws a
/// target adds its variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetId {
    /// The approval badge: reopens the approval queue.
    Badge,
    /// "↓ New messages below": jumps to the end.
    NewBelow,
    /// A conversation line: opens or closes what it names.
    Line(crate::app::Target),
    /// A link drawn on a conversation row: the screen row and column of
    /// its first cell, and a hash of the destination drawn there
    /// (`docs/tui.md`, "Links": a stale frame never opens another URL).
    Link { row: usize, col: u16, url: u64 },
    /// The search bar's match count: opens the search results
    /// (`docs/tui.md`, "Search").
    FindCount,
    /// A search results entry, by its index: jumps to its match
    /// (`docs/tui.md`, "Search").
    FindResult(usize),
    /// The paste token with this number in the input box: opens its text
    /// in the editor.
    Token(usize),
    /// A queued steering row, by its index: selects it.
    Steering(usize),
    /// A queued steering row's ✕, by its index: drops it.
    DropSteering(usize),
    /// A notice's box, by its id: shows its whole text.
    Notice(usize),
    /// A notice's ✕, by its id: dismisses it.
    DismissNotice(usize),
    /// "+N more" under the notices: lists them all.
    MoreNotices,
    /// A turn, by its index: a focus stop that clicks and hover pass
    /// over.
    Turn(usize),
    /// "Esc to interrupt" on the working line: interrupts the turn as
    /// Esc does (`docs/tui.md`, "The working line").
    Interrupt,
    /// The open overlay's ✕: closes it.
    CloseOverlay,
    /// A home row, chip or toggle: what a click there does.
    Home(crate::home::Spot),
    /// A chip, the Send row or the ✕ of the repository offer.
    Offer(crate::offer::Spot),
    /// A tab or row of the question form on the request panel.
    Form(crate::approvals::form::Spot),
    /// A configuration view's ✕ or row (`docs/tui.md`, "Swapped views").
    View(crate::swapped::Spot),
    /// A panel item: what a click there does.
    Panel(crate::app::panel::Spot),
    /// An item view's header target: closing the view or stopping its job.
    Item(crate::app::items::Spot),
    /// A rail card, its ✕ or its project's "+": what a click there does.
    Rail(crate::app::rail::Spot),
    /// An extension's click target: the interned identity of the
    /// (extension, slot, click id) tuple whose spans drew the cells.
    /// The cache owns the only lookup, so a key names a target the
    /// frame actually drew.
    Extension(ClickKey),
}

/// One click target as drawn: what it does and the cells it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) id: TargetId,
    pub(crate) rect: Rect,
}

/// The target drawn last whose cells hold `col`, `row`, passing over a
/// turn: turns are focus stops, never click targets.
pub(crate) fn under(targets: &[Target], col: u16, row: u16) -> Option<&Target> {
    targets
        .iter()
        .rev()
        .filter(|target| !matches!(target.id, TargetId::Turn(_)))
        .find(|target| target.rect.contains(Position::new(col, row)))
}

/// The id of the target drawn last whose cells hold `col`, `row`.
pub(crate) fn hit(targets: &[Target], col: u16, row: u16) -> Option<TargetId> {
    under(targets, col, row).map(|target| target.id)
}

/// A press and release paired on one extension target: the target's
/// identity, the release cell it resolved at, and the built-in target
/// under the same cell. A handled click runs nothing built in; any
/// other click resumes the built-in candidate, so scrolling and
/// selection keep working (`docs/tui.md`, "Input and focus").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClickRoute {
    /// The paired extension target's identity.
    pub(crate) id: ClickKey,
    /// The release cell the route resolved at.
    pub(crate) col: u16,
    pub(crate) row: u16,
    /// The topmost non-extension, non-turn target at the release cell
    /// in the same frame, if any: turns are focus stops, never click
    /// targets.
    pub(crate) fallback: Option<TargetId>,
}

/// The route for a press and release paired on the extension target
/// `id` at `col`, `row`: none when the frame holds no such target
/// there, so a stale pair never dispatches. Both candidates come from
/// the frame's one target list.
pub(crate) fn click_route(
    targets: &[Target],
    id: ClickKey,
    col: u16,
    row: u16,
) -> Option<ClickRoute> {
    let at = Position::new(col, row);
    let paired = targets
        .iter()
        .any(|target| target.id == TargetId::Extension(id) && target.rect.contains(at));
    if !paired {
        return None;
    }
    let fallback = targets
        .iter()
        .rev()
        .filter(|target| !matches!(target.id, TargetId::Extension(_) | TargetId::Turn(_)))
        .find(|target| target.rect.contains(at))
        .map(|target| target.id);
    Some(ClickRoute {
        id,
        col,
        row,
        fallback,
    })
}

/// The pointer: where it last was, when hover records it, and the target
/// a left press went down on and its cell.
#[derive(Debug, Default)]
pub(crate) struct Pointer {
    /// The last cell reported; never set with hover off.
    pub(crate) at: Option<(u16, u16)>,
    /// The target under the last left press, until a release.
    pressed: Option<TargetId>,
    /// The cell of the last left press, until a release.
    pressed_at: Option<(u16, u16)>,
}

impl Pointer {
    /// Takes one report against the targets on screen and returns the
    /// target clicked, if any: a left press then a release on the same
    /// target. Any other press disarms the click, and so does a drag to
    /// another cell: a drag selects (`docs/tui.md`, "Selection and copy"),
    /// so one that returns to its target and releases does not click.
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
                self.pressed_at = Some((mouse.col, mouse.row));
                None
            }
            MouseKind::Press(_) => {
                self.pressed = None;
                self.pressed_at = None;
                None
            }
            MouseKind::Release => {
                self.pressed_at = None;
                self.pressed.take().filter(|pressed| Some(*pressed) == here)
            }
            MouseKind::Drag(_) => {
                if Some((mouse.col, mouse.row)) != self.pressed_at {
                    self.pressed = None;
                }
                None
            }
            MouseKind::Motion | MouseKind::WheelUp | MouseKind::WheelDown => None,
        }
    }
}

#[cfg(test)]
#[path = "mouse_tests.rs"]
mod tests;
