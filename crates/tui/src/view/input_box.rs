//! The input box: the draft's rows on their own tint with half-block edges
//! (`docs/tui.md`, "Look", "The input box").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::panel::{PERMISSION_MODE, bar_spans, short_tokens};
use super::status_rows::{Joined, Segment, StatusRow, cut_row, push_segment};
use crate::app::App;
use crate::app::panel::Spot;
use crate::ext::{Context, Input, InputStatus, RawSpan, SlotDraw};
use crate::format;
use crate::mouse::{Target, TargetId};
use crate::surface;
use crate::text::to_u16;
use crate::theme::{Role, style};

/// The hint at the box's right end while a queued message is edited
/// (`docs/tui.md`, "Steering").
const EDITING_HINT: &str = "editing a queued message · Enter amends · ⌥X drops · Esc stops";

/// The item view's input hint while its draft is empty and no queued
/// message is edited: steering for a delegate, job input for a job;
/// `None` anywhere else (`docs/tui.md`, "Swapped views").
fn item_hint(app: &App) -> Option<String> {
    if !app.item_open() || app.steering_selected().is_some() || !app.input().expand().is_empty() {
        return None;
    }
    let view = app.item_view()?;
    if view.kind_glyph == "◆" {
        Some(format!("steering goes to ◆ {}", view.description))
    } else {
        Some("type into the job".to_owned())
    }
}

/// Draws the input box on the rows above `bottom`: its rows on the surface
/// tint with edges where they fit, token targets over them. The Ctrl+R
/// panel draws centred over the conversation instead of in reserved rows,
/// and the `/` and `@` panels draw over the conversation after it, so the
/// box draws on the same rows with any panel open or closed.
/// Moves `bottom` above it all and returns the box's top-edge row, so the
/// `/` and `@` overlay sits above it.
pub(super) fn draw(
    app: &App,
    area: Rect,
    bottom: &mut u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> u16 {
    let (rows, top, cursor_row, cursor_col) = rows(app, area.width);
    let fits = surface::edged(rows.len(), usize::from(area.height)) > rows.len();
    // While the rail holds focus the box steps back: its stripe in
    // muted and its text dim, so the lifted card leads. An editing
    // stripe still wins.
    let focused = app.rail_state().focused().is_some();
    let mut bottom_edge = false;
    if fits && let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) {
        *bottom = y;
        bottom_edge = true;
    }
    // The rule and the status row draw when the draft, the top edge and
    // both fit: cut short, the box keeps the draft with its edges, and
    // the chrome sheds first.
    let chrome = usize::from(bottom.saturating_sub(area.y)) >= rows.len() + usize::from(fits) + 2;
    let below_box = *bottom;
    if chrome {
        // The status lines sit under the status row, on the slab with
        // no panel tint; then the row through its slot, then the rule.
        super::status_rows::paint(app, area, buf, bottom, targets);
        paint_row(app, area, bottom, buf, targets);
        put_rule(area, bottom, buf);
    }
    let below = *bottom;
    let text = text_area(area);
    for (at, row) in rows.iter().enumerate().rev() {
        // The first draft row opens with the prompt in `info`
        // (`docs/tui.md`, "The input box").
        put_row(buf, area, text, bottom, row, top == 0 && at == 0, focused);
    }
    let slab = Rect::new(
        area.x,
        *bottom,
        area.width,
        below_box.saturating_sub(*bottom),
    );
    let mut top_edge = false;
    if fits && let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) {
        *bottom = y;
        top_edge = true;
    }
    // Above the box's top edge: the box's own top row, whatever drew it.
    let box_top = *bottom;
    // Without colour the box is thin: ┌─┐ and └─┘ on the edge rows and
    // │ on the slab's columns, dim, with no tint and no stripe: the
    // stripe's column is the box's left side (`docs/tui.md`, "Look").
    if surface::box_fit(area.width) {
        surface::draw_box(
            buf,
            area,
            slab,
            surface::Edges {
                top: top_edge,
                bottom: bottom_edge,
            },
        );
    } else {
        // While a queued message is edited the stripe is in `attention`
        // (`docs/tui.md`, "Steering").
        let stripe = if app.steering_selected().is_some() {
            Role::Attention
        } else if focused {
            Role::Muted
        } else {
            Role::Accent
        };
        surface::draw_slab(
            buf,
            slab,
            Role::Surface,
            Some(surface::Stripe {
                colour: stripe,
                right: false,
            }),
            surface::Edges {
                top: top_edge,
                bottom: bottom_edge,
            },
        );
    }
    if app.steering_selected().is_some() {
        draw_editing_hint(area, below, &rows, (cursor_row, cursor_col), buf);
    } else if let Some(hint) = item_hint(app) {
        draw_hint(area, below, &rows, (cursor_row, cursor_col), &hint, buf);
    }
    draw_selection(app, area, below, &rows, top, buf);
    if cursor_shown(app) {
        let (x, y) = caret_cell(area, below, &rows, cursor_row, cursor_col);
        // The caret's cell draws over the selection: the covered
        // character in the slab's colour on the text colour, or a block
        // in the text colour past the last character (`docs/tui.md`,
        // "The input box"). The terminal's own cursor stays where
        // `super::cursor` puts it. The row always sits inside the area:
        // the caret's row shows.
        let covered = rows
            .get(cursor_row)
            .and_then(|row| super::cursor::covered_symbol(row, usize::from(cursor_col)));
        let (symbol, style) =
            super::cursor::cursor_cell(covered.as_deref(), Role::Surface, Role::Text);
        buf.set_stringn(x, y, &symbol, crate::text::width(&symbol), style);
    }
    token_targets(app, area, below, (top, rows.len()), targets);
    box_top
}

