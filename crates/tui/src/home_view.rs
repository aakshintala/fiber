//! Home's frame: the logo, the input box with its chips, the session
//! list under it, and the foot hint (`docs/tui.md`, "Home"). Each width is
//! picked, not measured: the box is at most 100 columns, centred.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

use super::{FOCUS_STYLE, HOVER_TINT};
use crate::app::App;
use crate::format::{cut, width};
use crate::home::{HomeScreen, Spot};
use crate::markdown::{Role, style};
use crate::mouse::{self, Target, TargetId};

#[path = "logo.rs"]
mod logo;

/// The home input box's tint.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const BOX_TINT: Style = Style::new().bg(Color::Indexed(235));

/// The home input box's half-block edges, in the box tint's colour.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const BOX_EDGE: Style = Style::new().fg(Color::Indexed(235));

/// The input box's placeholder, after `> `, while the draft is empty and
/// no `start` went out in this run.
const PLACEHOLDER: &str = "> /? for shortcuts";

/// The box's width: at most 100 columns, centred.
const BOX_WIDTH: u16 = 100;

/// The draft rows the box shows: at least 3, at most 8.
const MIN_BOX: usize = 3;
const MAX_BOX: usize = 8;

/// Where home's rows draw.
struct Placed {
    /// The box's first column and width.
    x: u16,
    width: u16,
    /// The logo's first row and its rows: four pixel rows, or one.
    logo: u16,
    logo_rows: usize,
    /// The ▄ edge row.
    box_top: u16,
    /// The first draft row.
    draft_top: u16,
    /// The draft rows shown, from draft row `top`.
    shown: Vec<String>,
    top: usize,
    /// The chip row and the ▀ edge row.
    chip: u16,
    edge: u16,
    /// The foot hint's row.
    foot: u16,
}

/// Lays home out in `area`: the pad, the logo, one blank row, the box,
/// the list, and the foot hint on the last row. The logo is four pixel
/// rows when they fit with three list rows kept, else the one-row form.
fn layout(app: &App, screen: &HomeScreen, area: Rect) -> Placed {
    let width = area.width.min(BOX_WIDTH);
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let pad = area.height / 8;
    let draft = app.input().rows(width);
    let height = draft.len().clamp(MIN_BOX, MAX_BOX);
    // The box is its ▄ edge, the draft rows, the chip row and its ▀ edge.
    let boxed = super::to_u16(height).saturating_add(3);
    // Four rows need the pad, the four-row logo, one blank row, the
    // box, three list rows and the foot, and the logo's width.
    let need = pad + 4 + boxed + 3 + 1;
    let four =
        area.height > need && area.width >= super::to_u16(logo::width_cells(&screen.version));
    let logo_rows = if four { 4 } else { 1 };
    let logo = area.y.saturating_add(pad);
    let box_top = logo
        .saturating_add(super::to_u16(logo_rows))
        .saturating_add(1);
    let (row, _) = app.input().cursor(width);
    let top = row.saturating_add(1).saturating_sub(height);
    let shown = draft.into_iter().skip(top).take(height).collect();
    let draft_top = box_top.saturating_add(1);
    let chip = draft_top.saturating_add(super::to_u16(height));
    let edge = chip.saturating_add(1);
    let foot = area.bottom().saturating_sub(1);
    Placed {
        x,
        width,
        logo,
        logo_rows,
        box_top,
        draft_top,
        shown,
        top,
        chip,
        edge,
        foot,
    }
}

/// Writes `text` at `x`, `y`, cut to `width`, or nothing below the area.
fn put(buf: &mut Buffer, area: Rect, x: u16, y: u16, text: &str, max: u16, text_style: Style) {
    if y >= area.bottom() {
        return;
    }
    buf.set_stringn(
        x,
        y,
        cut(text, usize::from(max)),
        usize::from(max),
        text_style,
    );
}

