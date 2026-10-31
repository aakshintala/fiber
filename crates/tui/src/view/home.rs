//! Home's frame: the logo header, project sections, the bottom input box
//! and the foot hint (`docs/tui.md`, "Home").

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::{FOCUS_STYLE, completions, lift_hover, logo};
use crate::app::App;
use crate::app::Surface;
use crate::completion_rows::Rows;
use crate::format::{cut, width, wrap};
use crate::graphics::logo::{COLS, ROWS, Shown};
use crate::home::{HomeScreen, ListLine, Spot};
use crate::mouse::{self, Target, TargetId};
use crate::text::to_u16;
use crate::theme::{Role, style};

pub(super) mod picker;
pub(super) mod question;

/// The home input box's tint.
const BOX_TINT: Style = Style::new().bg(Role::Surface.color());

/// The input box's placeholder, after `› `, while the draft is empty and
/// no `start` went out in this run.
const PLACEHOLDER: &str = "› /? for shortcuts";

/// The draft rows the box shows: at least 2, at most 8.
const MIN_BOX: usize = 2;
const MAX_BOX: usize = 8;

/// Where home's rows draw.
struct Placed {
    /// The box's first column and width.
    x: u16,
    width: u16,
    /// The text's width inside the box: two narrower where a box takes
    /// the sides.
    inner: u16,
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
    /// The foot hint's row.
    foot: u16,
    /// The list's first row and exclusive end.
    list_top: u16,
    list_end: u16,
}

/// Reserves the foot, optional banner and bottom box before the list.
/// A four-row header needs twelve rows plus the wrapped blockers.
fn layout(app: &App, screen: &HomeScreen, area: Rect) -> Placed {
    let width = area.width;
    let inner = crate::surface::inset(width);
    let x = area.x;
    let draft = app.input().rows(inner);
    let height = draft.len().clamp(MIN_BOX, MAX_BOX);
    // The box is its ▄ edge, the draft rows, the chip row and its ▀ edge.
    let boxed = to_u16(height).saturating_add(3);
    // A rejected `start` draws its message above the box, wrapped at its
    // width, staying while the list fills.
    let blockers: Vec<String> = screen
        .blockers
        .iter()
        .flat_map(|line| wrap(line, usize::from(inner)))
        .collect();
    let need = 12u16.saturating_add(to_u16(blockers.len()));
    let four = area.height >= need && area.width >= to_u16(logo::width_cells());
    let logo_rows = if four { 4 } else { 1 };
    let logo = area.y;
    let foot = area.bottom().saturating_sub(1);
    let box_top = foot
        .saturating_sub(u16::from(app.banner().is_some()))
        .saturating_sub(boxed)
        .max(area.y);
    let list_top = area.y.saturating_add(5);
    let list_end = if area.height >= 12 {
        box_top.saturating_sub(to_u16(blockers.len())).max(area.y)
    } else {
        list_top
    };
    let (row, _) = app.input().cursor(inner);
    let top = row.saturating_add(1).saturating_sub(height);
    let shown = draft.into_iter().skip(top).take(height).collect();
    let draft_top = box_top.saturating_add(1);
    let chip = draft_top.saturating_add(to_u16(height));
    Placed {
        x,
        width,
        inner,
        logo,
        logo_rows,
        blockers,
        box_top,
        draft_top,
        shown,
        top,
        chip,
        foot,
        list_top,
        list_end,
    }
}

/// The logo image's 32-by-4 cells, while the image form draws, else
/// none: the event loop positions the image write here (`docs/tui.md`,
/// "The logo").
pub(super) fn logo_rect(app: &App, screen: &HomeScreen, area: Rect) -> Option<Rect> {
    if screen.logo == Shown::Pixels {
        return None;
    }
    let placed = layout(app, screen, area);
    if placed.logo_rows != 4 {
        return None;
    }
    Some(Rect::new(placed.x, placed.logo, COLS, ROWS))
}

/// The input text's first column inside the box.
fn text_x(placed: &Placed) -> u16 {
    placed
        .x
        .saturating_add(placed.width.saturating_sub(placed.inner))
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

/// The rows over which home's notice overlay draws, in `area`.
fn notice_area(placed: &Placed, area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y,
        area.width,
        placed
            .box_top
            .saturating_sub(to_u16(placed.blockers.len()))
            .saturating_sub(area.y),
    )
}

