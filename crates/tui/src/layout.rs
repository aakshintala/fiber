//! Where the session screen's regions go: the rail on the left, the
//! conversation column, and the panel on the right, each from the screen's
//! width alone (`docs/tui.md`, "Layout", "The narrow layout", "Shedding").

use ratatui::layout::Rect;

/// The conversation column's minimum width: below it the panel goes away
/// (`docs/tui.md`, "The narrow layout").
pub(crate) const CONVERSATION_MIN: u16 = 84;
/// The rail's floor, in columns.
pub(crate) const RAIL_FLOOR: u16 = 22;
/// The rail's ceiling, in columns.
pub(crate) const RAIL_CEILING: u16 = 48;
/// The panel's floor, in columns.
pub(crate) const PANEL_FLOOR: u16 = 30;
/// The panel's ceiling, in columns.
pub(crate) const PANEL_CEILING: u16 = 60;
/// Below this many columns or rows the screen is one line saying so
/// (`docs/tui.md`, "Shedding").
pub(crate) const FLOOR: (u16, u16) = (40, 10);

/// The rail's and the panel's shares of the screen's width, in percent
/// (`tui.rail.width`, `tui.panel.width`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Shares {
    /// The rail's share.
    pub(crate) rail: f64,
    /// The panel's share.
    pub(crate) panel: f64,
}

/// Which regions the session screen wants, before width decides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Want {
    /// The rail: two or more sessions are live and the person has not
    /// hidden it.
    pub(crate) rail: bool,
    /// Two or more sessions are live, hidden or not: a rail not drawn
    /// leaves its grip.
    pub(crate) rail_by_count: bool,
    /// The panel: the person has not hidden it.
    pub(crate) panel: bool,
}

/// The session screen's regions. They tile the screen's width with no
/// overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
    /// The rail, its last column the draggable edge.
    pub(crate) rail: Option<Rect>,
    /// The hidden rail's grip: column 0, while the rail is wanted by count
    /// and not drawn.
    pub(crate) grip: Option<Rect>,
    /// The conversation column, its header row included.
    pub(crate) column: Rect,
    /// The panel, its first column the draggable edge.
    pub(crate) panel: Option<Rect>,
    /// The panel is wanted and the screen is too narrow for it: the narrow
    /// layout's rows take its place.
    pub(crate) narrow: bool,
}

/// The one line a screen below [`FLOOR`] shows, naming its size; `None`
/// at the floor or above.
pub(crate) fn floor_line(width: u16, height: u16) -> Option<String> {
    let (columns, rows) = FLOOR;
    (width < columns || height < rows)
        .then(|| format!("Fiber needs {columns}×{rows} · now {width}×{height}"))
}

/// A region's width: `share` percent of `width`, rounded, kept from `floor`
/// to `ceiling`.
fn share_of(width: u16, share: f64, floor: u16, ceiling: u16) -> u16 {
    let target = f64::from(width) * share / 100.0;
    // The first whole number whose half above passes the target is the
    // target rounded half up; past the ceiling none is, and the ceiling
    // holds.
    (floor..=ceiling)
        .find(|columns| f64::from(*columns) + 0.5 > target)
        .unwrap_or(ceiling)
}

/// Splits a `width` by `height` screen. The rail sheds before the panel:
/// the panel's fit never counts the rail. A rail wanted by count but not
/// drawn leaves a one-column grip at the left edge, and the panel shows
/// only while the conversation keeps its minimum beside the grip.
pub(crate) fn split(width: u16, height: u16, shares: &Shares, want: &Want) -> Layout {
    let panel_w = share_of(width, shares.panel, PANEL_FLOOR, PANEL_CEILING);
    let rail_w = share_of(width, shares.rail, RAIL_FLOOR, RAIL_CEILING);
    let fits = |taken: u16| {
        width
            .checked_sub(taken)
            .is_some_and(|left| left >= CONVERSATION_MIN)
    };
    let panel_fits = want.panel && fits(panel_w);
    let beside = if panel_fits { panel_w } else { 0 };
    let rail_shown = want.rail && fits(rail_w.saturating_add(beside));
    let grip_w = u16::from(want.rail_by_count && !rail_shown);
    let panel_shown = want.panel && fits(grip_w.saturating_add(panel_w));
    let left = if rail_shown { rail_w } else { grip_w };
    let right = if panel_shown { panel_w } else { 0 };
    let rail = rail_shown.then(|| Rect::new(0, 0, rail_w, height));
    let grip = (grip_w > 0).then(|| Rect::new(0, 0, grip_w, height));
    let panel = panel_shown.then(|| Rect::new(width.saturating_sub(right), 0, right, height));
    let column = Rect::new(
        left,
        0,
        width.saturating_sub(left).saturating_sub(right),
        height,
    );
    Layout {
        rail,
        grip,
        column,
        panel,
        narrow: want.panel && !panel_shown,
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
