//! Drawing the terminal: the conversation's styled lines, the notices over
//! it, the quit hint, the approval badge, the steering queue, and the input
//! box or the approval panel in its place (`docs/tui.md`, "Turns", "The
//! input box", "Steering", "Notices", "Approvals and questions"), with the
//! click target under the pointer tinted ("Mouse and hover").

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget, Wrap};

use crate::app::{App, QUIT_HINT};
use crate::mouse::{self, Target, TargetId};

/// The overlay shown while scrolled up once new output arrives.
const NEW_BELOW: &str = "↓ New messages below";

/// The approval panel's tint when a standing rule or review asked.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
pub(crate) const APPROVAL_TINT: Style = Style::new().bg(Color::Indexed(17));

/// The approval panel's tint when the reviewer escalated.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
pub(crate) const ALERT_TINT: Style = Style::new().bg(Color::Indexed(52));

/// A notice's tint.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const NOTICE_TINT: Style = Style::new().bg(Color::Indexed(236));

/// The background of the click target under the pointer.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
pub(crate) const HOVER_TINT: Style = Style::new().bg(Color::Indexed(238));

/// One line, wrapped the way it draws.
fn paragraph(line: Line<'_>) -> Paragraph<'_> {
    Paragraph::new(line).wrap(Wrap { trim: false })
}

/// How many rows `line` takes at `width`.
pub(crate) fn rows(line: Line<'_>, width: u16) -> usize {
    paragraph(line).line_count(width).max(1)
}

/// Draws `app` into `area` of `buf`, from the bottom up: the input box
/// on the last rows with a completion panel above it, or the approval panel
/// in their place, then the steering queue, the badge and the quit hint
/// when shown, and the conversation in the rows left with the notices
/// floating over its top-right corner, or the key map over them while it
/// is open. A screen too short for them all drops the hint first, then the
/// badge. A panel taller than the screen keeps its top.
///
/// Returns the click targets drawn, in draw order. Last, the target under
/// `pointer`, if any, gets [`HOVER_TINT`] as its background.
pub(crate) fn render(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    pointer: Option<(u16, u16)>,
) -> Vec<Target> {
    let mut targets = Vec::new();
    let mut bottom = area.bottom();
    if let Some(panel) = app.panel() {
        let height: usize = panel
            .lines
            .iter()
            .map(|line| rows(Line::raw(line.as_str()), area.width))
            .sum();
        let top = bottom.saturating_sub(to_u16(height)).max(area.y);
        let rect = Rect::new(area.x, top, area.width, bottom.saturating_sub(top));
        let tint = if panel.alert {
            ALERT_TINT
        } else {
            APPROVAL_TINT
        };
        buf.set_style(rect, tint);
        Paragraph::new(panel.lines.join("\n"))
            .wrap(Wrap { trim: false })
            .render(rect, buf);
        bottom = top;
    }
    if app.panel().is_none() {
        let (rows, top, _, _) = input_box(app, area.width);
        let below = bottom;
        for row in rows.iter().rev() {
            put(buf, area, &mut bottom, row, Style::default());
        }
        token_targets(app, area, below, (top, rows.len()), &mut targets);
        if let Some(completions) = app.completions() {
            for (at, line) in completions.lines.iter().enumerate().rev() {
                let style = if completions.selected == Some(at) {
                    Style::new().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                put(buf, area, &mut bottom, line, style);
            }
        }
    }
    // The steering queue sits above the input box, its newest row lowest;
    // a row a `steer` sent ends in a ✕ that drops it.
    let drops = app.steering_drops();
    for (at, row) in app.steering().iter().enumerate().rev() {
        let Some(rect) = put(buf, area, &mut bottom, row, Style::default()) else {
            continue;
        };
        targets.push(Target {
            id: TargetId::Steering(at),
            rect,
        });
        if drops.get(at) == Some(&true) && area.width > 0 {
            let close = Rect::new(area.right().saturating_sub(1), rect.y, 1, 1);
            buf.set_string(close.x, close.y, "✕", Style::default());
            targets.push(Target {
                id: TargetId::DropSteering(at),
                rect: close,
            });
        }
    }
    if let Some(rect) = app
        .badge()
        .and_then(|badge| put(buf, area, &mut bottom, &badge, Style::default()))
    {
        targets.push(Target {
            id: TargetId::Badge,
            rect,
        });
    }
    if app.hint() {
        put(buf, area, &mut bottom, QUIT_HINT, Style::default());
    }
    let rows = bottom.saturating_sub(area.y);
    let conversation = Rect::new(area.x, area.y, area.width, rows);
    match app.keymap_top() {
        Some(top) => Paragraph::new(crate::keymap::lines().join("\n"))
            .wrap(Wrap { trim: false })
            .scroll((to_u16(top), 0))
            .render(conversation, buf),
        None => {
            conversation_rows(app, conversation, buf, &mut targets);
            notices(app, conversation, buf, &mut targets);
        }
    }
    if let Some(id) = pointer.and_then(|(col, row)| mouse::hit(&targets, col, row)) {
        for target in targets.iter().filter(|target| target.id == id) {
            buf.set_style(target.rect, HOVER_TINT);
        }
    }
    targets
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
    for span in app.input().token_spans(area.width) {
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
        let end = span.end.min(area.width);
        if span.start < end {
            targets.push(Target {
                id: TargetId::Token(span.number),
                rect: Rect::new(area.x.saturating_add(span.start), y, end - span.start, 1),
            });
        }
    }
}

/// Puts `text` on the row above `bottom` and moves `bottom` up to it,
/// returning the cells its text took; nothing once `bottom` reaches the top
/// of `area`.
fn put(buf: &mut Buffer, area: Rect, bottom: &mut u16, text: &str, style: Style) -> Option<Rect> {
    let row = bottom.checked_sub(1).filter(|row| *row >= area.y)?;
    let (end, _) = buf.set_stringn(area.x, row, text, usize::from(area.width), style);
    *bottom = row;
    Some(Rect::new(area.x, row, end.saturating_sub(area.x), 1))
}

/// Floats the notices over the conversation's top-right corner, newest on
/// top, each a click target with its ✕ a target over it, or the notice
/// overlay over the whole conversation while it is open, which hides the
/// conversation's targets.
fn notices(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let mut y = area.y;
    if let Some(texts) = app.notice_overlay() {
        targets.retain(|target| !matches!(target.id, TargetId::Line(_) | TargetId::NewBelow));
        let rows: Vec<String> = texts
            .iter()
            .flat_map(|text| crate::format::wrap(text, usize::from(area.width)))
            .collect();
        for row in rows {
            if y >= area.bottom() {
                return;
            }
            let width = usize::from(area.width);
            let blank = " ".repeat(width);
            buf.set_stringn(area.x, y, blank, width, NOTICE_TINT);
            buf.set_stringn(area.x, y, &row, width, NOTICE_TINT);
            y = y.saturating_add(1);
        }
        return;
    }
    for notice in app.notices() {
        let top = y;
        let mut wide = 0;
        for row in &notice.rows {
            if y >= area.bottom() {
                break;
            }
            wide = to_u16(crate::format::width(row)).min(area.width);
            let x = area.right().saturating_sub(wide);
            buf.set_stringn(x, y, row, usize::from(wide), NOTICE_TINT);
            y = y.saturating_add(1);
        }
        if y == top {
            return;
        }
        let x = area.right().saturating_sub(wide);
        let rect = Rect::new(x, top, wide, y.saturating_sub(top));
        let Some(id) = notice.id else {
            targets.push(Target {
                id: TargetId::MoreNotices,
                rect,
            });
            continue;
        };
        targets.push(Target {
            id: TargetId::Notice(id),
            rect,
        });
        // The ✕ ends the first row.
        let close = Rect::new(area.right().saturating_sub(1), top, 1, 1);
        targets.push(Target {
            id: TargetId::DismissNotice(id),
            rect: close,
        });
    }
}

/// The input box's shown rows, the draft row they start at, and the
/// cursor's row in them and column: at most [`App::input_height`] rows,
/// scrolled so the cursor's row shows.
fn input_box(app: &App, width: u16) -> (Vec<String>, usize, usize, u16) {
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

/// Where the terminal cursor shows: at the draft's cursor while the input
/// box has focus, `None` while the approval panel is open or the cursor's
/// row is off a screen too short for it.
pub(crate) fn cursor(app: &App, area: Rect) -> Option<Position> {
    if app.panel().is_some() {
        return None;
    }
    let (rows, _, row, col) = input_box(app, area.width);
    let below = to_u16(rows.len().saturating_sub(row));
    let y = area.bottom().checked_sub(below).filter(|y| *y >= area.y)?;
    let x = area.x.saturating_add(col.min(area.width.saturating_sub(1)));
    Some(Position::new(x, y))
}

/// Draws the conversation's visible rows, bottom-aligned while it is
/// shorter than its area. Pushes a target over the rows shown of each line
/// that opens something, then one over the cells "↓ New messages below"
/// took, when drawn; the overlay's row is no line's target.
fn conversation_rows(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let lines = app.lines();
    let heights: Vec<usize> = lines
        .iter()
        .map(|line| rows(line.clone(), area.width))
        .collect();
    let total: usize = heights.iter().sum();
    let height = usize::from(area.height);
    let bottom_top = total.saturating_sub(height);
    let top = app.top().map_or(bottom_top, |top| top.min(bottom_top));
    let end = top.saturating_add(height);
    let shown = total.min(end).saturating_sub(top);
    let mut y = area.y.saturating_add(to_u16(height.saturating_sub(shown)));
    let mut start = 0usize;
    let overlay = app.has_new() && area.height > 0;
    let last = area.bottom().saturating_sub(u16::from(overlay));
    let mut opens = app.targets().into_iter().peekable();
    // A line wholly above `top` or below `end` shows no rows.
    for (at, (line, rows)) in lines.into_iter().zip(heights).enumerate() {
        let next = start.saturating_add(rows);
        let skip = top.saturating_sub(start);
        let count = next.min(end).saturating_sub(start.max(top));
        let rect = Rect::new(area.x, y, area.width, to_u16(count));
        paragraph(line).scroll((to_u16(skip), 0)).render(rect, buf);
        if let Some((_, open)) = opens.next_if(|(index, _)| *index == at) {
            let height = rect.height.min(last.saturating_sub(rect.y));
            if height > 0 {
                targets.push(Target {
                    id: TargetId::Line(open),
                    rect: Rect { height, ..rect },
                });
            }
        }
        y = y.saturating_add(to_u16(count));
        start = next;
    }
    if overlay {
        let row = area.bottom().saturating_sub(1);
        let blank = " ".repeat(usize::from(area.width));
        buf.set_stringn(
            area.x,
            row,
            blank,
            usize::from(area.width),
            Style::default(),
        );
        let width = to_u16(NEW_BELOW.chars().count());
        let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
        let (end, _) =
            buf.set_stringn(x, row, NEW_BELOW, usize::from(area.width), Style::default());
        targets.push(Target {
            id: TargetId::NewBelow,
            rect: Rect::new(x, row, end.saturating_sub(x), 1),
        });
    }
}

/// Where the conversation is scrolled to.
#[derive(Debug, Default)]
pub(crate) struct Scroll {
    /// The top wrapped row while scrolled up; `None` follows new output.
    pub(crate) top: Option<usize>,
    /// New output arrived while scrolled up.
    pub(crate) has_new: bool,
}

impl Scroll {
    /// New output while scrolled up shows the overlay; the view stays put.
    pub(crate) fn changed(&mut self) {
        if self.top.is_some() {
            self.has_new = true;
        }
    }

    /// PageUp: up by `step` from the top row, or from `bottom` when
    /// following.
    pub(crate) fn up(&mut self, step: usize, bottom: usize) {
        let top = self.top.unwrap_or(bottom);
        self.top = Some(top.saturating_sub(step));
    }

    /// PageDown: down by `step`, following again on reaching `bottom`.
    pub(crate) fn down(&mut self, step: usize, bottom: usize) {
        let Some(top) = self.top else {
            return;
        };
        let next = top.saturating_add(step);
        if next >= bottom {
            self.follow();
        } else {
            self.top = Some(next);
        }
    }

    /// End jumps to the bottom and resumes following.
    pub(crate) fn follow(&mut self) {
        self.top = None;
        self.has_new = false;
    }
}

/// A row count as a screen coordinate; a screen is never taller than
/// `u16::MAX`.
fn to_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// The screen as text, one row per line, trailing spaces trimmed.
pub(crate) fn text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut row = String::new();
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
