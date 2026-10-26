//! The delegate or job view's header and body (`docs/tui.md`, "Swapped
//! views": 'Its top row, on the `surface` tint, is the view's name
//! bold, ...', 'The breadcrumb is bold. The status card is a `surface`
//! slab: ...').

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::App;
use crate::app::items::{ItemView, Spot};
use crate::format;
use crate::mouse::{Target, TargetId};
use crate::swapped;
use crate::text::to_u16;
use crate::theme::{Role, style};

/// The header's rows: the top row, the breadcrumb and the status rows.
pub(crate) const ITEM_HEADER_ROWS: usize = 5;

/// One status row's styled parts over the card's `surface` tint, cut at
/// the row's cells: the tint fills the row first, then each part draws
/// its cells in its own style, so the tint shows past short text
/// (`docs/tui.md`, "Swapped views": the status card is a `surface`
/// slab).
fn card_row(buf: &mut Buffer, x: u16, y: u16, width: usize, parts: &[(&str, Style)]) {
    let wide = u16::try_from(width).unwrap_or(u16::MAX);
    if wide == 0 {
        return;
    }
    buf.set_style(
        Rect::new(x, y, wide, 1),
        Style::new().bg(Role::Surface.color()),
    );
    let mut at = x;
    for (text, line_style) in parts {
        if at >= x.saturating_add(wide) {
            break;
        }
        let room = usize::from(x.saturating_add(wide).saturating_sub(at));
        let (end, _) = buf.set_stringn(at, y, text, room, *line_style);
        at = end;
    }
}

/// Draws the item view's header: the top row, whose whole row returns,
/// the bold breadcrumb, then the delegate's status card or the job's
/// status rows (`docs/tui.md`, "Swapped views").
pub(crate) fn draw_header(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let Some(view) = app.item_view() else {
        return;
    };
    if area.height == 0 || area.width == 0 {
        return;
    }
    // The top row names the view on the `surface` tint: the whole row is
    // one close target, with no ✕ drawn.
    let delegate = view.kind_glyph == "◆";
    let name = if delegate { "Delegate" } else { "Job" };
    swapped::draw_top_row(
        buf,
        Rect::new(area.x, area.y, area.width, 1),
        name,
        &format!("◆ {}", view.description),
        TargetId::Item(Spot::Close),
        targets,
    );
    if area.height < 2 {
        return;
    }
    // The breadcrumb names the attached session and the open job.
    let crumb = format!("{} › {} {}", view.parent, view.kind_glyph, view.description);
    let wide = usize::from(area.width);
    buf.set_stringn(
        area.x,
        area.y.saturating_add(1),
        format::cut(&crumb, wide),
        wide,
        Style::new().add_modifier(Modifier::BOLD),
    );
    if area.height < 3 {
        return;
    }
    let elapsed = elapsed_ms(app, view.started_ts, view.completed_ts);
    if delegate {
        draw_delegate_card(&view, area, elapsed, buf, targets);
    } else {
        draw_job_rows(&view, area, elapsed, buf, targets);
    }
}

/// The delegate's status card on the `surface` tint: the harness, the
/// model and the thinking level; under it the calls, the elapsed time
/// and the session; then the stop target while it runs, else the
/// outcome where the stop would be (`docs/tui.md`, "Swapped views").
fn draw_delegate_card(
    view: &ItemView,
    area: Rect,
    elapsed: u64,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) {
    let surface = Style::new().bg(Role::Surface.color());
    let y = area.y.saturating_add(2);
    let wide = usize::from(area.width);
    let mut model = format!(" · model {}", view.model);
    if let Some(level) = &view.level {
        model.push_str(&format!(" · {level}"));
    }
    card_row(
        buf,
        area.x,
        y,
        wide,
        &[
            (
                &format!("harness {}", view.harness),
                style(Role::Text).bg(Role::Surface.color()),
            ),
            (&model, style(Role::Muted).bg(Role::Surface.color())),
        ],
    );
    if area.height < 4 {
        return;
    }
    let calls = view
        .calls
        .map_or("—".to_owned(), |calls| format!("{calls} calls"));
    let session = view.session_label.as_deref().unwrap_or("—");
    card_row(
        buf,
        area.x,
        y.saturating_add(1),
        wide,
        &[(
            &format!(
                "{calls} · elapsed {} · session {session}",
                format::duration(elapsed)
            ),
            style(Role::Muted).bg(Role::Surface.color()),
        )],
    );
    if area.height < 5 {
        return;
    }
    let y = y.saturating_add(2);
    if !view.running {
        // The outcome keeps its mark where the stop target would be.
        let outcome = format!("{} {}", view.glyph, view.word);
        card_row(buf, area.x, y, wide, &[(&outcome, surface)]);
        return;
    }
    // The stop label always starts inside the row: the message is cut to
    // fit it, so the stop target covers the label's drawn cells.
    let stop = format!("■ stop {}", view.session_label.as_deref().unwrap_or("—"));
    let head = match &view.message {
        Some(message) => {
            let cut = format::cut(message, wide.saturating_sub(format::width(&stop) + 3));
            if cut.is_empty() {
                String::new()
            } else {
                format!("{cut} · ")
            }
        }
        None => String::new(),
    };
    let start = u16::try_from(format::width(&head)).unwrap_or(u16::MAX);
    let stop_at = area.x.saturating_add(start);
    let stop_width = u16::try_from(format::width(&stop)).unwrap_or(u16::MAX);
    card_row(
        buf,
        area.x,
        y,
        wide,
        &[
            (&head, style(Role::Text).bg(Role::Surface.color())),
            (&stop, style(Role::Error).bg(Role::Surface.color())),
        ],
    );
    targets.push(Target {
        id: TargetId::Item(Spot::Stop),
        rect: Rect::new(
            stop_at,
            y,
            stop_width.min(area.right().saturating_sub(stop_at)),
            1,
        ),
    });
}

