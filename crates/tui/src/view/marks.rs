//! What the view paints over the conversation's rows once they are drawn
//! (`docs/tui.md`, "Selection and copy"): the selection's highlight.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::app::App;

/// The selection's highlight.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const SELECTION: Style = Style::new().bg(Color::Indexed(24));

/// Paints the selection's cells in the conversation `area`.
pub(super) fn draw(app: &App, area: Rect, buf: &mut Buffer) {
    for rect in app.selection_cells(area) {
        buf.set_style(rect.intersection(area), SELECTION);
    }
}

#[cfg(test)]
#[path = "marks_tests.rs"]
mod tests;
