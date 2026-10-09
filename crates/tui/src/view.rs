//! Drawing the terminal: the conversation's styled lines, the notices over
//! it, the quit hint, the approval badge, the steering queue, and the input
//! box or the approval panel in its place (`docs/tui.md`, "Turns", "The
//! input box", "Steering", "Notices", "Approvals and questions"), with the
//! click target under the pointer tinted ("Mouse and hover").

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget, Wrap};

use crate::app::App;
use crate::markdown::{Role, style};
use crate::mouse::{self, Target, TargetId};

mod banner;
pub(crate) mod chrome;
mod drag;
#[path = "home_view.rs"]
mod home;
mod input_box;
mod marks;
mod offer;
pub(crate) mod panel;
pub(crate) mod rail;
pub(crate) mod request;
mod results;
pub(crate) mod status_rows;
mod steering_queue;
mod working_line;

pub(crate) use home::max_question_scroll;

/// The overlay shown while scrolled up once new output arrives.
const NEW_BELOW: &str = "↓ New messages below";

/// Shown on the conversation's top row after a copy.
const COPIED: &str = "Copied";

/// The approval panel's tint when a standing rule or review asked.
pub(crate) const APPROVAL_TINT: Style = Style::new().bg(Role::Approval.color());

/// The approval panel's tint when the reviewer escalated.
pub(crate) const ALERT_TINT: Style = Style::new().bg(Role::Alert.color());

/// The input box's tint: its rows sit on the surface colour
/// (`docs/tui.md`, "Look").
pub(crate) const SURFACE_TINT: Style = Style::new().bg(Role::Surface.color());

/// A notice's tint.
const NOTICE_TINT: Style = Style::new().bg(Role::Surface.color());

/// The background of the click target under the pointer.
pub(crate) const HOVER_TINT: Style = Style::new().bg(Role::Hover.color());

/// The focused click target's style in navigate mode: reversed, so it
/// shows on every theme and with no colour.
pub(crate) const FOCUS_STYLE: Style = Style::new().add_modifier(Modifier::REVERSED);