/// The job's status rows on the `surface` tint: the job, its command,
/// the elapsed running time and the stop target while it runs, else the
/// outcome where the stop would be; under it the live-screen line
/// (`docs/tui.md`, "Swapped views").
fn draw_job_rows(
    view: &ItemView,
    area: Rect,
    elapsed: u64,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) {
    let y = area.y.saturating_add(2);
    let wide = usize::from(area.width);
    let mut status = format!(
        "job {} · {} · {} {}",
        view.job_id.0,
        view.description,
        view.word,
        format::duration(elapsed),
    );
    if view.running
        && let Some(message) = &view.message
    {
        status.push_str(&format!(" · {message}"));
    }
    if view.running {
        // The message is cut to fit the stop, so the stop label always
        // starts inside the row: its target covers the drawn cells.
        let stop = format!("■ stop {}", view.job_id.0);
        let status_cut = format::cut(&status, wide.saturating_sub(format::width(&stop) + 3));
        let start =
            u16::try_from(format::width(&status_cut) + format::width(" · ")).unwrap_or(u16::MAX);
        let stop_at = area.x.saturating_add(start);
        let stop_width = u16::try_from(format::width(&stop)).unwrap_or(u16::MAX);
        card_row(
            buf,
            area.x,
            y,
            wide,
            &[
                (&status_cut, style(Role::Text).bg(Role::Surface.color())),
                (" · ", style(Role::Muted).bg(Role::Surface.color())),
                (&stop, style(Role::Error).bg(Role::Surface.color())),
            ],
        );
        targets.push(Target {
            id: TargetId::Item(Spot::Stop),
            rect: Rect::new(
                stop_at,
                y,
                stop_width.min(area.right().saturating_sub(stop_at)),
                1,
            ),
        });
    } else {
        card_row(
            buf,
            area.x,
            y,
            wide,
            &[(&status, style(Role::Text).bg(Role::Surface.color()))],
        );
    }
    if area.height < 4 {
        return;
    }
    card_row(
        buf,
        area.x,
        y.saturating_add(1),
        wide,
        &[(
            "live screen · 80 columns × 24 rows from attach",
            style(Role::Muted).bg(Role::Surface.color()),
        )],
    );
}

/// Draws the item view's body for a view with no transcript: a `tty`
/// job's screen on its own `turn` slab, else the output line and the
/// job's tail (`docs/tui.md`, "Swapped views": the screen sits on a
/// `turn` slab 80 columns wide; any other body keeps "Output since this
/// terminal attached · {path}").
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
    if view.tty {
        // The screen keeps its own 80 columns: the slab clips at the
        // area's edge below 80, with no reflow of its rows.
        let wide = area.width.min(80);
        buf.set_style(
            Rect::new(area.x, area.y, wide, area.height),
            Style::new().bg(Role::Turn.color()),
        );
        let rows = app.item_output_rows(wide, area.height);
        let tint = style(Role::Text).bg(Role::Turn.color());
        for (index, row) in rows.iter().enumerate() {
            let y = area.y.saturating_add(to_u16(index));
            buf.set_stringn(
                area.x,
                y,
                format::cut(row, usize::from(wide)),
                usize::from(wide),
                tint,
            );
        }
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