/// The columns the draft's rows take: past the stripe and gap where the
/// box has them (`docs/tui.md`, "Look", "The input box"). Without
/// colour the stripe's column is the box's left side and the right column
/// is reserved, so the text sits one column in, two narrower
/// (`docs/tui.md`, "Look").
fn text_area(area: Rect) -> Rect {
    if surface::box_fit(area.width) {
        Rect::new(
            area.x.saturating_add(1),
            area.y,
            area.width.saturating_sub(2),
            area.height,
        )
    } else {
        let width = surface::inset(area.width);
        Rect::new(
            area.x.saturating_add(area.width.saturating_sub(width)),
            area.y,
            width,
            area.height,
        )
    }
}

/// The row's left side in `room` columns: the working directory, then
/// the permission mode in `attention`, joined by a dim `  │  `. Too
/// wide, the working directory is cut from the left to keep its tail
/// first, then the side is cut at its right end
/// (`docs/tui.md`, "The input box").
fn status_left(workspace: &str, room: usize) -> StatusRow {
    let mut full = Joined {
        spans: Vec::new(),
        spots: Vec::new(),
    };
    if !workspace.is_empty() {
        push_segment(
            &mut full,
            Segment::spotted(vec![Span::raw(workspace.to_owned())], Spot::Status),
        );
    }
    push_segment(
        &mut full,
        Segment::spotted(
            vec![Span::styled(PERMISSION_MODE, style(Role::Attention))],
            Spot::Status,
        ),
    );
    let wide: usize = full
        .spans
        .iter()
        .map(|span| format::width(&span.content))
        .sum();
    if wide <= room {
        return cut_row(full, wide.saturating_add(1));
    }
    // Cut the working directory from the left first, keeping its tail
    // behind an ellipsis; the shed cells leave exactly `room`.
    let shed = wide - room;
    let ws_w: usize = format::width(workspace);
    if !workspace.is_empty() && shed < ws_w {
        let tail = crate::text::cut_left(workspace, ws_w - shed);
        let mut cut = Joined {
            spans: Vec::new(),
            spots: Vec::new(),
        };
        push_segment(
            &mut cut,
            Segment::spotted(vec![Span::raw(tail)], Spot::Status),
        );
        push_segment(
            &mut cut,
            Segment::spotted(
                vec![Span::styled(PERMISSION_MODE, style(Role::Attention))],
                Spot::Status,
            ),
        );
        return cut_row(cut, room.saturating_add(1));
    }
    // Past the working directory the side is cut at its right end.
    let mut rest = Joined {
        spans: Vec::new(),
        spots: Vec::new(),
    };
    push_segment(
        &mut rest,
        Segment::spotted(
            vec![Span::styled(PERMISSION_MODE, style(Role::Attention))],
            Spot::Status,
        ),
    );
    cut_row(rest, room.saturating_add(1))
}

