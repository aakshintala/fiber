//! The working line (`docs/tui.md`, "The working line"): what a running
//! turn says while it works, and the spinner drawn on a moving
//! conversation line. Builders only mark the spinning cell; this draw
//! site is the only place that spins it, so a cached line's text never
//! changes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::App;
use crate::ext::{Input, RawSpan, SlotDraw, WorkingLine, WorkingRetry};
use crate::mouse::{Target, TargetId};
use crate::theme::{Role, style};
use crate::view::notice_rows::frame_bound;
use crate::working::{Working, lay};

/// The working line's renderer input: the attached turn's start with
/// its wait to retry, from the same turn card the built-in reads.
fn input(working: &Working, flat: bool) -> WorkingLine {
    WorkingLine {
        started: working.started_ms,
        retry: working.retry.as_ref().map(|retry| WorkingRetry {
            code: retry.retry.code.clone(),
            attempt: retry.attempt,
            last_attempt: retry.retry.last_attempt,
            delay_ms: retry.retry.delay_ms,
            ts: retry.ts,
        }),
        flat,
    }
}

/// The built-in working-line row as spans: the laid text in its base
/// style. The spinner's colour and the glimmer band are paint-time
/// recolors, so a cached line's text never changes.
fn builtin_spans(app: &App, working: &Working, width: u16) -> Vec<Vec<RawSpan>> {
    let motion = app.motion();
    let laid = lay(working, motion.wall_ms(), motion.spinner(), width);
    let line = if laid.retrying {
        style(Role::Warning)
    } else {
        style(Role::Muted)
    };
    vec![crate::ext::spans::from_ratatui(&[
        ratatui::text::Span::styled(laid.text, line),
    ])]
}

/// How many rows the working line reserves: the banner's row while it
/// shows, else a replacement's line count, or the built-in row while
/// the turn runs. A zero-line replacement hides the line.
pub(crate) fn count(app: &App, width: u16) -> usize {
    if app.banner().is_some() {
        return 1;
    }
    let Some(working) = app.working_line() else {
        return 0;
    };
    let bound = frame_bound(app);
    let folded = input(&working, bound.flat);
    let builtin = &|_: &Input<'_>, width: u16| builtin_spans(app, &working, width);
    match bound.slot_lines(
        SlotDraw::Count,
        "working_line",
        &Input::WorkingLine(folded),
        width,
        builtin,
    ) {
        Some(lines) => lines.len(),
        // A miss draws the built-in, which shows exactly when the row shows.
        None => usize::from(app.working_row_shown()),
    }
}

/// Draws the working line on the row above `bottom`, above the running
/// delegates, and moves `bottom` up to it; nothing while no turn runs.
/// The glimmer runs across its word on the tick, and `Esc to interrupt`
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
    let bound = frame_bound(app);
    let folded = input(&working, bound.flat);
    let builtin = &|_: &Input<'_>, width: u16| builtin_spans(app, &working, width);
    let Some(fitted) = bound.slot_lines(
        SlotDraw::Paint,
        "working_line",
        &Input::WorkingLine(folded),
        area.width,
        builtin,
    ) else {
        paint_builtin(app, &working, area, buf, bottom, targets);
        return;
    };
    if fitted.is_empty() {
        // An empty replacement hides the line: no rows and no targets.
        return;
    }
    paint_replacement(area, buf, bottom, targets, &fitted);
}

/// Paints a replacement's lines bottom-up, keeping one interrupt target
/// over each line's full width.
fn paint_replacement(
    area: Rect,
    buf: &mut Buffer,
    bottom: &mut u16,
    targets: &mut Vec<Target>,
    fitted: &[crate::ext::spans::FittedLine],
) {
    let count = fitted
        .len()
        .min(usize::from(u16::MAX))
        .min(bottom.saturating_sub(area.y) as usize);
    if count == 0 {
        return;
    }
    *bottom = bottom.saturating_sub(u16::try_from(count).unwrap_or(u16::MAX));
    for (dy, line) in fitted.iter().take(count).enumerate() {
        let row = bottom.saturating_add(u16::try_from(dy).unwrap_or(u16::MAX));
        buf.set_line(area.x, row, &line.line, area.width);
        targets.push(Target {
            id: TargetId::Interrupt,
            rect: Rect::new(area.x, row, area.width, 1),
        });
    }
}

/// Paints the built-in working line: the laid text with the spinner in
/// the attention colour and the glimmer band across its word.
fn paint_builtin(
    app: &App,
    working: &Working,
    area: Rect,
    buf: &mut Buffer,
    bottom: &mut u16,
    targets: &mut Vec<Target>,
) {
    let motion = app.motion();
    let laid = lay(working, motion.wall_ms(), motion.spinner(), area.width);
    let line = if laid.retrying {
        style(Role::Warning)
    } else {
        style(Role::Muted)
    };
    let Some(rect) = super::put(buf, area, bottom, &laid.text, line) else {
        return;
    };
    // The spinner in the attention colour, stepping on the motion tick
    // (`docs/tui.md`, "The working line"). Under reduced motion the
    // spinner is still and the word is plain: the motion gives no band.
    if let Some(at) = laid.spinner {
        let x = rect.x.saturating_add(u16::try_from(at).unwrap_or(u16::MAX));
        if let Some(cell) = buf.cell_mut((x, rect.y)) {
            cell.set_style(style(Role::Attention));
        }
    }
    // The glimmer's band across the word: its centre bold in the
    // spinner's colour, its two sides in it, the rest of the word dim.
    // The sweep resumes after its rest, so rest frames ask for the next
    // boundary too: frame 17 would otherwise wait a whole second.
    if let Some(word) = &laid.word {
        if let Some(band) = motion.glimmer(word.len()) {
            // The band's middle cell carries the bold centre: the first
            // of two, the only of one.
            let centre = band
                .start
                .saturating_add(band.end.saturating_sub(band.start) / 2);
            for cell in band.clone() {
                let at = word.start.saturating_add(cell);
                let x = rect.x.saturating_add(u16::try_from(at).unwrap_or(u16::MAX));
                if let Some(into) = buf.cell_mut((x, rect.y)) {
                    let mut paint = Style::new().fg(Role::Attention.color());
                    if cell == centre {
                        paint = paint.add_modifier(Modifier::BOLD);
                    }
                    into.set_style(paint);
                }
            }
        }
        motion.ask_frame();
    }
    if let Some(next) = laid.next_ms {
        motion.ask_wall(next);
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
    app.motion()
        .spin(buf, area.x.saturating_add(drawn.col), drawn.y);
}

#[cfg(test)]
#[path = "working_line_tests.rs"]
mod tests;