/// Draws home's `screen` into `area` of `buf`: the one-row logo, the box
/// with the completion panel above it, the session list under the box,
/// and the foot hint on the last row, with the notices top-right.
/// Returns the click targets drawn; last, as on the conversation screen,
/// the target under `pointer` gets [`HOVER_TINT`] and the focused target
/// gets [`FOCUS_STYLE`].
pub(super) fn render(
    app: &App,
    screen: &HomeScreen,
    area: Rect,
    buf: &mut Buffer,
    pointer: Option<(u16, u16)>,
) -> Vec<Target> {
    if area.is_empty() {
        return Vec::new();
    }
    let mut targets = Vec::new();
    let placed = layout(app, screen, area);
    if placed.logo_rows == 4 {
        // The four-row logo is pixel letters drawn with half blocks,
        // centred, with the version dim on its fourth row.
        let cells = super::to_u16(logo::width_cells(&screen.version));
        let at = placed
            .x
            .saturating_add(placed.width.saturating_sub(cells) / 2);
        logo::draw(buf, at, placed.logo, &screen.version);
    } else {
        // The logo is `⌇ fiber <version>`: the ⌇ and the name in the accent
        // colour, the version dim.
        let name = format!("{} fiber ", screen.glyph);
        let at = placed.x.saturating_add(
            placed.width.saturating_sub(super::to_u16(
                width(&name).saturating_add(width(&screen.version)),
            )) / 2,
        );
        if placed.logo < area.bottom() {
            let (end, _) = buf.set_stringn(
                at,
                placed.logo,
                &name,
                usize::from(area.width),
                style(Role::Accent),
            );
            buf.set_stringn(
                end,
                placed.logo,
                &screen.version,
                usize::from(area.width),
                Style::new().add_modifier(Modifier::DIM),
            );
        }
    }
    put(
        buf,
        area,
        placed.x,
        placed.box_top,
        &"▄".repeat(usize::from(placed.width)),
        placed.width,
        BOX_EDGE,
    );
    for (at, row) in placed.shown.iter().enumerate() {
        let y = placed.draft_top.saturating_add(super::to_u16(at));
        if screen.placeholder && placed.top == 0 && at == 0 {
            // The placeholder is one style, so its letters are written
            // together.
            put(
                buf,
                area,
                placed.x,
                y,
                PLACEHOLDER,
                placed.width,
                BOX_TINT.add_modifier(Modifier::DIM),
            );
        } else {
            put(buf, area, placed.x, y, row, placed.width, BOX_TINT);
        }
    }
    put(
        buf,
        area,
        placed.x,
        placed.chip,
        &screen.chips.join("  "),
        placed.width,
        BOX_TINT,
    );
    put(
        buf,
        area,
        placed.x,
        placed.edge,
        &"▀".repeat(usize::from(placed.width)),
        placed.width,
        BOX_EDGE,
    );
    // The session list draws under the box, down to the row above the
    // foot, scrolling past the screen. Each row is a target opening its
    // session. The focused row is the last drawn one once it is past
    // the first screenful, and the list draws from the top otherwise.
    let list_top = placed.edge.saturating_add(1);
    let capacity = usize::from(placed.foot.saturating_sub(list_top));
    let start = match app.focused() {
        Some(TargetId::Home(Spot::Entry(key))) => screen
            .rows
            .iter()
            .position(|(row, _, _)| *row == key)
            .map_or(0, |at| (at + 1).saturating_sub(capacity)),
        _ => 0,
    };
    let mut row_y = list_top;
    for (key, text, _has_x) in screen.rows.iter().skip(start) {
        if row_y >= placed.foot {
            break;
        }
        put(
            buf,
            area,
            placed.x,
            row_y,
            text,
            placed.width,
            Style::default(),
        );
        targets.push(Target {
            id: TargetId::Home(Spot::Entry(*key)),
            rect: Rect::new(placed.x, row_y, placed.width, 1),
        });
        row_y = row_y.saturating_add(1);
    }
    if let Some(completions) = app.completions() {
        let mut bottom = placed.box_top;
        for (at, line) in completions.lines.iter().enumerate().rev() {
            let Some(row) = bottom.checked_sub(1).filter(|row| *row >= area.y) else {
                break;
            };
            bottom = row;
            let line_style = if completions.selected == Some(at) {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            put(buf, area, placed.x, row, line, placed.width, line_style);
        }
    }
    token_targets(app, area, &placed, &mut targets);
    // The foot hint sits on the last row, centred, cut to the width.
    let foot = &screen.foot;
    if placed.foot >= area.y {
        if width(foot) <= usize::from(area.width) {
            let at = area
                .x
                .saturating_add(area.width.saturating_sub(super::to_u16(width(foot))) / 2);
            put(
                buf,
                area,
                at,
                placed.foot,
                foot,
                area.width,
                Style::default(),
            );
        } else {
            put(
                buf,
                area,
                area.x,
                placed.foot,
                foot,
                area.width,
                Style::default(),
            );
        }
    }
    super::notices(
        app,
        Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1)),
        buf,
        &mut targets,
    );
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

/// A target over each paste token's label the home box shows.
fn token_targets(app: &App, area: Rect, placed: &Placed, targets: &mut Vec<Target>) {
    for span in app.input().token_spans(placed.width) {
        let Some(dy) = span
            .row
            .checked_sub(placed.top)
            .filter(|_| span.row < placed.top.saturating_add(placed.shown.len()))
        else {
            continue;
        };
        let y = placed.draft_top.saturating_add(super::to_u16(dy));
        if y >= area.bottom() {
            continue;
        }
        let end = span.end.min(placed.width);
        if span.start < end {
            targets.push(Target {
                id: TargetId::Token(span.number),
                rect: Rect::new(placed.x.saturating_add(span.start), y, end - span.start, 1),
            });
        }
    }
}

/// Where the terminal cursor shows on home: at the draft's cursor in the
/// box, `None` while navigating.
pub(super) fn cursor(app: &App, screen: &HomeScreen, area: Rect) -> Option<Position> {
    if app.focused().is_some() || area.is_empty() {
        return None;
    }
    let placed = layout(app, screen, area);
    let (row, col) = app.input().cursor(placed.width);
    let y = placed
        .draft_top
        .saturating_add(super::to_u16(row.saturating_sub(placed.top)));
    if y >= area.bottom() {
        return None;
    }
    let x = placed
        .x
        .saturating_add(col.min(placed.width.saturating_sub(1)));
    Some(Position::new(x, y))
}

#[cfg(test)]
#[path = "home_view_tests.rs"]
mod tests;
