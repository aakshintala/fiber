//! Home's frame: the logo, the input box with its chips, the session
//! list under it, and the foot hint (`docs/tui.md`, "Home"). Each width is
//! picked, not measured: the box is at most 100 columns, centred.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::{FOCUS_STYLE, HOVER_TINT, completions};
use crate::app::App;
use crate::completion_rows::Rows;
use crate::format::{cut, width, wrap};
use crate::home::{HomeScreen, Spot};
use crate::markdown::{Role, style};
use crate::mouse::{self, Target, TargetId};

#[path = "logo.rs"]
mod logo;

/// The home input box's tint.
const BOX_TINT: Style = Style::new().bg(Role::Surface.color());

/// The input box's placeholder, after `› `, while the draft is empty and
/// no `start` went out in this run.
const PLACEHOLDER: &str = "› /? for shortcuts";

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
    /// The blocker lines above the box, wrapped at its width.
    blockers: Vec<String>,
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
    // A rejected `start` draws its message above the box, wrapped at its
    // width, staying while the list fills.
    let blockers: Vec<String> = screen
        .blockers
        .iter()
        .flat_map(|line| wrap(line, usize::from(width)))
        .collect();
    // Four rows need the pad, the four-row logo, one blank row, the
    // blocker lines, the box, three list rows and the foot, and the
    // logo's width.
    let need = pad + 4 + boxed + 3 + 1;
    let need = need.saturating_add(super::to_u16(blockers.len()));
    let four =
        area.height > need && area.width >= super::to_u16(logo::width_cells(&screen.version));
    let logo_rows = if four { 4 } else { 1 };
    let logo = area.y.saturating_add(pad);
    let box_top = logo
        .saturating_add(super::to_u16(logo_rows))
        .saturating_add(1)
        .saturating_add(super::to_u16(blockers.len()));
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
        blockers,
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

/// Writes one draft `row` at `x`, `y`, cut to `width`: the prompt in
/// `info`, the rest on the box's tint, dim while `dim` (`docs/tui.md`,
/// "The input box").
fn put_prompt(buf: &mut Buffer, area: Rect, x: u16, y: u16, row: &str, max: u16, dim: bool) {
    if y >= area.bottom() {
        return;
    }
    // The prompt is the row's first two cells (`Draft::rows`).
    let prompt: String = row.chars().take(2).collect();
    let rest: String = row.chars().skip(2).collect();
    let (end, _) = buf.set_stringn(
        x,
        y,
        cut(&prompt, usize::from(max)),
        usize::from(max),
        BOX_TINT.fg(Role::Info.color()),
    );
    let room = max.saturating_sub(end.saturating_sub(x));
    let rest_style = if dim {
        BOX_TINT.add_modifier(Modifier::DIM)
    } else {
        BOX_TINT
    };
    buf.set_stringn(
        end,
        y,
        cut(&rest, usize::from(room)),
        usize::from(room),
        rest_style,
    );
}

/// The rows the frame shows the delete question in: the list rows from
/// the box edge to the foot. Drawing clamps the stored offset to the
/// wrapped rows past these, and scrolling clamps the stored offset to
/// the same maximum, so Down past the end holds and one Up steps back
/// one row.
fn question_capacity(placed: &Placed) -> usize {
    usize::from(placed.foot.saturating_sub(placed.edge.saturating_add(1)))
}