/// The rows of `area` over which home's notice overlay draws.
pub(crate) fn notice_rows_area(app: &App, screen: &HomeScreen, area: Rect) -> Rect {
    notice_area(&layout(app, screen, area), area)
}

/// The session list's window in `area`: the rows' rect, and the
/// highest first row the window shows, the rows past the window.
/// The wheel over the rect scrolls the list.
pub(crate) fn list_window(app: &App, screen: &HomeScreen, area: Rect) -> (Rect, usize) {
    let placed = layout(app, screen, area);
    let tall = placed.list_end.saturating_sub(placed.list_top);
    let max = list_rows(screen).len().saturating_sub(usize::from(tall));
    (
        Rect::new(placed.x, placed.list_top, placed.width, tall),
        max,
    )
}

struct ListRow {
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    spot: Option<Spot>,
    stop: bool,
    plus: Option<(u64, u16)>,
}

fn list_rows(screen: &HomeScreen) -> Vec<ListRow> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut rows = Vec::new();
    for line in &screen.list {
        let mut row = ListRow {
            left: Vec::new(),
            right: Vec::new(),
            spot: None,
            stop: false,
            plus: None,
        };
        match line {
            ListLine::Head {
                start,
                name,
                live,
                spend,
                waiting,
            } => {
                row.left.push(Span::styled(
                    name.clone(),
                    style(Role::Accent).add_modifier(Modifier::BOLD),
                ));
                if let Some(key) = start {
                    row.plus = Some((*key, to_u16(width(name).saturating_add(1))));
                    row.left.push(Span::styled(" +", style(Role::Accent)));
                }
                row.left
                    .push(Span::styled(format!("  {live} live · {spend}"), dim));
                if *waiting != 0 {
                    row.left.push(Span::styled(
                        format!("  {waiting} waiting on you"),
                        style(Role::Attention),
                    ));
                }
            }
            ListLine::Live {
                key,
                number,
                glyph,
                name,
                word,
                tone,
                since,
                stop,
                id,
                turns,
                branch,
                spend,
                context,
                wait,
            } => {
                row.spot = Some(Spot::Entry(*key));
                row.stop = *stop;
                let number = number.map(|n| format!("⌥{n} ")).unwrap_or_default();
                row.left = vec![
                    Span::styled(number, dim),
                    Span::styled(format!("{glyph} "), style(*tone)),
                    Span::raw(name.clone()),
                ];
                row.right = vec![
                    Span::styled(word.to_string(), style(*tone)),
                    Span::styled(format!("  {since}"), dim),
                ];
                rows.push(row);
                let mut segments = vec![id.clone()];
                if let Some(turns) = turns {
                    segments.push(crate::format::count(*turns, "turn", "turns"));
                }
                if let Some(branch) = branch {
                    segments.push(format!("⎇ {branch}"));
                }
                segments.push(spend.clone());
                if let Some(context) = context {
                    segments.push(format!("{context}%"));
                }
                let mut left = vec![Span::styled(format!("    {}", segments.join(" · ")), dim)];
                if let Some(wait) = wait {
                    left.push(Span::styled(format!(" · {wait}"), style(Role::Attention)));
                }
                rows.push(ListRow {
                    left,
                    right: Vec::new(),
                    spot: Some(Spot::Entry(*key)),
                    stop: false,
                    plus: None,
                });
                continue;
            }
            ListLine::ExitedHead { count } => row
                .left
                .push(Span::styled(format!("  exited · {count}"), dim)),
            ListLine::Exited {
                key,
                glyph,
                name,
                id,
                turns,
                spend,
                age,
            } => {
                row.spot = Some(Spot::Entry(*key));
                row.left
                    .push(Span::styled(format!("   {glyph} {name}"), dim));
                let mut segments = vec![id.clone()];
                if let Some(turns) = turns {
                    segments.push(crate::format::count(*turns, "turn", "turns"));
                }
                segments.push(spend.clone());
                if let Some(age) = age {
                    segments.push(age.clone());
                }
                row.right.push(Span::styled(segments.join(" · "), dim));
            }
            ListLine::More { key, count } => {
                row.spot = Some(Spot::More(*key));
                row.left
                    .push(Span::styled(format!("     … {count} more"), dim));
            }
        }
        rows.push(row);
    }
    rows
}