/// The row's ctx segment over the whole segment: `ctx` dim, the 8-cell
/// bar, the size and the share of the window dim, opening the context
/// breakdown (`docs/tui.md`, "The input box"). Unknown figures show an
/// empty bar with dim dashes, so the row keeps both sides from the
/// first frame.
fn ctx_segment(app: &App) -> Segment {
    let panel = app.panel_state();
    let known = app.session().and_then(|session| {
        let window = panel.window_for(session).filter(|window| *window > 0)?;
        let context = panel.status_for(session)?.context.as_ref()?;
        Some((window, context))
    });
    let mut spans = vec![
        Span::styled("ctx", style(Role::Muted)),
        Span::raw(" ".to_owned()),
    ];
    match known {
        Some((window, context)) => {
            spans.extend(bar_spans(
                context.tokens,
                panel.trigger_at().unwrap_or(window),
                None,
                8,
            ));
            spans.push(Span::raw(" ".to_owned()));
            spans.push(Span::styled(
                short_tokens(context.tokens),
                style(Role::Muted),
            ));
            spans.push(Span::styled(" · ", style(Role::Muted)));
            let share = context
                .tokens
                .saturating_mul(100)
                .checked_div(window)
                .unwrap_or(0);
            spans.push(Span::styled(format!("{share}%"), style(Role::Muted)));
        }
        None => {
            spans.extend(bar_spans(0, 1, None, 8));
            spans.push(Span::raw(" ".to_owned()));
            spans.push(Span::styled("—", style(Role::Muted)));
            spans.push(Span::styled(" · ", style(Role::Muted)));
            spans.push(Span::styled("—", style(Role::Muted)));
        }
    }
    Segment::spotted(spans, Spot::Context)
}

/// The row's right side: the model in `accent` with the thinking level
/// in `attention`, then the ctx segment, joined by a dim `  │  `. The
/// model slot stays blank until a preamble names one
/// (`docs/tui.md`, "The input box").
fn status_right(app: &App) -> Joined {
    let panel = app.panel_state();
    let mut joined = Joined {
        spans: Vec::new(),
        spots: Vec::new(),
    };
    let model = panel
        .model()
        .or_else(|| panel.status().map(|status| status.model.as_str()));
    let mut spans = Vec::new();
    if let Some(model) = model {
        spans.push(Span::styled(model.to_owned(), style(Role::Accent)));
    }
    if let Some(thinking) = panel.thinking() {
        if !spans.is_empty() {
            spans.push(Span::raw(" ".to_owned()));
        }
        spans.push(Span::styled(thinking.to_owned(), style(Role::Attention)));
    }
    if !spans.is_empty() {
        push_segment(&mut joined, Segment::spotted(spans, Spot::Model));
    }
    push_segment(&mut joined, ctx_segment(app));
    joined
}

/// The status row's laid sides at `avail` columns: the cut left and
/// right rows with each side's drawn width. Shared by the built-in
/// paint and the extension's view of it, so both cut the same way.
struct RowSides {
    /// The cut left side.
    left: StatusRow,
    /// The cut right side.
    right: StatusRow,
    /// The left side's drawn width in cells.
    lw: usize,
    /// The right side's drawn width in cells.
    right_w: usize,
}

