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
use crate::home::{HomeScreen, ListLine, Spot, TypedScreen};
use crate::mouse::{self, Target, TargetId};
use crate::text::to_u16;
use crate::theme::{Role, style};

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

/// The delete question's footer: one session, or every session the
/// cascade question names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteFooter {
    /// `Enter delete · Esc keep`.
    Delete,
    /// `Enter delete all · Esc keep`.
    DeleteAll,
}

/// The delete question's head and its footer: the head is the question
/// without its legend and that deleting is permanent, so a session
/// named with the footer words keeps its name whole. The legend is the
/// question's last one; a question without one deletes one session.
fn split_question(question: &str) -> (&str, DeleteFooter) {
    // The question ends in its legend, so stripping the suffix keeps a
    // name holding the legend words whole.
    const ALL: &str = " · Enter delete all · Esc keep";
    const ONE: &str = " · Enter delete · Esc keep";
    const UNDO: &str = " It cannot be undone";
    if let Some(head) = question.strip_suffix(ALL) {
        (
            head.strip_suffix(UNDO).unwrap_or(head),
            DeleteFooter::DeleteAll,
        )
    } else if let Some(head) = question.strip_suffix(ONE) {
        (
            head.strip_suffix(UNDO).unwrap_or(head),
            DeleteFooter::Delete,
        )
    } else {
        (question, DeleteFooter::Delete)
    }
}

