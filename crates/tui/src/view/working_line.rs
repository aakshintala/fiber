//! The working line (`docs/tui.md`, "The working line"): what a running
//! turn says while it works, and the spinner drawn on a moving
//! conversation line. Builders only mark the spinning cell; this draw
//! site is the only place that spins it, so a cached line's text never
//! changes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::App;
use crate::markdown::{Role, style};
use crate::mouse::{Target, TargetId};
use crate::working::lay;

/// Draws the working line on the row above `bottom`, above the running
/// delegates, and moves `bottom` up to it; nothing while no turn runs.
/// The glimmer runs across its word on the tick, and `esc to interrupt`
/// clicks like Esc.
pub(super) fn draw(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    bottom: &mut u16,
    targets: &mut Vec<Target>,
) {
    let Some(working) = app.working_line() else {
        return;
    };
    let laid = lay(&working, app.motion().wall_ms(), area.width);
    let line = if laid.retrying {
        style(Role::Warning)
    } else {
        style(Role::Muted)
    };
    let Some(rect) = super::put(buf, area, bottom, &laid.text, line) else {
        return;
    };
    // The glimmer's band in the spinner's colour, the word plain. The
    // sweep resumes after its rest, so rest frames ask for the next
    // boundary too: frame 17 would otherwise wait a whole second.
    if let Some(word) = &laid.word {
        if let Some(band) = app.motion().glimmer(word.len()) {
            for cell in band {
                let x = rect
                    .x
                    .saturating_add(u16::try_from(cell).unwrap_or(u16::MAX));
                if let Some(into) = buf.cell_mut((x, rect.y)) {
                    into.set_style(style(Role::Accent));
                }
            }
        }
        app.motion().ask_frame();
    }
    if let Some(next) = laid.next_ms {
        app.motion().ask_wall(next);
    }
    if let Some(cells) = laid.interrupt {
        targets.push(Target {
            id: TargetId::Interrupt,
            rect: Rect::new(
                rect.x.saturating_add(cells.start),
                rect.y,
                cells.end.saturating_sub(cells.start),
                1,
            ),
        });
    }
}

/// Where a marked line drew: its first drawn row, its mark's column,
/// whether its first row shows, and its total wrapped rows with how
/// many drew. A mark past the first row never spins.
pub(crate) struct Drawn {
    /// The line's first drawn row on screen.
    pub(crate) y: u16,
    /// The mark's column in the line.
    pub(crate) col: u16,
    /// The line's first row shows (`skip == 0`).
    pub(crate) first_row_shown: bool,
    /// The line's total wrapped rows.
    pub(crate) rows: usize,
    /// How many of its rows drew.
    pub(crate) count: usize,
}

/// Spins a marked conversation line's cell: the line's first row, while
/// that row shows above the "new messages" overlay. Anything else stays
/// as drawn and asks for no frame, so a hidden mark draws no timer.
pub(crate) fn spin(app: &App, buf: &mut Buffer, area: Rect, drawn: Drawn) {
    if !drawn.first_row_shown
        || drawn.count == 0
        || drawn.col >= area.width
        || (drawn.col > 0 && drawn.rows != 1)
    {
        return;
    }
    let last = area
        .bottom()
        .saturating_sub(u16::from(app.has_new() && area.height > 0));
    if drawn.y >= last {
        return;
    }
    app.motion().spin(
        buf,
        area.x.saturating_add(drawn.col),
        drawn.y,
    );
}

#[cfg(test)]
#[path = "working_line_tests.rs"]
mod tests;
