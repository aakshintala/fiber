//! The delegate or job view's header and body (`docs/tui.md`, "Swapped
//! views": 'Its header is a breadcrumb ("main › ◆ review: …"). A status
//! card gives harness, model, calls, elapsed time and session, with a stop
//! target.').

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::app::App;
use crate::app::items::Spot;
use crate::format;
use crate::markdown::{Role, style};
use crate::mouse::{Target, TargetId};
use crate::text::to_u16;

/// The header's rows: the breadcrumb and the status row.
pub(crate) const ITEM_HEADER_ROWS: usize = 2;

/// Draws the item view's header: the whole breadcrumb row returns,
/// then the status row has the stop target while the item runs.
pub(crate) fn draw_header(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let Some(view) = app.item_view() else {
        return;
    };
    if area.height == 0 || area.width == 0 {
        return;
    }
    let crumb = format!("{} › {} {}", view.parent, view.kind_glyph, view.description);
    let crumb_width = usize::from(area.width);
    buf.set_stringn(
        area.x,
        area.y,
        format::cut(&crumb, crumb_width),
        crumb_width,
        Style::default(),
    );
    targets.push(Target {
        id: TargetId::Item(Spot::Close),
        rect: Rect::new(area.x, area.y, area.width, 1),
    });
    if area.height < 2 {
        return;
    }
    // The status row: glyph and word, harness, model, calls, elapsed time
    // and session, with the stop target while the item runs.
    let elapsed = elapsed_ms(app, view.started_ts, view.completed_ts);
    let calls = view
        .calls
        .map_or("—".to_owned(), |calls| format!("{calls} calls"));
    let session = view.session_label.as_deref().unwrap_or("—");
    let mut status = format!(
        "{} {} · harness {} · model {} · {calls} · {} · session {session}",
        view.glyph,
        view.word,
        view.harness,
        view.model,
        format::duration(elapsed),
    );
    if let Some(message) = &view.message {
        status.push_str(&format!(" · {message}"));
    }
    let y = area.y.saturating_add(1);
    if view.running {
        let stop = "■ stop";
        let room = usize::from(area.width);
        let status_cut = format::cut(&status, room.saturating_sub(format::width(stop) + 3));
        let line = format!("{status_cut} · {stop}");
        buf.set_stringn(area.x, y, &line, room, style(Role::Text));
        let start =
            u16::try_from(format::width(&status_cut) + format::width(" · ")).unwrap_or(u16::MAX);
        let wide = u16::try_from(format::width(stop)).unwrap_or(u16::MAX);
        if area.x.saturating_add(start) < area.right() {
            targets.push(Target {
                id: TargetId::Item(Spot::Stop),
                rect: Rect::new(
                    area.x.saturating_add(start),
                    y,
                    wide.min(area.right().saturating_sub(area.x.saturating_add(start))),
                    1,
                ),
            });
        }
    } else {
        buf.set_stringn(
            area.x,
            y,
            format::cut(&status, usize::from(area.width)),
            usize::from(area.width),
            Style::default(),
        );
    }
}

/// Draws the item view's body for a view with no transcript: the output
/// line, then the job's output since the terminal attached: a `tty` job's
/// screen, or the tail of any other job's.
///
/// `job_delta` is ephemeral, so output written before the terminal
/// attached is not shown (`docs/events.md`, `job_delta`).
pub(crate) fn draw_body(app: &App, area: Rect, buf: &mut Buffer) {
    let Some(view) = app.item_view() else {
        return;
    };
    if area.height == 0 || area.width == 0 {
        return;
    }
    let wide = usize::from(area.width);
    buf.set_stringn(
        area.x,
        area.y,
        format::cut(
            &format!("Output since this terminal attached · {}", view.output_path),
            wide,
        ),
        wide,
        style(Role::Muted),
    );
    let height = usize::from(area.height).saturating_sub(1);
    let rows = app.item_output_rows(area.width, to_u16(height));
    for (index, row) in rows.iter().enumerate() {
        let y = area.y.saturating_add(to_u16(index).saturating_add(1));
        buf.set_stringn(area.x, y, format::cut(row, wide), wide, style(Role::Text));
    }
}

/// The elapsed milliseconds the status row shows: from the run's start to
/// the frame's wall time while it runs, frozen at the completion's `ts`
/// after. While the item runs the next whole second is asked for, so the
/// row ticks without any other wake.
pub(crate) fn elapsed_ms(app: &App, started_ts: u64, completed_ts: Option<u64>) -> u64 {
    if let Some(completed) = completed_ts {
        return completed.saturating_sub(started_ts);
    }
    let Some(wall) = app.motion().wall_ms() else {
        return 0;
    };
    let elapsed = wall.saturating_sub(started_ts);
    let next = started_ts.checked_add((elapsed / 1000).saturating_add(1).saturating_mul(1000));
    if let Some(next) = next {
        app.motion().ask_wall(next);
    }
    elapsed
}

#[cfg(test)]
#[path = "item_tests.rs"]
mod tests;