/// One line, wrapped the way it draws.
pub(crate) fn paragraph(line: Line<'_>) -> Paragraph<'_> {
    Paragraph::new(line).wrap(Wrap { trim: false })
}

/// How many rows `line` takes at `width`: one when it fits, without
/// wrapping it.
pub(crate) fn rows(line: Line<'_>, width: u16) -> usize {
    if line.width() <= usize::from(width) {
        return 1;
    }
    paragraph(line).line_count(width).max(1)
}

/// Draws `app` into `area` of `buf`, from the bottom up: the narrow
/// layout's status rows on the last rows, then the input box with a
/// completion panel above it, or the approval panel in their place, then
/// the steering queue, the reconnect banner, the
/// badge and the quit hint when shown, and the conversation in the rows
/// left with the notices floating over its top-right corner, or the key
/// map over them while it is open. A screen too short for them all drops
/// the hint first, then the badge. A panel taller than the screen keeps
/// its top, except that a question form scrolls to keep its cursor's row
/// shown.
///
/// Returns the click targets drawn, in draw order. Last, the target under
/// `pointer`, if any, gets [`HOVER_TINT`] as its background.
pub(crate) fn render(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    pointer: Option<(u16, u16)>,
) -> Vec<Target> {
    // The frame's asks start here: `Screen::draw_with` may render twice,
    // and the loop takes what the drawn frame asked.
    app.motion().clear_wake();
    // Below the floor one line shows, home included; home draws while no
    // session is on screen; the conversation draws in its column once one
    // attaches.
    if let Some(line) = app.chrome().floor_line() {
        chrome::floor(line, area, buf);
        return Vec::new();
    }
    // A configuration view takes home's place (`docs/tui.md`, "Swapped
    // views"). Notices float above it (`docs/tui.md`, "Notices"), as
    // over the conversation.
    if app.config_view_open() && app.on_home() {
        let mut targets = Vec::new();
        crate::swapped::draw(app, area, buf, &mut targets);
        notices(app, area, buf, &mut targets);
        if let Some(id) = pointer.and_then(|(col, row)| mouse::hit(&targets, col, row)) {
            for target in targets.iter().filter(|target| target.id == id) {
                buf.set_style(target.rect, HOVER_TINT);
            }
        }
        if let Some(id) = app.focused() {
            for target in targets.iter().filter(|target| target.id == id) {
                buf.set_style(target.rect, FOCUS_STYLE);
            }
        }
        return targets;
    }
    if let Some(screen) = app.home_screen() {
        return home::render(app, &screen, area, buf, pointer);
    }
    let area = match app.chrome().layout() {
        Some(layout) => chrome::draw(app, &layout, buf),
        None => area,
    };
    let mut targets = Vec::new();
    if let Some(rect) = app.chrome().layout().and_then(|layout| layout.panel) {
        panel::draw(app, rect, buf, &mut targets);
    }
    if let Some(rect) = app.chrome().layout().and_then(|layout| layout.rail) {
        rail::draw(app, rect, buf, pointer, &mut targets);
    }
    let mut bottom = area.bottom();
    status_rows::draw_status(app, area, buf, &mut bottom, &mut targets);
    if let Some(panel) = app.panel() {
        bottom = request::draw(&panel, area, bottom, buf, &mut targets);
    }
    if app.panel().is_none() {
        input_box::draw(app, area, &mut bottom, buf, &mut targets);
    }
    status_rows::draw_widget(app, area, buf, &mut bottom, &mut targets);
    // The steering queue sits above the input box, its newest row lowest;
    // a row a `steer` sent ends in a ✕ that drops it.
    steering_queue::draw(app, area, &mut bottom, buf, &mut targets);
    status_rows::draw_delegates(app, area, buf, &mut bottom);
    banner::draw(app, area, buf, &mut bottom);
    working_line::draw(app, area, buf, &mut bottom, &mut targets);
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
        put(buf, area, &mut bottom, &app.hint_text(), Style::default());
    }
    let rows = bottom.saturating_sub(area.y);
    let conversation = Rect::new(area.x, area.y, area.width, rows);
    match app.keymap_top() {
        Some(top) => {
            Paragraph::new(crate::keymap::lines(app.keys()).join("\n"))
                .wrap(Wrap { trim: false })
                .scroll((to_u16(top), 0))
                .render(conversation, buf);
            overlay_cross(buf, conversation, &mut targets);
        }
        // A configuration view swaps in for the conversation. Notices
        // float above it (`docs/tui.md`, "Notices"), as over the
        // conversation.
        None if app.config_view_open() => {
            crate::swapped::draw(app, conversation, buf, &mut targets);
            notices(app, conversation, buf, &mut targets);
        }
        None => {
            // The repository offer swaps in for the conversation.
            match app.offer_rows(conversation.width) {
                Some((rows, top)) => offer::render(&rows, top, conversation, buf, &mut targets),
                // The search results swap in for the conversation while
                // open, with the ✕ closing them (`docs/tui.md`, "Search",
                // "Swapped views").
                None => match app.find_results() {
                    Some(view) => {
                        results::render(&view, conversation, buf, &mut targets);
                        marks::draw(app, conversation, buf, &mut targets);
                        // The ✕ draws over the bar's last cell, so the
                        // view keeps a visible closer (`docs/tui.md`,
                        // "Swapped views").
                        overlay_cross(buf, conversation, &mut targets);
                    }
                    None => {
                        conversation_rows(app, conversation, buf, &mut targets);
                        marks::draw(app, conversation, buf, &mut targets);
                    }
                },
            }
            notices(app, conversation, buf, &mut targets);
        }
    }
    drag::draw(app, buf, pointer);
    if let Some(id) = pointer.and_then(|(col, row)| mouse::hit(&targets, col, row)) {
        for target in targets.iter().filter(|target| target.id == id) {
            buf.set_style(target.rect, HOVER_TINT);
        }
    }
    if let Some(id) = app.focused() {
        for target in targets.iter().filter(|target| target.id == id) {
            buf.set_style(target.rect, FOCUS_STYLE);
        }
    }
    targets
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

/// The open overlay's ✕ on the conversation's top-right cell, a click
/// target closing it; nothing when the conversation shows no rows.
fn overlay_cross(buf: &mut Buffer, area: Rect, targets: &mut Vec<Target>) {
    if area.is_empty() {
        return;
    }
    let cross = Rect::new(area.right().saturating_sub(1), area.y, 1, 1);
    buf.set_string(cross.x, cross.y, "✕", Style::default());
    targets.push(Target {
        id: TargetId::CloseOverlay,
        rect: cross,
    });
}
/// top, each a click target with its ✕ a target over it, or the notice
/// overlay over the whole conversation while it is open, which hides the
/// conversation's targets.
fn notices(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let mut y = area.y;
    if let Some(texts) = app.notice_overlay() {
        targets.retain(|target| {
            !matches!(
                target.id,
                TargetId::Line(_)
                    | TargetId::Link { .. }
                    | TargetId::FindResult(_)
                    | TargetId::NewBelow
                    | TargetId::Turn(_)
                    | TargetId::Offer(_)
            )
        });
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
        overlay_cross(buf, area, targets);
        return;
    }
    // "Copied" takes the corner's first row while it shows, below the
    // search bar while it is open (`docs/tui.md`, "Notices").
    y = y
        .saturating_add(u16::from(app.find_bar().is_some()))
        .saturating_add(u16::from(app.copied()));
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

/// Where the terminal cursor shows: at the draft's cursor while the input
/// box has focus, at a question form's text cursor while its words row has
/// the cursor, `None` while navigating, any other panel or the repository
/// offer is open, or the cursor's row is off a screen too short for it. A
/// shown form's caret wins over an open repository offer, which already
/// gives the panel its keys.
pub(crate) fn cursor(app: &App, area: Rect) -> Option<Position> {
    // A configuration view draws its own caret.
    if app.chrome().floor_line().is_some() || app.config_view_open() {
        return None;
    }
    if let Some(screen) = app.home_screen() {
        return home::cursor(app, &screen, area);
    }
    let area = app
        .chrome()
        .layout()
        .map_or(area, |layout| chrome::body(&layout));
    if app.focused().is_some() {
        return None;
    }
    // A shown panel wins over the offer: the panel takes the keys, so a
    // form's caret takes the cursor.
    if let Some(panel) = app.panel() {
        return request::caret(&panel, area, status_rows::input_bottom(app, area));
    }
    // The offer hides the draft's cursor while it swaps in for the
    // conversation.
    if app.offer_open() {
        return None;
    }
    // The search bar's cursor at its query's end, while it is open
    // (`docs/tui.md`, "Search").
    if let Some(bar) = app.find_bar() {
        return marks::bar_cursor(&bar, area);
    }
    let (below, col) = input_box::cursor_row(app, area.width, usize::from(area.height));
    let y = status_rows::input_bottom(app, area)
        .checked_sub(below)
        .filter(|y| *y >= area.y)?;
    let x = area.x.saturating_add(col.min(area.width.saturating_sub(1)));
    Some(Position::new(x, y))
}

/// Draws the conversation's visible rows, bottom-aligned while it is
/// shorter than its area. Only the pages that draw a visible row are read;
/// a page not loaded draws blank rows. Pushes a target over each shown line
/// that opens something, and over "↓ New messages below" when drawn.
fn conversation_rows(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let total = app.scroll().1;
    let height = usize::from(area.height);
    let bottom_top = total.saturating_sub(height);
    let top = app.top().map_or(bottom_top, |top| top.min(bottom_top));
    let end = top.saturating_add(height);
    let shown = total.min(end).saturating_sub(top);
    let mut y = area.y.saturating_add(to_u16(height.saturating_sub(shown)));
    let crate::window::Shown {
        first,
        lines,
        turns,
        spins,
    } = app.shown(top, height);
    let mut start = first;
    let overlay = app.has_new() && area.height > 0;
    let last = area.bottom().saturating_sub(u16::from(overlay));
    // Each line's cells on screen: its y and how many of its rows show.
    let mut layout: Vec<(u16, u16)> = Vec::with_capacity(lines.len());
    {
        let (mut row_start, mut line_y) = (start, y);
        for (_, rows, _) in &lines {
            let next = row_start.saturating_add(*rows);
            let count = next.min(end).saturating_sub(row_start.max(top));
            layout.push((line_y, to_u16(count)));
            line_y = line_y.saturating_add(to_u16(count));
            row_start = next;
        }
    }
    // Turns are focus stops over the rows their page cards draw; they are
    // not click targets, so the line targets drawn after them win clicks.
    for (at, range) in turns {
        let mut rect: Option<Rect> = None;
        for (line, (line_y, count)) in layout.iter().enumerate() {
            if range.contains(&line) {
                rect = Some(match rect {
                    Some(before) => Rect {
                        height: before.height.saturating_add(*count),
                        ..before
                    },
                    None => Rect::new(area.x, *line_y, area.width, *count),
                });
            }
        }
        if let Some(rect) = rect {
            let height = rect.height.min(last.saturating_sub(rect.y));
            if height > 0 {
                targets.push(Target {
                    id: TargetId::Turn(at),
                    rect: Rect { height, ..rect },
                });
            }
        }
    }
    // A line wholly above `top` or below `end` shows no rows.
    for (at, (line, rows, open)) in lines.into_iter().enumerate() {
        let next = start.saturating_add(rows);
        let skip = top.saturating_sub(start);
        let count = next.min(end).saturating_sub(start.max(top));
        let rect = Rect::new(area.x, y, area.width, to_u16(count));
        paragraph(line).scroll((to_u16(skip), 0)).render(rect, buf);
        // A marked line spins its cell on the working line's tick; the
        // mark is the builders' promise that the line moves.
        if let Some(col) = spins
            .iter()
            .find_map(|(marked, col)| (*marked == at).then_some(*col))
        {
            working_line::spin(
                app,
                buf,
                area,
                working_line::Drawn {
                    y,
                    col,
                    first_row_shown: skip == 0,
                    rows,
                    count,
                },
            );
        }
        if let Some(open) = open {
            let height = rect.height.min(last.saturating_sub(rect.y));
            // A code block's copy target is its `copy` cells, not the line.
            let cells = app.copy_cells(open).map_or(rect, |cols| Rect {
                x: area.x.saturating_add(cols.start),
                width: cols.end.saturating_sub(cols.start),
                ..rect
            });
            if height > 0 {
                targets.push(Target {
                    id: TargetId::Line(open),
                    rect: Rect { height, ..cells },
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
    if app.copied() && area.height > 0 {
        let width = to_u16(COPIED.len());
        let x = area.x.saturating_add(area.width.saturating_sub(width));
        let y = area.y.saturating_add(u16::from(app.find_bar().is_some()));
        let shown = usize::from(area.width);
        buf.set_stringn(x, y, COPIED, shown, style(Role::Accent));
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