/// Lays the status row's sides in `room` columns: the working directory
/// cut from the left first, then the left side at its right end; the
/// right side only when the slab is narrower than it.
// Exempt from mutation testing: the two comparisons below are
// equivalent on every input a consumer can draw, so no test can kill
// their mutants. `status_right` always pushes the ctx segment (`"ctx "`
// alone is 4 cells), so `rw >= 4` on every call: at `rw == avail` both
// arms build the same pair, and `rw >= 0` is the same condition as
// `rw > 0`. `rw == 0` and `rw < 0` reduce to the room-0 branch, and
// `status_left` at room 0 draws the same empty line as the dropped
// side (proved by flipping each mutant and diffing the drawn rows).
#[cfg_attr(false, mutants::skip)]
fn row_sides(app: &App, avail: usize) -> RowSides {
    let workspace = app
        .panel_state()
        .status()
        .map(|status| status.workspace.as_str())
        .unwrap_or("");
    let right = status_right(app);
    let rw: usize = right
        .spans
        .iter()
        .map(|span| format::width(&span.content))
        .sum();
    // The right side is cut only when the slab is narrower than it;
    // the left side then drops.
    let (left, right, lw) = if rw > avail {
        (
            cut_row(
                Joined {
                    spans: Vec::new(),
                    spots: Vec::new(),
                },
                1,
            ),
            cut_row(right, avail.saturating_add(1)),
            0,
        )
    } else {
        // The two sides keep at least two columns between them; past
        // that the left side drops.
        let room = avail.saturating_sub(rw).saturating_sub(2);
        let left = if room == 0 && rw > 0 {
            cut_row(
                Joined {
                    spans: Vec::new(),
                    spots: Vec::new(),
                },
                1,
            )
        } else {
            status_left(workspace, room)
        };
        let lw: usize = left.line.width();
        (left, cut_row(right, rw.saturating_add(1)), lw)
    };
    RowSides {
        left,
        right,
        lw,
        right_w: rw.min(avail),
    }
}

/// Draws the input box's status row on the row above `bottom`: the
/// working directory and the permission mode on the left; the model,
/// the thinking level and the ctx segment right-aligned, ending one
/// column short of the slab's edge, with at least two columns between
/// the sides. Too wide, the working directory is cut from the left
/// first, then the left side is cut at its right end; the right side
/// is cut only when the slab is narrower than it, at the slab's right
/// edge (`docs/tui.md`, "The input box"). Moves `bottom` above it.
fn put_status_row(
    app: &App,
    area: Rect,
    bottom: &mut u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) {
    let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
        return;
    };
    let start = area.x.saturating_add(1);
    let end = area.right().saturating_sub(1);
    let avail = usize::from(end.saturating_sub(start));
    if avail == 0 {
        return;
    }
    let RowSides {
        left,
        right,
        lw,
        right_w,
    } = row_sides(app, avail);
    let right_x = end.saturating_sub(u16::try_from(right_w).unwrap_or(u16::MAX));
    buf.set_line(start, y, &left.line, u16::try_from(lw).unwrap_or(u16::MAX));
    buf.set_line(
        right_x,
        y,
        &right.line,
        u16::try_from(right_w).unwrap_or(u16::MAX),
    );
    *bottom = y;
    for (at, wide, spot) in left.spots {
        targets.push(Target {
            id: TargetId::Panel(spot),
            rect: Rect::new(start.saturating_add(at), y, wide, 1),
        });
    }
    for (at, wide, spot) in right.spots {
        targets.push(Target {
            id: TargetId::Panel(spot),
            rect: Rect::new(right_x.saturating_add(at), y, wide, 1),
        });
    }
}

