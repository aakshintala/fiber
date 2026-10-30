//! Home's question drawing: the delete frame (scrolling, with a pinned
//! command variant), the import offer, their row builders, and the
//! scroll clamp. Drawing and scrolling share the rows, so the key path
//! clamps to what the view draws.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::format::{wrap, wrap_joined};
use crate::home::{HomeScreen, Spot};
use crate::mouse::{Target, TargetId};
use crate::rows::Join;
use crate::view::overlay::{self, Place};

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
fn question_row(spans: Vec<Span<'static>>) -> overlay::Row {
    overlay::Row {
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
fn question_rows(question: &str, area: Rect) -> Vec<overlay::Row> {
    // The frame wraps at its inner width first, so it shrinks to what
    // the head needs (`overlay::question_inner`).
    let inner = overlay::question_inner(area.width);
    let (head, _) = split_question(question);
    let mut rows: Vec<overlay::Row> = wrap(head, inner)
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
pub(super) fn import_rows(question: &str, area: Rect) -> Vec<overlay::Row> {
    // The frame wraps at its inner width first, so it shrinks to what
    // the head needs (`overlay::question_inner`).
    let inner = overlay::question_inner(area.width);
    let mut rows: Vec<overlay::Row> = wrap(question, inner)
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
pub(super) fn draw_import_question(
    question: &str,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
    draw_import_question_inner(None, question, area, buf, targets)
}

pub(super) fn draw_import_question_noted(
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
    let framed = overlay::Overlay {
        title: Some(("Import settings".to_owned(), Some(Span::raw("✕")))),
        close: Some(TargetId::Home(Spot::Keep)),
        body: rows,
        footer: Some(overlay::legend(&[("Esc", "not now")])),
        prefer: 71,
    };
    let before = targets.len();
    let slab = overlay::draw(buf, area, &framed, Place::Centre, targets);
    // The head wraps onto following rows with a space where the wrap
    // dropped one; a source row starts its own line.
    let inner = overlay::question_inner(area.width);
    let mut joins: Vec<Join> = wrap_joined(question, inner)
        .into_iter()
        .map(|(_, join)| join)
        .collect();
    joins.resize(framed.body.len(), Join::Break);
    if let Some(app) = app {
        overlay::note_overlay(app, buf, slab, &framed, area, Place::Centre, &joins);
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
        .saturating_sub(overlay::question_capacity(area.height))
}

/// Draws the delete question over `area`: the frame centred with its
/// title, the window of body rows at `scroll`, and its footer, so the
/// title row and the footer stay visible while the body scrolls.
pub(super) fn draw_delete_question(
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
    let capacity = overlay::question_capacity(area.height);
    let warning = pinned.then(|| rows.pop()).flatten();
    let window = capacity.saturating_sub(usize::from(pinned));
    let max = rows.len().saturating_sub(window);
    // The head's wrap joins read from the same wrap the rows keep.
    let inner = overlay::question_inner(area.width);
    let (head, _) = split_question(question);
    let mut head_joins: Vec<Join> = wrap_joined(head, inner)
        .into_iter()
        .map(|(_, join)| join)
        .collect();
    head_joins.resize(rows.len() + usize::from(warning.is_some()), Join::Break);
    let warn_join = if warning.is_some() {
        head_joins.pop()
    } else {
        None
    };
    let start = scroll.min(max);
    let mut body: Vec<overlay::Row> = rows.into_iter().skip(start).take(window).collect();
    let mut joins: Vec<Join> = head_joins.into_iter().skip(start).take(window).collect();
    if let (Some(warning), Some(join)) = (warning, warn_join) {
        body.push(warning);
        joins.push(join);
    }
    let (_, footer) = split_question(question);
    let legend = match footer {
        DeleteFooter::Delete => overlay::legend(&[("Enter", "delete"), ("Esc", "keep")]),
        DeleteFooter::DeleteAll => overlay::legend(&[("Enter", "delete all"), ("Esc", "keep")]),
    };
    let framed = overlay::Overlay {
        title: Some(("Delete session".to_owned(), Some(Span::raw("✕")))),
        close: Some(TargetId::Home(Spot::Keep)),
        body,
        footer: Some(legend),
        prefer: 71,
    };
    let before = targets.len();
    let slab = overlay::draw(buf, area, &framed, Place::Centre, targets);
    overlay::note_overlay(app, buf, slab, &framed, area, Place::Centre, &joins);
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