/// The delete question's maximum scroll offset in `area`: its wrapped
/// rows past the rows the frame shows them in.
pub(crate) fn max_question_scroll(app: &App, screen: &HomeScreen, area: Rect) -> usize {
    let Some(question) = screen.question.as_ref() else {
        return 0;
    };
    let placed = layout(app, screen, area);
    wrap(question, usize::from(placed.width))
        .len()
        .saturating_sub(question_capacity(&placed))
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
        // colour, the version dim. The logo row always draws: the pad
        // keeps it inside the area.
        let name = format!("{} fiber ", screen.glyph);
        let at = placed.x.saturating_add(
            placed.width.saturating_sub(super::to_u16(
                width(&name).saturating_add(width(&screen.version)),
            )) / 2,
        );
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
    // The blocker lines sit above the box, wrapped at its width.
    let mut blocker_y = placed
        .logo
        .saturating_add(super::to_u16(placed.logo_rows))
        .saturating_add(1);
    for line in &placed.blockers {
        put(
            buf,
            area,
            placed.x,
            blocker_y,
            line,
            placed.width,
            Style::default(),
        );
        blocker_y = blocker_y.saturating_add(1);
    }
    // The box's edges above and below its draft and chip rows, with the
    // rows' surface tint edge to edge, past the placeholder or the
    // written text (`docs/tui.md`, "Look", "The input box").
    crate::surface::draw_slab(
        buf,
        ratatui::layout::Rect::new(
            placed.x,
            placed.draft_top,
            placed.width,
            placed
                .chip
                .saturating_sub(placed.draft_top)
                .saturating_add(1),
        ),
        Role::Surface,
        None,
        crate::surface::Edges::BOTH,
    );
    for (at, row) in placed.shown.iter().enumerate() {
        let y = placed.draft_top.saturating_add(super::to_u16(at));
        if screen.placeholder && placed.top == 0 && at == 0 {
            // The placeholder's prompt is `info` like a draft's; the rest
            // stays dim (`docs/tui.md`, "The input box").
            put_prompt(buf, area, placed.x, y, PLACEHOLDER, placed.width, true);
        } else {
            put_prompt(buf, area, placed.x, y, row, placed.width, false);
        }
    }
    // The cursor is a drawn dim `█` at the draft's cursor while nothing
    // takes the keyboard (`docs/tui.md`, "The input box"). The
    // terminal's own cursor stays where `cursor` puts it.
    if app.focused().is_none() {
        let (row, col) = app.input().cursor(placed.width);
        let y = placed
            .draft_top
            .saturating_add(super::to_u16(row.saturating_sub(placed.top)));
        let x = placed
            .x
            .saturating_add(col.min(placed.width.saturating_sub(1)));
        // The draft sits below the area's top row by construction, so
        // only the bottom needs the guard.
        if y < area.bottom() {
            buf.set_stringn(x, y, "█", 1, style(Role::Muted));
        }
    }
    // The chip row: the workspace, the model, the thinking level, and
    // what Enter does. The workspace chip opens the picker.
    let texts: Vec<&str> = screen.chips.iter().map(|(_, text)| text.as_str()).collect();
    put(
        buf,
        area,
        placed.x,
        placed.chip,
        &texts.join("  "),
        placed.width,
        BOX_TINT,
    );
    let mut chip_x = placed.x;
    for (at, (spot, text)) in screen.chips.iter().enumerate() {
        if at != 0 {
            chip_x = chip_x.saturating_add(2);
        }
        let wide = super::to_u16(width(text));
        if let Some(spot) = spot {
            targets.push(Target {
                id: TargetId::Home(*spot),
                rect: Rect::new(chip_x, placed.chip, wide, 1),
            });
        }
        chip_x = chip_x.saturating_add(wide);
    }
    // The session list draws under the box, down to the row above the
    // foot, scrolling past the screen. The scope toggle heads it while
    // it shows; each row is a target opening its session. The focused
    // row is the last drawn one once it is past the first screenful,
    // and the list draws from the top otherwise.
    let list_top = placed.edge.saturating_add(1);
    let capacity = question_capacity(&placed);
    let mut row_y = list_top;
    // The delete question wraps over as many rows as needed, naming
    // the session and everything `--cascade` adds, taking priority
    // space over the list; Up and Down scroll it past the screen.
    let mut asked = 0;
    if let Some(question) = &screen.question {
        let wrapped = wrap(question, usize::from(placed.width));
        let max = wrapped.len().saturating_sub(capacity);
        let scroll = screen.question_scroll.min(max);
        asked = wrapped.len().saturating_sub(scroll).min(capacity);
        for (at, row) in wrapped.iter().skip(scroll).take(asked).enumerate() {
            put(
                buf,
                area,
                placed.x,
                row_y.saturating_add(super::to_u16(at)),
                row,
                placed.width,
                Style::default(),
            );
        }
        row_y = row_y.saturating_add(super::to_u16(asked));
    }
    if let Some(toggle) = &screen.toggle
        && screen.question.is_none()
        && row_y < placed.foot
    {
        put(
            buf,
            area,
            placed.x,
            row_y,
            toggle,
            placed.width,
            Style::default(),
        );
        targets.push(Target {
            id: TargetId::Home(Spot::Toggle),
            rect: Rect::new(placed.x, row_y, placed.width, 1),
        });
        row_y = row_y.saturating_add(1);
    }
    let rows_capacity = capacity.saturating_sub(asked).saturating_sub(usize::from(
        screen.toggle.is_some() && screen.question.is_none(),
    ));
    let start = match app.focused() {
        Some(TargetId::Home(Spot::Entry(key) | Spot::Stop(key))) => screen
            .rows
            .iter()
            .position(|(row, _, _)| *row == key)
            .map_or(0, |at| (at + 1).saturating_sub(rows_capacity)),
        _ => 0,
    };
    for (key, text, has_x) in screen.rows.iter().skip(start) {
        if row_y >= placed.foot {
            break;
        }
        // A readable row ends in a ✕ in its last column: stopping a
        // live session, deleting an exited one. The box fills the
        // width, so there is always a last column.
        if *has_x {
            let body = usize::from(placed.width.saturating_sub(1));
            let wide = super::to_u16(body);
            put(
                buf,
                area,
                placed.x,
                row_y,
                &cut(text, body),
                wide,
                Style::default(),
            );
            let cross = placed.x.saturating_add(wide);
            buf.set_stringn(cross, row_y, "✕", 1, Style::default());
            if wide > 0 {
                targets.push(Target {
                    id: TargetId::Home(Spot::Entry(*key)),
                    rect: Rect::new(placed.x, row_y, wide, 1),
                });
            }
            targets.push(Target {
                id: TargetId::Home(Spot::Stop(*key)),
                rect: Rect::new(cross, row_y, 1, 1),
            });
        } else {
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
        }
        // A drawn row's spinner moves on the tick.
        if let Some(row) = app.row_by_key(*key) {
            app.motion().ask_spin(row);
        }
        row_y = row_y.saturating_add(1);
    }
    // The workspace picker is an overlay centred over home: the title,
    // one row per recent workspace with the selection barred, and what
    // the mouse does (`docs/tui.md`, "Home", "Look", "Overlays"). It
    // keeps the window it had above the box, scrolled around the
    // selection so the selected entry always draws.
    if let Some((items, selected)) = &screen.picker {
        let visible = usize::from(placed.box_top.saturating_sub(area.y)).min(items.len());
        let start = selected
            .saturating_sub(visible.saturating_sub(1))
            .min(items.len().saturating_sub(visible));
        let end = start.saturating_add(visible).min(items.len());
        if let Some(window) = items.get(start..end) {
            draw_picker(window, start, *selected, area, buf, &mut targets);
        }
    }
    // The completion panel above the box, in the shared frame centred
    // across the box's width (`docs/tui.md`, "Look", "Overlays").
    if let Some(panel) = app.completions()
        && matches!(
            panel.rows,
            Rows::Slash(_) | Rows::Files(_) | Rows::Message(_)
        )
    {
        let rect = Rect::new(
            placed.x,
            area.y,
            placed.width,
            placed.box_top.saturating_sub(area.y),
        );
        completions::draw(&panel, rect, placed.box_top, buf, &mut targets);
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
    if app.quit_open() {
        super::quit::draw(app, area, buf, &mut targets);
    }
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

/// Draws the workspace picker over `area`: `window` with the selection
/// barred, in the overlay frame centred across and down home. A long
/// path is cut on the right; rows keep their pick targets.
fn draw_picker(
    window: &[String],
    start: usize,
    selected: usize,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) {
    // The content wraps at the preferred width first: rows are built
    // once at what fits, and the frame shrinks to what they need.
    let room = usize::from(55u16.saturating_sub(4)).min(usize::from(area.width.saturating_sub(4)));
    let text = room.saturating_sub(2);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let body = window
        .iter()
        .enumerate()
        .map(|(n, item)| {
            let at = start.saturating_add(n);
            let focused = at == selected;
            super::overlay::Row {
                spans: vec![
                    Span::styled(
                        if focused { "› " } else { "  " }.to_owned(),
                        if focused { bold } else { dim },
                    ),
                    Span::raw(cut(item, text)),
                ],
                right: Vec::new(),
                targets: vec![(0, u16::MAX, TargetId::Home(Spot::Pick(at)))],
                barred: focused,
            }
        })
        .collect();
    let framed = super::overlay::Overlay {
        title: Some(("Workspaces".to_owned(), None)),
        close: None,
        body,
        footer: Some(super::overlay::legend(&[
            ("↑↓", "move"),
            ("enter", "open"),
            ("esc", "closes"),
        ])),
        prefer: 55,
    };
    super::overlay::draw(buf, area, &framed, super::overlay::Place::Centre, targets);
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

#[cfg(test)]
#[path = "home_view_motion_tests.rs"]
mod motion_tests;