/// The status row's renderer input: the same figures the row draws,
/// folded the way its sides read them.
fn row_input(app: &App, flat: bool) -> InputStatus<'_> {
    let panel = app.panel_state();
    let status = panel.status();
    let known = app.session().and_then(|session| {
        let window = panel.window_for(session).filter(|window| *window > 0)?;
        let context = panel.status_for(session)?.context.as_ref()?;
        Some((window, context))
    });
    InputStatus {
        workspace: status.map(|status| status.workspace.as_str()),
        model: panel
            .model()
            .or_else(|| status.map(|status| status.model.as_str())),
        thinking: panel.thinking(),
        context: known.map(|(window, context)| Context {
            tokens: context.tokens,
            window,
        }),
        flat,
    }
}

/// The built-in status row as extension spans: the laid sides joined
/// into one span line, the gap between them blank. The paint below
/// draws it; the renderer sees the same cut.
fn row_builtin_spans(app: &App, width: u16) -> Vec<Vec<RawSpan>> {
    let avail = usize::from(width.saturating_sub(2));
    if avail == 0 {
        return Vec::new();
    }
    let RowSides {
        left,
        right,
        lw,
        right_w,
    } = row_sides(app, avail);
    let mut spans = left.line.spans;
    spans.extend(std::iter::repeat_n(
        Span::raw(" ".to_owned()),
        avail.saturating_sub(lw).saturating_sub(right_w),
    ));
    spans.extend(right.line.spans);
    vec![crate::ext::spans::from_ratatui(&spans)]
}

/// Draws the input box's status row through its slot: a replacement's
/// first line over the row's columns, further lines dropped; an empty
/// replacement leaves the row blank. With no replacement the built-in
/// row draws with its targets. Moves `bottom` above the row.
fn paint_row(app: &App, area: Rect, bottom: &mut u16, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let bound = super::notice_rows::frame_bound(app);
    let folded = row_input(app, bound.flat);
    let builtin = &|_: &Input<'_>, width: u16| row_builtin_spans(app, width);
    let Some(fitted) = bound.slot_lines(
        SlotDraw::Paint,
        "input_status",
        &Input::InputStatus(folded),
        area.width,
        builtin,
    ) else {
        put_status_row(app, area, bottom, buf, targets);
        return;
    };
    let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
        return;
    };
    // An empty replacement leaves the row blank; the rule stays.
    *bottom = y;
    let Some(first) = fitted.first() else {
        return;
    };
    // A replacement keeps one target, the Session status view, over the
    // row's full width (`docs/tui.md`, "Extension seams"): whatever the
    // replacement draws, a click on the row opens the Session status
    // card's view, as the working directory and the mode do.
    let x = area.x.saturating_add(1);
    let wide = area.width.saturating_sub(1);
    buf.set_line(x, y, &first.line, wide);
    targets.push(Target {
        id: TargetId::Panel(Spot::Status),
        rect: Rect::new(x, y, wide, 1),
    });
}

/// Draws the rule above the status row: `─` in `rule` from the stripe's
/// next column to one column short of the slab's right edge
/// (`docs/tui.md`, "The input box"). Moves `bottom` above it.
fn put_rule(area: Rect, bottom: &mut u16, buf: &mut Buffer) {
    let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
        return;
    };
    let start = area.x.saturating_add(1);
    let end = area.right().saturating_sub(1);
    let avail = usize::from(end.saturating_sub(start));
    if avail == 0 {
        return;
    }
    buf.set_stringn(start, y, "─".repeat(avail), avail, style(Role::Rule));
    *bottom = y;
}

/// Puts one draft row on the row above `bottom` at the text columns,
/// moving `bottom` up to it; nothing once `bottom` reaches the top of
/// `area`. The prompt opens the first draft row in `info`, and the rest
/// of every row is the default foreground (`docs/tui.md`, "The input
/// box").
fn put_row(
    buf: &mut Buffer,
    area: Rect,
    text: Rect,
    bottom: &mut u16,
    row: &str,
    prompt: bool,
    dim: bool,
) {
    let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
        return;
    };
    // While the rail holds focus the text carries DIM, in every mode.
    let text_style = if dim {
        Style::new().add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };
    if prompt {
        // The prompt is the row's first two cells (`Draft::rows`).
        let cut: String = row.chars().take(2).collect();
        let rest: String = row.chars().skip(2).collect();
        let prompt_style = if dim {
            style(Role::Info).add_modifier(Modifier::DIM)
        } else {
            style(Role::Info)
        };
        let (end, _) = buf.set_stringn(text.x, y, &cut, usize::from(text.width), prompt_style);
        let room = text.width.saturating_sub(end.saturating_sub(text.x));
        buf.set_stringn(end, y, &rest, usize::from(room), text_style);
    } else {
        buf.set_stringn(text.x, y, row, usize::from(text.width), text_style);
    }
    *bottom = y;
}