/// One delete frame body row: its `spans`, with no right end, no
/// targets and no bar.
fn question_row(spans: Vec<Span<'static>>) -> super::overlay::Row {
    super::overlay::Row {
        spans,
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The delete frame's body rows for `question` in `area`: the head
/// wrapped at the frame's inner width, a blank row, and that deleting
/// is permanent. Drawing and scrolling share them, so the key path
/// clamps to what the view draws.
fn question_rows(question: &str, area: Rect) -> Vec<super::overlay::Row> {
    // The frame wraps at its inner width first, so it shrinks to what
    // the head needs (`overlay::question_inner`).
    let inner = super::overlay::question_inner(area.width);
    let (head, _) = split_question(question);
    let mut rows: Vec<super::overlay::Row> = wrap(head, inner)
        .into_iter()
        .map(|line| question_row(vec![Span::styled(line, Style::default())]))
        .collect();
    if rows.is_empty() {
        rows.push(question_row(Vec::new()));
    }
    rows.push(question_row(Vec::new()));
    rows.push(question_row(vec![Span::styled(
        "It cannot be undone.".to_owned(),
        Style::default(),
    )]));
    rows
}

/// The import offer's frame body rows for `question` in `area`: the
/// head wrapped at the frame's inner width, a blank row, and one row
/// per source with its key. The offer never scrolls, so the rows
/// always fit the frame they draw in.
fn import_rows(question: &str, area: Rect) -> Vec<super::overlay::Row> {
    // The frame wraps at its inner width first, so it shrinks to what
    // the head needs (`overlay::question_inner`).
    let inner = super::overlay::question_inner(area.width);
    let mut rows: Vec<super::overlay::Row> = wrap(question, inner)
        .into_iter()
        .map(|line| question_row(vec![Span::styled(line, Style::default())]))
        .collect();
    if rows.is_empty() {
        rows.push(question_row(Vec::new()));
    }
    rows.push(question_row(Vec::new()));
    for (key, name, dir) in [
        ("1", "Claude Code", "~/.claude"),
        ("2", "Codex", "~/.codex"),
    ] {
        rows.push(question_row(vec![
            Span::styled(format!("{key} "), Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{name} ({dir})")),
        ]));
    }
    rows
}

/// Draws the import offer over `area`: the frame centred with its
/// title, the head and one row per source, and its footer, beside
/// the delete frame's drawing.
/// The import question as drawn, without selection noting: tests draw
/// it standalone, while `render` notes through
/// `draw_import_question_noted`.
#[cfg(test)]
fn draw_import_question(
    question: &str,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
    draw_import_question_inner(None, question, area, buf, targets)
}

fn draw_import_question_noted(
    app: &App,
    question: &str,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
    draw_import_question_inner(Some(app), question, area, buf, targets)
}

fn draw_import_question_inner(
    app: Option<&App>,
    question: &str,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
    let rows = import_rows(question, area);
    let framed = super::overlay::Overlay {
        title: Some(("Import settings".to_owned(), Some(Span::raw("✕")))),
        close: Some(TargetId::Home(Spot::Keep)),
        body: rows,
        footer: Some(super::overlay::legend(&[("Esc", "not now")])),
        prefer: 71,
    };
    let before = targets.len();
    let slab = super::overlay::draw(buf, area, &framed, super::overlay::Place::Centre, targets);
    // The head wraps onto following rows with a space where the wrap
    // dropped one; a source row starts its own line.
    let inner = super::overlay::question_inner(area.width);
    let mut joins: Vec<crate::rows::Join> = crate::format::wrap_joined(question, inner)
        .into_iter()
        .map(|(_, join)| join)
        .collect();
    joins.resize(framed.body.len(), crate::rows::Join::Break);
    if let Some(app) = app {
        super::overlay::note_overlay(
            app,
            buf,
            slab,
            &framed,
            area,
            super::overlay::Place::Centre,
            &joins,
        );
    }
    // One slab-wide target over every frame row, under the frame's
    // own targets: the ✕ keeps its click, and every other frame cell
    // swallows its click instead of reaching the rows under it.
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

/// The delete question's maximum scroll offset in `area`: its body rows
/// past the rows the frame shows them in. The import offer never
/// scrolls.
pub(crate) fn max_question_scroll(_app: &App, screen: &HomeScreen, area: Rect) -> usize {
    let Some(question) = screen.question.as_ref() else {
        return 0;
    };
    if screen.question_import {
        return 0;
    }
    question_rows(question, area)
        .len()
        .saturating_sub(super::overlay::question_capacity(area.height))
}

/// Draws the delete question over `area`: the frame centred with its
/// title, the window of body rows at `scroll`, and its footer, so the
/// title row and the footer stay visible while the body scrolls.
fn draw_delete_question(
    app: &App,
    question: &str,
    scroll: usize,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
    draw_delete_frame(app, question, scroll, area, buf, targets, false)
}

/// Draws the command's delete frame over the attached session: the same
/// title, ✕, footer, text wrapping, scroll clamp and slab target, with
/// the permanence warning pinned as the last drawn row so a long
/// cascade still shows it on its first frame. Home's frame scrolls
/// every row instead, as its clipped test pins.
pub(crate) fn draw_command_delete_question(
    app: &App,
    question: &str,
    scroll: usize,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
    draw_delete_frame(app, question, scroll, area, buf, targets, true)
}

/// The delete frame: the scrolled body window at `scroll`, clamped, its
/// wrap joins, its footer and its slab target. Pinned keeps the
/// permanence warning as the last drawn row, outside the scrolled
/// window; unpinned scrolls every row, as home's frame does.
fn draw_delete_frame(
    app: &App,
    question: &str,
    scroll: usize,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
    pinned: bool,
) -> Rect {
    let mut rows = question_rows(question, area);
    let capacity = super::overlay::question_capacity(area.height);
    let warning = pinned.then(|| rows.pop()).flatten();
    let window = capacity.saturating_sub(usize::from(pinned));
    let max = rows.len().saturating_sub(window);
    // The head's wrap joins read from the same wrap the rows keep.
    let inner = super::overlay::question_inner(area.width);
    let (head, _) = split_question(question);
    let mut head_joins: Vec<crate::rows::Join> = crate::format::wrap_joined(head, inner)
        .into_iter()
        .map(|(_, join)| join)
        .collect();
    head_joins.resize(
        rows.len() + usize::from(warning.is_some()),
        crate::rows::Join::Break,
    );
    let warn_join = if warning.is_some() {
        head_joins.pop()
    } else {
        None
    };
    let start = scroll.min(max);
    let mut body: Vec<super::overlay::Row> = rows.into_iter().skip(start).take(window).collect();
    let mut joins: Vec<crate::rows::Join> =
        head_joins.into_iter().skip(start).take(window).collect();
    if let (Some(warning), Some(join)) = (warning, warn_join) {
        body.push(warning);
        joins.push(join);
    }
    let (_, footer) = split_question(question);
    let legend = match footer {
        DeleteFooter::Delete => super::overlay::legend(&[("Enter", "delete"), ("Esc", "keep")]),
        DeleteFooter::DeleteAll => {
            super::overlay::legend(&[("Enter", "delete all"), ("Esc", "keep")])
        }
    };
    let framed = super::overlay::Overlay {
        title: Some(("Delete session".to_owned(), Some(Span::raw("✕")))),
        close: Some(TargetId::Home(Spot::Keep)),
        body,
        footer: Some(legend),
        prefer: 71,
    };
    let before = targets.len();
    let slab = super::overlay::draw(buf, area, &framed, super::overlay::Place::Centre, targets);
    super::overlay::note_overlay(
        app,
        buf,
        slab,
        &framed,
        area,
        super::overlay::Place::Centre,
        &joins,
    );
    // One slab-wide target over every frame row, under the frame's own
    // targets: the ✕ keeps its click, and every other frame cell
    // swallows its click instead of reaching the rows under it.
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
) -> Vec<Target> {
    if area.is_empty() {
        return Vec::new();
    }
    let mut targets = Vec::new();
    let placed = layout(app, screen, area);
    if placed.logo_rows == 4 {
        logo::draw(buf, placed.x, placed.logo);
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
    // The cursor is a drawn dim `█` at the draft's cursor while nothing
    // takes the keyboard (`docs/tui.md`, "The input box"). A focused
    // chip keeps it there: the chip is the input box's keyboard. The
    // terminal's own cursor stays where `cursor` puts it.
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
            buf.set_stringn(x, y, "█", 1, style(Role::Muted));
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
            Some(typed) => draw_typed_picker(
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
                    draw_picker(
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
    super::notice_rows::draw(
        app,
        rows,
        buf,
        &mut bottom,
        &mut targets,
        &super::notice_rows::frame_bound(app),
    );
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
            draw_import_question_noted(app, question, area, buf, &mut targets)
        } else {
            draw_delete_question(
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
    targets
}

/// One body row of the typed workspace picker, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypedRow {
    /// The typed path's row, with its cursor.
    Typed,
    /// The completion at its index into the draw list.
    Completion(usize),
    /// The dim "no such directory" row, in place of the completions.
    Missing,
    /// The blank row standing the header off the completions.
    Blank,
    /// The dim "── recents ──" header.
    Header,
    /// The recent at its index into the current recents.
    Recent(usize),
}

/// The typed picker's body rows within `room` rows: the typed row
/// always draws, and the focused row draws whenever the room holds
/// more than it. The blank row and the header draw exactly when a
/// recent row draws, and the separators yield first: with a recent
/// focused and no room for them, the recent stands without them.
/// `focus` indexes the completions then the recents, as the screen
/// carries it; `missing` swaps the completions for the one dim row.
fn typed_body(
    completions: usize,
    recents: usize,
    focus: Option<usize>,
    missing: bool,
    room: usize,
) -> Vec<TypedRow> {
    let mut rows = vec![TypedRow::Typed];
    let mut budget = room.saturating_sub(1);
    let focused_recent = match focus {
        Some(at) if at >= completions && recents > 0 => Some(
            at.saturating_sub(completions)
                .min(recents.saturating_sub(1)),
        ),
        Some(_) | None => None,
    };
    if let Some(recent) = focused_recent {
        if budget >= 3 {
            let (off, take) = completions::window(recent, recents, budget.saturating_sub(2));
            let shown = budget.saturating_sub(2).saturating_sub(take);
            // The nearest completions fill what the recents leave, above
            // the header; the missing row takes their place.
            if missing && shown > 0 {
                rows.push(TypedRow::Missing);
            } else if shown > 0 {
                let (coff, ctake) =
                    completions::window(completions.saturating_sub(1), completions, shown);
                rows.extend((coff..coff.saturating_add(ctake)).map(TypedRow::Completion));
            }
            rows.push(TypedRow::Blank);
            rows.push(TypedRow::Header);
            rows.extend((off..off.saturating_add(take)).map(TypedRow::Recent));
        } else if budget == 2 {
            // One context row beside the focused recent: the next
            // recent, else the nearest completion, else the missing row.
            if recent.saturating_add(1) < recents {
                rows.push(TypedRow::Recent(recent));
                rows.push(TypedRow::Recent(recent.saturating_add(1)));
            } else if missing {
                rows.push(TypedRow::Missing);
                rows.push(TypedRow::Recent(recent));
            } else if completions > 0 {
                rows.push(TypedRow::Completion(completions.saturating_sub(1)));
                rows.push(TypedRow::Recent(recent));
            } else {
                rows.push(TypedRow::Recent(recent));
            }
        } else if budget >= 1 {
            rows.push(TypedRow::Recent(recent));
        }
        return rows;
    }
    let anchor = match focus {
        Some(at) if at < completions => at,
        Some(_) | None => 0,
    };
    if missing {
        if budget >= 1 {
            rows.push(TypedRow::Missing);
            budget = budget.saturating_sub(1);
        }
    } else if budget > 0 {
        let (off, take) = completions::window(anchor, completions, budget);
        rows.extend((off..off.saturating_add(take)).map(TypedRow::Completion));
        budget = budget.saturating_sub(take);
    }
    if recents > 0 && budget >= 3 {
        let (off, take) = completions::window(0, recents, budget.saturating_sub(2));
        rows.push(TypedRow::Blank);
        rows.push(TypedRow::Header);
        rows.extend((off..off.saturating_add(take)).map(TypedRow::Recent));
    }
    rows
}

/// The workspace picker's footer: what the mouse does.
fn picker_footer(escapes: bool) -> super::overlay::Row {
    let mut legend = vec![("↑↓", "move"), ("Enter", "open")];
    if escapes {
        legend.push(("Esc", "closes"));
    }
    super::overlay::legend(&legend)
}

/// Draws the typed workspace picker over `area`: the typed row with
/// its cursor, the completions with the focused one on the bar, the
/// blank row, the dim header and the dim recents, with a focused
/// recent barred. Only the recent rows take clicks, by their index
/// into the current recents. It centres over `area` while that keeps
/// it clear of the input box's top row `box_top`; otherwise, when it
/// fits above the box, its bottom edge sits on the row above the box.
/// A screen too short for that keeps it centred, over the box.
#[allow(
    clippy::too_many_arguments,
    reason = "a picker draw needs its window, place and targets"
)]
fn draw_typed_picker(
    app: &App,
    typed: &TypedScreen,
    recents: &[String],
    focus: Option<usize>,
    escapes: bool,
    (area, box_top): (Rect, u16),
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
    // The content wraps at the preferred width first: rows are built
    // once at what fits, and the frame shrinks to what they need.
    let room = usize::from(55u16.saturating_sub(4)).min(usize::from(area.width.saturating_sub(4)));
    // The typed row holds `› `, the text and the drawn cursor.
    let text = room.saturating_sub(3);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let info = Style::new().fg(Role::Info.color());
    // The chrome is two edges, two pad rows, the title and its gap, and
    // the footer's gap and row.
    const CHROME: u16 = 8;
    let body_rows = typed_body(
        typed.completions.len(),
        recents.len(),
        focus,
        typed.missing,
        usize::from(area.height.saturating_sub(CHROME)),
    );
    let tall = to_u16(body_rows.len()).saturating_add(CHROME);
    let centred_bottom = area
        .y
        .saturating_add(area.height.saturating_sub(tall) / 2)
        .saturating_add(tall);
    // Above the box: the centred place when it is clear of the box,
    // else the bottom edge on the row above it.
    let place = if tall <= box_top.saturating_sub(area.y) {
        super::overlay::Place::Across {
            bottom: centred_bottom.min(box_top),
        }
    } else {
        super::overlay::Place::Centre
    };
    let overall = |row: &TypedRow| match row {
        TypedRow::Completion(at) => Some(*at),
        TypedRow::Recent(at) => Some(typed.completions.len().saturating_add(*at)),
        TypedRow::Typed | TypedRow::Missing | TypedRow::Blank | TypedRow::Header => None,
    };
    let mut body = Vec::with_capacity(body_rows.len());
    for row in &body_rows {
        let focused = overall(row).is_some_and(|at| Some(at) == focus);
        match row {
            TypedRow::Typed => body.push(super::overlay::Row {
                spans: vec![
                    Span::styled("› ".to_owned(), info),
                    Span::raw(cut(&typed.text, text)),
                    Span::styled("█".to_owned(), dim),
                ],
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            }),
            TypedRow::Completion(at) => {
                let Some(name) = typed.completions.get(*at) else {
                    continue;
                };
                body.push(super::overlay::Row {
                    spans: vec![
                        Span::styled(
                            if focused { "› " } else { "  " }.to_owned(),
                            if focused { bold } else { dim },
                        ),
                        Span::styled(cut(name, text), if focused { bold } else { Style::new() }),
                    ],
                    right: Vec::new(),
                    targets: Vec::new(),
                    barred: focused,
                });
            }
            TypedRow::Missing => body.push(super::overlay::Row {
                spans: vec![
                    Span::styled("  ".to_owned(), dim),
                    Span::styled("no such directory".to_owned(), dim),
                ],
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            }),
            TypedRow::Blank => body.push(super::overlay::Row {
                spans: Vec::new(),
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            }),
            TypedRow::Header => body.push(super::overlay::Row {
                spans: vec![Span::styled("── recents ──".to_owned(), dim)],
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            }),
            TypedRow::Recent(at) => {
                let Some(name) = recents.get(*at) else {
                    continue;
                };
                body.push(super::overlay::Row {
                    spans: vec![
                        Span::styled(
                            if focused { "› " } else { "  " }.to_owned(),
                            if focused { bold } else { dim },
                        ),
                        Span::styled(cut(name, text), if focused { bold } else { dim }),
                    ],
                    right: Vec::new(),
                    targets: vec![(0, u16::MAX, TargetId::Home(Spot::Pick(*at)))],
                    barred: focused,
                });
            }
        }
    }
    let framed = super::overlay::Overlay {
        title: Some(("Workspaces".to_owned(), None)),
        close: None,
        body,
        footer: Some(picker_footer(escapes)),
        prefer: 55,
    };
    let slab = super::overlay::draw(buf, area, &framed, place, targets);
    // Picker rows are cut to one visual row each; each starts its own
    // line.
    let joins = vec![crate::rows::Join::Break; framed.body.len()];
    super::overlay::note_overlay(app, buf, slab, &framed, area, place, &joins);
    slab
}

/// Draws the workspace picker over `area`: `window` with the selection
/// barred, in the overlay frame centred across and down home. A long
/// path is cut on the right; rows keep their pick targets.
#[allow(
    clippy::too_many_arguments,
    reason = "a picker draw needs its window, place and targets"
)]
fn draw_picker(
    app: &App,
    window: &[String],
    start: usize,
    selected: usize,
    escapes: bool,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
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
        footer: Some(picker_footer(escapes)),
        prefer: 55,
    };
    let slab = super::overlay::draw(buf, area, &framed, super::overlay::Place::Centre, targets);
    // Picker rows are cut to one visual row each; each starts its own
    // line.
    let joins = vec![crate::rows::Join::Break; framed.body.len()];
    super::overlay::note_overlay(
        app,
        buf,
        slab,
        &framed,
        area,
        super::overlay::Place::Centre,
        &joins,
    );
    slab
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