// The `at + height > end` pull keeps an entry fully inside its window:
// with `at >= start` and `at + height == end` the pull recomputes the
// same `start`, so `>=` is equivalent and no test can catch it.
#[cfg_attr(false, mutants::skip)]
fn draw_list(
    app: &App,
    screen: &HomeScreen,
    placed: &Placed,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) {
    let capacity = usize::from(placed.list_end.saturating_sub(placed.list_top));
    let rows = list_rows(screen);
    let max = rows.len().saturating_sub(capacity);
    let focused = app.focused().and_then(|target| {
        let TargetId::Home(spot) = target else {
            return None;
        };
        spot.list_stop()
    });
    // The window starts at the stored offset, clamped to the rows
    // past it. A focused row outside the window pulls into view, and
    // pulls the stored offset with it, so the wheel carries on from
    // the rows shown.
    let mut start = app.home_list_scroll().min(max);
    if let Some(at) = focused.and_then(|spot| rows.iter().position(|r| r.spot == Some(spot))) {
        let height = 1 + usize::from(
            rows.get(at.saturating_add(1))
                .is_some_and(|r| r.spot == focused),
        );
        let end = start.saturating_add(capacity);
        if at < start || at.saturating_add(height) > end {
            start = at
                .saturating_add(height.min(capacity))
                .saturating_sub(capacity);
        }
    }
    app.set_home_list_scroll(start);
    if screen.empty {
        let y = placed
            .list_top
            .saturating_add(to_u16(capacity.saturating_sub(2) / 2));
        for (i, text) in [
            "No sessions yet.",
            "Type below and press Enter to start one.",
        ]
        .into_iter()
        .enumerate()
        {
            let y = y.saturating_add(to_u16(i));
            if y < placed.list_end {
                put(
                    buf,
                    area,
                    area.x
                        .saturating_add(area.width.saturating_sub(to_u16(width(text))) / 2),
                    y,
                    text,
                    area.width,
                    Style::new().add_modifier(Modifier::DIM),
                );
            }
        }
    }
    for (at, row) in rows.iter().skip(start).take(capacity).enumerate() {
        let y = placed.list_top.saturating_add(to_u16(at));
        let body = placed.width.saturating_sub(if row.stop { 2 } else { 0 });
        let right = ratatui::text::Line::from(row.right.clone());
        let right_width = to_u16(right.width()).min(body);
        let right_x = placed.x.saturating_add(body.saturating_sub(right_width));
        let left = ratatui::text::Line::from(row.left.clone());
        buf.set_line(placed.x, y, &left, body.saturating_sub(right_width));
        buf.set_line(right_x, y, &right, right_width);
        // A session row reads as one line: the left spans, then the
        // right spans at their drawn offset. The ✕ is a button and the
        // focus bar a highlight; neither is copyable text.
        {
            let left_shown = body.saturating_sub(right_width);
            let left_text: String = row.left.iter().map(|span| span.content.as_ref()).collect();
            let mut text = cut(&left_text, usize::from(left_shown));
            let right_text: String = row.right.iter().map(|span| span.content.as_ref()).collect();
            if !right_text.is_empty() {
                let gap =
                    usize::from(right_x.saturating_sub(placed.x)).saturating_sub(width(&text));
                text.push_str(&" ".repeat(gap));
                text.push_str(&cut(&right_text, usize::from(right_width)));
            }
            if !text.is_empty() {
                let end = placed.x.saturating_add(to_u16(width(&text)));
                app.note_surface(crate::app::NotedSurface {
                    surface: crate::app::SurfaceId::Home,
                    rows: vec![crate::app::SpanRow {
                        y,
                        x: placed.x,
                        end,
                        text,
                        join: crate::rows::Join::Break,
                    }],
                });
                for rect in app.selection_cells(
                    crate::app::SurfaceId::Home,
                    Rect::new(placed.x, y, placed.width, 1),
                ) {
                    buf.set_style(
                        rect.intersection(area),
                        crate::view::marks::selection_style(),
                    );
                }
            }
        }
        if let Some(spot) = row.spot {
            let entry_width = placed.width.saturating_sub(u16::from(row.stop));
            if entry_width > 1 {
                targets.push(Target {
                    id: TargetId::Home(spot),
                    rect: Rect::new(placed.x, y, entry_width, 1),
                });
            }
            if row.spot == focused {
                buf.set_style(
                    Rect::new(placed.x, y, placed.width, 1),
                    Style::new().bg(Role::Selection.color()),
                );
            }
            if let Spot::Entry(key) = spot {
                if row.stop {
                    let x = area.right().saturating_sub(1);
                    put(buf, area, x, y, "✕", 1, Style::default());
                    targets.push(Target {
                        id: TargetId::Home(Spot::Stop(key)),
                        rect: Rect::new(x, y, 1, 1),
                    });
                }
                if let Some(row) = app.row_by_key(key) {
                    app.motion().ask_spin(row);
                }
            }
        }
        if let Some((key, offset)) = row.plus
            && offset < placed.width
        {
            targets.push(Target {
                id: TargetId::Home(Spot::Start(key)),
                rect: Rect::new(placed.x.saturating_add(offset), y, 1, 1),
            });
        }
    }
}