/// Whether the input box draws its cursor: the native cursor shows on
/// the box exactly then (`super::cursor`).
fn cursor_shown(app: &App) -> bool {
    app.chrome().floor_line().is_none()
        && !app.swapped_view_open()
        && app.focused().is_none()
        && app.panel().is_none()
        && !app.offer_open()
        && app.find_bar().is_none()
}

/// The caret's cell in `area`: the cursor's column past the stripe and
/// gap, on the cursor's shown row above `below` (`docs/tui.md",
/// "The input box"). Without colour the text sits one column in, past
/// the box's left side.
fn caret_cell(area: Rect, below: u16, rows: &[String], row: usize, col: u16) -> (u16, u16) {
    let text = text_area(area);
    (
        text.x.saturating_add(col),
        below
            .saturating_sub(to_u16(rows.len()))
            .saturating_add(to_u16(row)),
    )
}

/// The input box's shown rows, the draft row they start at, and the
/// cursor's row in them and column: the draft wrapped past the stripe
/// and gap, at most [`App::input_height`] rows, scrolled so the cursor's
/// row shows.
pub(super) fn rows(app: &App, width: u16) -> (Vec<String>, usize, usize, u16) {
    let width = surface::inset(width);
    let draft = app.input();
    let (row, col) = draft.cursor(width);
    let height = app.input_height();
    let top = row.saturating_add(1).saturating_sub(height);
    let rows = draft
        .rows(width)
        .into_iter()
        .skip(top)
        .take(height)
        .collect();
    (rows, top, row.saturating_sub(top), col)
}

/// Draws the editing hint at the box's right end on its last text row,
/// right-aligned and dim: only over cells past the draft's text and never
/// over the caret, so a long draft clips it at the right edge instead of
/// covering text (`docs/tui.md`, "Steering").
fn draw_editing_hint(
    area: Rect,
    below: u16,
    rows: &[String],
    (cursor_row, cursor_col): (usize, u16),
    buf: &mut Buffer,
) {
    draw_hint(
        area,
        below,
        rows,
        (cursor_row, cursor_col),
        EDITING_HINT,
        buf,
    );
}

