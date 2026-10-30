//! Home's workspace picker drawing: the typed picker's body rows,
//! the picker's footer, and the typed and untyped picker draws. The
//! draws read only picker state; the window and placement math stays
//! in the parent, which passes the area and the picker's read-only
//! state in.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::format::cut;
use crate::home::{Spot, TypedScreen};
use crate::mouse::{Target, TargetId};
use crate::text::to_u16;
use crate::theme::Role;
use crate::view::{completions, overlay};

/// One body row of the typed workspace picker, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TypedRow {
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
pub(super) fn typed_body(
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
fn picker_footer(escapes: bool) -> overlay::Row {
    let mut legend = vec![("↑↓", "move"), ("Enter", "open")];
    if escapes {
        legend.push(("Esc", "closes"));
    }
    overlay::legend(&legend)
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
#[allow(
    clippy::indexing_slicing,
    reason = "typed_body builds both indices from the same collections, so both are in range"
)]
pub(super) fn draw_typed_picker(
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
        overlay::Place::Across {
            bottom: centred_bottom.min(box_top),
        }
    } else {
        overlay::Place::Centre
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
            TypedRow::Typed => body.push(overlay::Row {
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
                let name = &typed.completions[*at];
                body.push(overlay::Row {
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
            TypedRow::Missing => body.push(overlay::Row {
                spans: vec![
                    Span::styled("  ".to_owned(), dim),
                    Span::styled("no such directory".to_owned(), dim),
                ],
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            }),
            TypedRow::Blank => body.push(overlay::Row {
                spans: Vec::new(),
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            }),
            TypedRow::Header => body.push(overlay::Row {
                spans: vec![Span::styled("── recents ──".to_owned(), dim)],
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            }),
            TypedRow::Recent(at) => {
                let name = &recents[*at];
                body.push(overlay::Row {
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
    let framed = overlay::Overlay {
        title: Some(("Workspaces".to_owned(), None)),
        close: None,
        body,
        footer: Some(picker_footer(escapes)),
        prefer: 55,
    };
    let slab = overlay::draw(buf, area, &framed, place, targets);
    // Picker rows are cut to one visual row each; each starts its own
    // line.
    let joins = vec![crate::rows::Join::Break; framed.body.len()];
    overlay::note_overlay(app, buf, slab, &framed, area, place, &joins);
    slab
}

/// Draws the workspace picker over `area`: `window` with the selection
/// barred, in the overlay frame centred across and down home. A long
/// path is cut on the right; rows keep their pick targets.
#[allow(
    clippy::too_many_arguments,
    reason = "a picker draw needs its window, place and targets"
)]
pub(super) fn draw_picker(
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
            overlay::Row {
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
    let framed = overlay::Overlay {
        title: Some(("Workspaces".to_owned(), None)),
        close: None,
        body,
        footer: Some(picker_footer(escapes)),
        prefer: 55,
    };
    let slab = overlay::draw(buf, area, &framed, overlay::Place::Centre, targets);
    // Picker rows are cut to one visual row each; each starts its own
    // line.
    let joins = vec![crate::rows::Join::Break; framed.body.len()];
    overlay::note_overlay(
        app,
        buf,
        slab,
        &framed,
        area,
        overlay::Place::Centre,
        &joins,
    );
    slab
}
