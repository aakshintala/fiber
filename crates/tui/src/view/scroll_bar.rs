//! The conversation's scroll bar: a thumb on a track in the column's
//! last column, showing where the rows on screen sit among every row.
//! The thumb's rows are a pure function of the top row, the total rows
//! and the drawn rows, so the geometry is tested with a boundary table.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::App;
use crate::markdown::{Role, style};

/// The thumb's glyph.
pub(crate) const THUMB: &str = "█";
/// The track's glyph.
pub(crate) const TRACK: &str = "│";

/// Splits the conversation's area into the rows' area and the scroll bar's
/// column: the last column, once the area is at least 2 columns wide. Below
/// that the rows keep the whole area and the bar's rect has width 0 at
/// `area.right()`.
pub(crate) fn split(area: Rect) -> (Rect, Rect) {
    if area.width < 2 {
        return (area, Rect::new(area.right(), area.y, 0, area.height));
    }
    let rows = Rect::new(area.x, area.y, area.width - 1, area.height);
    let bar = Rect::new(area.right() - 1, area.y, 1, area.height);
    (rows, bar)
}

/// The width the conversation's rows wrap at in a column `column` wide:
/// `split`'s rows width.
pub(crate) fn text_width(column: u16) -> u16 {
    split(Rect::new(0, 0, column, 1)).0.width
}

/// The rows of a `track`-row bar the thumb covers, when `total` rows do
/// not fit in `track`; `None` when they fit.
pub(crate) fn thumb(top: usize, total: usize, track: u16) -> Option<Range<u16>> {
    if total <= usize::from(track) {
        return None;
    }
    let total128 = u128::try_from(total).ok()?;
    let track128 = u128::try_from(track).ok()?;
    let size128 = (track128 * track128 / total128).max(1);
    let size = u16::try_from(size128).ok()?;
    let span = total - usize::from(track);
    let free = track.checked_sub(size)?;
    let clamped = top.min(span);
    let start128 =
        u128::try_from(clamped).ok()? * u128::try_from(free).ok()? / u128::try_from(span).ok()?;
    let start = u16::try_from(start128).ok()?;
    let end = start.checked_add(size)?;
    Some(start..end)
}

/// Draws the bar in `bar`, the column `split` gave: nothing when `bar` is
/// empty or the conversation fits.
pub(crate) fn draw(app: &App, bar: Rect, buf: &mut Buffer) {
    if bar.is_empty() {
        return;
    }
    let total = app.scroll().1;
    let top = app.top().unwrap_or(total);
    let Some(rows) = thumb(top, total, bar.height) else {
        return;
    };
    let ink = style(Role::Muted);
    for y in bar.top()..bar.bottom() {
        let at = y - bar.y;
        let glyph = if rows.contains(&at) { THUMB } else { TRACK };
        buf.set_string(bar.x, y, glyph, ink);
    }
}

#[cfg(test)]
#[path = "scroll_bar_tests.rs"]
mod tests;