/// Draws `hint` at the box's right end on its last text row,
/// right-aligned and dim: only over cells past the draft's text and never
/// over the caret, so a long draft clips it at the right edge instead of
/// covering text (`docs/tui.md`, "Steering").
fn draw_hint(
    area: Rect,
    below: u16,
    rows: &[String],
    (cursor_row, cursor_col): (usize, u16),
    hint: &str,
    buf: &mut Buffer,
) {
    let Some(last) = rows.last() else {
        return;
    };
    let Some(y) = below.checked_sub(1).filter(|y| *y >= area.y) else {
        return;
    };
    let text = text_area(area);
    let end = text.x.saturating_add(to_u16(format::width(last)));
    let caret = caret_cell(area, below, rows, cursor_row, cursor_col);
    // Without colour the right column is the box's side: the hint keeps
    // inside the interior, never covering the border.
    let right = if surface::box_fit(area.width) {
        area.right().saturating_sub(1)
    } else {
        area.right()
    };
    // The hint draws right-aligned only over free cells: past the
    // draft's text, never over the caret. Cells past the box clip in
    // the buffer write.
    let mut x = right.saturating_sub(to_u16(format::width(hint)));
    // Without colour the hint keeps clear of the box's right side; with
    // colour overlong hints still clip in the buffer write.
    let boxed = surface::box_fit(area.width);
    for ch in hint.chars() {
        let wide = to_u16(format::width(&ch.to_string()));
        if x >= end && (x, y) != caret && (!boxed || x < right) {
            buf.set_stringn(x, y, ch.to_string(), usize::from(wide), style(Role::Muted));
        }
        x = x.saturating_add(wide.max(1));
    }
}
/// The cursor's rows above the column's bottom and its column: the shown
/// rows below the cursor's with the rule, the status row, the kept
/// status lines and the bottom edge below them, so the cursor sits in
/// the same column as without edges, past the stripe and gap
/// (`docs/tui.md`, "Look"). Without colour the text sits one column in,
/// past the box's left side.
pub(super) fn cursor_row(app: &App, width: u16, room: usize) -> (u16, u16) {
    let (rows, _, row, col) = rows(app, width);
    let edges = usize::from(surface::edged(rows.len(), room) > rows.len());
    let indent = if surface::box_fit(width) {
        1
    } else {
        width.saturating_sub(surface::inset(width))
    };
    // The rule and the status row sit under the draft; the kept status
    // lines sit under them.
    let chrome = 2usize.saturating_add(super::status_rows::kept(app, width));
    (
        to_u16(
            rows.len()
                .saturating_sub(row)
                .saturating_add(edges)
                .saturating_add(chrome),
        ),
        col.saturating_add(indent),
    )
}

/// Paints the draft's selection over the box's shown rows: the same
/// `selection` tint every surface uses (`docs/tui.md`, "Selection and
/// copy"). Draft row `top` is the first shown; the shown rows end on the
/// row above `below`. Notes the shown text slab for the press and drag
/// mapping, so a click outside the box never claims it.
fn draw_selection(
    app: &App,
    area: Rect,
    below: u16,
    rows: &[String],
    top: usize,
    buf: &mut Buffer,
) {
    let text = text_area(area);
    let shown = to_u16(rows.len());
    let first = below.saturating_sub(shown).max(area.y);
    let height = area.bottom().saturating_sub(first).min(shown);
    // Always noted: an empty note claims nothing — empty rows hold
    // no pieces, and a zero-height slab holds no cell.
    app.note_input_area(Rect::new(text.x, first, text.width, height), top);
    // The selection's rects read draft rows from the column's first row:
    // the shown slab clips them, so scrolled rows paint nothing.
    let base = below.saturating_sub(shown).saturating_sub(to_u16(top));
    let full = Rect::new(text.x, base, text.width, to_u16(top).saturating_add(shown));
    let slab = Rect::new(text.x, first, text.width, height);
    let tint = super::marks::selection_style();
    for rect in app.selection_cells(crate::app::SurfaceId::Input, full) {
        // An empty intersection paints no cell.
        let clipped = rect.intersection(slab);
        buf.set_style(clipped, tint);
    }
}

/// Pushes a target over the cells of each paste token's label the input
/// box shows: the box's `shown` rows, from draft row `top`, end on the row
/// above `below`.
fn token_targets(
    app: &App,
    area: Rect,
    below: u16,
    (top, shown): (usize, usize),
    targets: &mut Vec<Target>,
) {
    let text = text_area(area);
    for span in app.input().token_spans(text.width) {
        let Some(up) = span
            .row
            .checked_sub(top)
            .and_then(|at| shown.checked_sub(at))
            .filter(|up| *up > 0)
        else {
            continue;
        };
        let Some(y) = below.checked_sub(to_u16(up)).filter(|y| *y >= area.y) else {
            continue;
        };
        let end = span.end.min(text.width);
        if span.start < end {
            targets.push(Target {
                id: TargetId::Token(span.number),
                rect: Rect::new(text.x.saturating_add(span.start), y, end - span.start, 1),
            });
        }
    }
}

#[cfg(test)]
#[path = "input_box_tests.rs"]
mod tests;