/// Draws home's `screen` into `area` of `buf`: the one-row logo, the box
/// with the completion panel above it, the session list under the box,
/// and the foot hint on the last row, with the notice rows over the rows
/// above the foot and the overlay over the whole area.
/// Returns the click targets drawn; last, as on the conversation screen,
/// the target under `pointer` gets [`lift_hover`] and the focused target
/// gets [`FOCUS_STYLE`].
pub(super) fn render(
    app: &App,
    screen: &HomeScreen,
    area: Rect,
    buf: &mut Buffer,
    pointer: Option<(u16, u16)>,
) -> (Vec<Target>, Vec<super::super::ext::anim::AnimKey>) {
    if area.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut painted = Vec::new();
    let mut targets = Vec::new();
    let placed = layout(app, screen, area);
    if placed.logo_rows == 4 {
        // The image form keeps the pixel logo's cells: kitty's
        // placeholders move with the text, iTerm2's and Sixel's blank
        // cells sit under the positioned write. The dim version beside
        // the cells never moves (`docs/tui.md`, "The logo").
        match screen.logo {
            Shown::Pixels => logo::draw(buf, placed.x, placed.logo),
            Shown::Kitty => {
                let style = crate::look::placeholder_style(crate::graphics::logo::ID);
                for row in 0..usize::from(ROWS) {
                    for col in 0..usize::from(COLS) {
                        let span = crate::graphics::cell(row, col, style);
                        if let Some(into) = buf.cell_mut((
                            placed.x.saturating_add(to_u16(col)),
                            placed.logo.saturating_add(to_u16(row)),
                        )) {
                            into.set_symbol(span.content.as_ref());
                            into.set_style(span.style);
                        }
                    }
                }
            }
            Shown::Blank => {
                for row in 0..usize::from(ROWS) {
                    for col in 0..usize::from(COLS) {
                        if let Some(into) = buf.cell_mut((
                            placed.x.saturating_add(to_u16(col)),
                            placed.logo.saturating_add(to_u16(row)),
                        )) {
                            into.set_symbol(" ");
                            into.set_style(Style::default());
                        }
                    }
                }
            }
        }
        let x = placed.x.saturating_add(35);
        let dim = Style::new().add_modifier(Modifier::DIM);
        put(
            buf,
            area,
            x,
            placed.logo.saturating_add(1),
            &screen.version,
            placed.width.saturating_sub(35),
            dim,
        );
        let mut spans = Vec::new();
        for (role, text) in &screen.header.counts {
            if !spans.is_empty() {
                spans.push(Span::styled(" · ", dim));
            }
            spans.push(Span::styled(text.clone(), style(*role)));
        }
        if let Some(spend) = &screen.header.spend {
            if !spans.is_empty() {
                spans.push(Span::styled(" · ", dim));
            }
            spans.push(Span::styled(spend.clone(), dim));
        } else {
            spans.push(Span::styled("no live sessions", dim));
        }
        let count_width = placed
            .width
            .saturating_sub(35)
            .min(area.right().saturating_sub(x));
        if count_width != 0 {
            buf.set_line(
                x,
                placed.logo.saturating_add(2),
                &ratatui::text::Line::from(spans),
                count_width,
            );
        }
    } else {
        // The logo is `⌇ fiber <version>`: the ⌇ and the name in the accent
        // colour, the version dim. The logo row always draws: the pad
        // keeps it inside the area.
        let name = format!("{} fiber ", screen.glyph);
        let at = placed.x;
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
    let mut blocker_y = placed.box_top.saturating_sub(to_u16(placed.blockers.len()));
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
    // written text (`docs/tui.md`, "Look", "The input box"). Without
    // colour the box is thin: ┌─┐ and └─┘ on the edge rows and │ on the
    // box's columns, dim, with no tint, and the text sits one column in.
    let slab = ratatui::layout::Rect::new(
        placed.x,
        placed.draft_top,
        placed.width,
        placed
            .chip
            .saturating_sub(placed.draft_top)
            .saturating_add(1),
    );
    // The text's first column: one past the box's left side where the
    // box takes the sides.
    let text_x = text_x(&placed);
    if crate::surface::box_fit(placed.width) {
        crate::surface::draw_box(buf, area, slab, crate::surface::Edges::BOTH);
    } else {
        crate::surface::draw_slab(
            buf,
            slab,
            Role::Surface,
            Some(crate::surface::Stripe {
                colour: Role::Accent,
                right: false,
            }),
            crate::surface::Edges::BOTH,
        );
    }
    for (at, row) in placed.shown.iter().enumerate() {
        let y = placed.draft_top.saturating_add(to_u16(at));
        if screen.placeholder && placed.top == 0 && at == 0 {
            // The placeholder's prompt is `info` like a draft's; the rest
            // stays dim (`docs/tui.md`, "The input box").
            put_prompt(buf, area, text_x, y, PLACEHOLDER, placed.inner, true);
        } else {
            put_prompt(buf, area, text_x, y, row, placed.inner, false);
        }
    }
    // The draft's selection paints over the shown rows with the same
    // tint every surface uses, and the shown text slab notes for the
    // press and drag mapping, as the conversation's input box does
    // (`docs/tui.md`, "Selection and copy").
    {
        let shown = to_u16(placed.shown.len());
        let text_w = placed.inner;
        if shown != 0 {
            app.note_input_area(
                Rect::new(text_x, placed.draft_top, text_w, shown),
                placed.top,
            );
        }
        let full = Rect::new(
            text_x,
            placed.draft_top.saturating_sub(to_u16(placed.top)),
            text_w,
            to_u16(placed.top).saturating_add(shown),
        );
        let slab = Rect::new(text_x, placed.draft_top, text_w, shown);
        for rect in app.selection_cells(crate::app::SurfaceId::Input, full) {
            let clipped = rect.intersection(slab);
            buf.set_style(clipped, crate::view::marks::selection_style());
        }
    }
    // The caret's cell draws the covered character in the slab's colour
    // on the text colour, or a block in the text colour past the row's
    // end, while nothing takes the keyboard (`docs/tui.md`, "The input
    // box"). A focused chip keeps it there: the chip is the input box's
    // keyboard. The terminal's own cursor stays where `cursor` puts it.
    let chip_focused = matches!(app.focused(), Some(TargetId::Home(spot)) if spot.is_chip());
    if app.focused().is_none() || chip_focused {
        let (row, col) = app.input().cursor(placed.inner);
        let y = placed
            .draft_top
            .saturating_add(to_u16(row.saturating_sub(placed.top)));
        let x = text_x.saturating_add(col.min(placed.inner.saturating_sub(1)));
        // The draft sits below the area's top row by construction, so
        // only the bottom needs the guard.
        if y < area.bottom() {
            let covered = row
                .checked_sub(placed.top)
                .and_then(|at| placed.shown.get(at))
                .and_then(|shown| crate::view::cursor::covered_symbol(shown, usize::from(col)));
            let (symbol, style) =
                crate::view::cursor::cursor_cell(covered.as_deref(), Role::Surface, Role::Text);
            buf.set_stringn(x, y, &symbol, crate::text::width(&symbol), style);
        }
    }
    // The chip row: the workspace, the model, the thinking level, and
    // what Enter does. The workspace chip opens the picker.
    let mut chip_x = text_x;
    // A chip past the box's right edge draws nothing and takes no
    // target; a chip cut part-way keeps its visible cells. Without
    // colour the right side takes the box's last column.
    let box_end = text_x
        .saturating_add(placed.inner)
        .min(area.x.saturating_add(area.width));
    for (at, chip) in screen.chips.iter().enumerate() {
        let text = &chip.text;
        if at != 0 {
            put(
                buf,
                area,
                chip_x,
                placed.chip,
                "  ",
                box_end.saturating_sub(chip_x),
                BOX_TINT,
            );
            chip_x = chip_x.saturating_add(2);
        }
        put(
            buf,
            area,
            chip_x,
            placed.chip,
            text,
            box_end.saturating_sub(chip_x),
            if chip.dim {
                BOX_TINT.add_modifier(Modifier::DIM)
            } else {
                BOX_TINT
            },
        );
        let wide = to_u16(width(text));
        if let Some(spot) = &chip.spot {
            let end = chip_x.saturating_add(wide).min(box_end);
            if end > chip_x {
                targets.push(Target {
                    id: TargetId::Home(*spot),
                    rect: Rect::new(chip_x, placed.chip, end - chip_x, 1),
                });
            }
        }
        chip_x = chip_x.saturating_add(wide);
    }
    draw_list(app, screen, &placed, area, buf, &mut targets);
    // The workspace picker is an overlay centred over home: the title,
    // one row per recent workspace with the selection barred, and what
    // the mouse does (`docs/tui.md`, "Home", "Look", "Overlays"). It
    // keeps the window it had above the box, scrolled around the
    // selection so the selected entry always draws. With a typed path
    // the typed row, the completions, the header and the dim recents
    // draw instead.
    if let Some(picker) = &screen.picker {
        let slab = match &picker.typed {
            Some(typed) => picker::draw_typed_picker(
                app,
                typed,
                &picker.recents,
                picker.focus,
                picker.escapes,
                (area, placed.box_top),
                buf,
                &mut targets,
            ),
            None => {
                let items = &picker.recents;
                let selected = picker.focus.unwrap_or(0);
                let visible = usize::from(placed.box_top.saturating_sub(area.y)).min(items.len());
                let start = selected
                    .saturating_sub(visible.saturating_sub(1))
                    .min(items.len().saturating_sub(visible));
                let end = start.saturating_add(visible).min(items.len());
                // Fit the window to the body rows, keeping the selection
                // visible: the chrome is two edges, two pad rows, the title
                // and its gap, and the footer's gap and row.
                let room = usize::from(area.height.saturating_sub(8));
                let len = end.saturating_sub(start);
                let (off, take) = completions::window(selected.saturating_sub(start), len, room);
                let at = start.saturating_add(off);
                if let Some(window) = items.get(at..at.saturating_add(take)) {
                    picker::draw_picker(
                        app,
                        window,
                        at,
                        selected,
                        picker.escapes,
                        area,
                        buf,
                        &mut targets,
                    )
                } else {
                    Rect::default()
                }
            }
        };
        app.frames().note(Surface::WorkspacePicker, slab);
    }
    token_targets(app, area, &placed, &mut targets);
    let mut bottom = placed.foot;
    crate::view::banner::draw(app, area, buf, &mut bottom);
    // The foot hint sits on the last row, centred, cut to the width.
    let foot = &screen.foot;
    if placed.foot >= area.y {
        let at = if foot.width() <= usize::from(area.width) {
            area.x
                .saturating_add(area.width.saturating_sub(to_u16(foot.width())) / 2)
        } else {
            area.x
        };
        buf.set_line(at, placed.foot, foot, area.width);
    }
    let rows = notice_area(&placed, area);
    bottom = rows.bottom();
    let slab = super::notice_rows::overlay(
        app,
        rows,
        buf,
        &mut targets,
        &super::notice_rows::frame_bound(app),
    );
    app.frames().note(Surface::Notice, slab);
    // The rows sit above the banner, unreserved, over the area's bottom
    // rows (`docs/tui.md`, "Notices").
    painted.extend(super::notice_rows::draw(
        app,
        rows,
        buf,
        &mut bottom,
        &mut targets,
        &super::notice_rows::frame_bound(app),
    ));
    // The completion panels draw after the rows under them, so their
    // cells win, and before the quit question, which takes every key.
    // The Ctrl+R panel centres over home's whole area, as the quit
    // question does; the `/` and `@` panels keep the box strip
    // (`docs/tui.md`, "Look", "Overlays").
    if let Some(panel) = app.completions() {
        let rect = if matches!(panel.rows, Rows::Search(_)) {
            area
        } else {
            Rect::new(
                placed.x,
                area.y,
                placed.width,
                placed.box_top.saturating_sub(area.y),
            )
        };
        let search = matches!(panel.rows, Rows::Search(_));
        let slab = completions::draw(&panel, rect, placed.box_top, buf, &mut targets);
        app.frames().note(
            if search {
                Surface::Search
            } else {
                Surface::Completion
            },
            slab,
        );
    }
    if let Some(question) = &screen.question {
        let slab = if screen.question_import {
            question::draw_import_question_noted(app, question, area, buf, &mut targets)
        } else {
            question::draw_delete_question(
                app,
                question,
                screen.question_scroll,
                area,
                buf,
                &mut targets,
            )
        };
        app.frames().note(Surface::DeleteQuestion, slab);
    }
    // The import report draws over home when open, above the offer and
    // delete frames: one overlay with both counts, no notices
    // (`docs/tui.md`, "Overlays"). The slab swallows the clicks landing
    // on it, as the question frames do.
    // The surfaces that draw a frame go up bottom first, so the quit
    // question draws above the report.
    super::draw_framed(app, area, buf, &mut targets, |targets, buf| {
        let before = targets.len();
        let slab = crate::import_report::draw_report(app, area, buf, targets);
        if !slab.is_empty() {
            targets.insert(
                before.min(targets.len()),
                Target {
                    id: TargetId::Home(Spot::Question),
                    rect: slab,
                },
            );
        }
        slab
    });
    if let Some(id) = pointer.and_then(|(col, row)| mouse::hit(&targets, col, row))
        && !matches!(id, TargetId::Home(Spot::Question))
    {
        for target in targets.iter().filter(|target| target.id == id) {
            lift_hover(buf, target.rect);
        }
    }
    if let Some(id) = app.focused()
        && screen.question.is_none()
    {
        // A focused chip draws as a hovered one, never reversed; every
        // other focused target keeps the focus style.
        let chip = matches!(id, TargetId::Home(spot) if spot.is_chip());
        for target in targets.iter().filter(|target| target.id == id) {
            if chip {
                lift_hover(buf, target.rect);
            } else if !matches!(
                id,
                TargetId::Home(Spot::Entry(_) | Spot::Stop(_) | Spot::More(_))
            ) {
                buf.set_style(target.rect, FOCUS_STYLE);
            }
        }
    }
    (targets, painted)
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
        let y = placed.draft_top.saturating_add(to_u16(dy));
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
/// box, `None` while navigating or typing a workspace path, whose row
/// draws its own cursor. A focused chip keeps it there: the chip
/// is the input box's keyboard.
pub(super) fn cursor(app: &App, screen: &HomeScreen, area: Rect) -> Option<Position> {
    if area.is_empty() {
        return None;
    }
    if screen
        .picker
        .as_ref()
        .is_some_and(|picker| picker.typed.is_some())
    {
        return None;
    }
    if let Some(focus) = app.focused()
        && !matches!(focus, TargetId::Home(spot) if spot.is_chip())
    {
        return None;
    }
    let placed = layout(app, screen, area);
    let (row, col) = app.input().cursor(placed.inner);
    let y = placed
        .draft_top
        .saturating_add(to_u16(row.saturating_sub(placed.top)));
    if y >= area.bottom() {
        return None;
    }
    let x = text_x(&placed).saturating_add(col.min(placed.inner.saturating_sub(1)));
    Some(Position::new(x, y))
}

#[cfg(test)]
#[path = "../home_view_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../home_view_motion_tests.rs"]
mod motion_tests;
