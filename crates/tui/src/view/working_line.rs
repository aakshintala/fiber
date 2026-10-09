//! The working line (`docs/tui.md`, "The working line"): what a running
//! turn says while it works, and the spinner drawn on a moving
//! conversation line. Builders only mark the spinning cell; this draw
//! site is the only place that spins it, so a cached line's text never
//! changes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::App;

/// Spins a marked conversation line's cell: the line's first row, while
/// that row shows above the "new messages" overlay. Anything else stays
/// as drawn and asks for no frame, so a hidden mark draws no timer.
pub(crate) fn spin(
    app: &App,
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    col: u16,
    first_row_shown: bool,
    rows: usize,
) {
    if !first_row_shown || rows == 0 || col >= area.width || (col > 0 && rows != 1) {
        return;
    }
    let last = area
        .bottom()
        .saturating_sub(u16::from(app.has_new() && area.height > 0));
    if y >= last {
        return;
    }
    app.motion().spin(buf, area.x.saturating_add(col), y);
}

#[cfg(test)]
#[path = "working_line_tests.rs"]
mod tests;
