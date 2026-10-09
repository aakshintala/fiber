//! Drawing the session rail: its cards grouped under project headers,
//! each card's four rows, drawn into the rail rect (`docs/tui.md`, "The
//! rail", "State glyphs"). The rect's last column is the draggable edge
//! the chrome draws; a card spans the columns before it, its stripe in
//! the first and its text from the third, with one margin column each
//! side.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::app::rail::Spot;
use crate::format;
use crate::home::{Left, Row, State, glyph, title};
use crate::markdown::{Role, style};
use crate::mouse::{Target, TargetId};

/// The rail's width from which a card draws its context bar.
const BAR_FROM: u16 = 30;

/// A card's rows: its two edges and four text rows.
pub(crate) const CARD_ROWS: usize = 6;

/// A project's cards under its header.
pub(crate) struct Group<'a> {
    /// The project's name: its first card's workspace's last segment.
    pub(crate) name: String,
    /// The summed spend of its live cards, in US dollars.
    pub(crate) spend: f64,
    /// Its cards, in feed order.
    pub(crate) cards: Vec<&'a Row>,
}

/// The cards grouped by project: the launch project's group first, then
/// each other project in the order its first card appears.
pub(crate) fn groups<'a>(cards: &[&'a Row], launch_project: &str) -> Vec<Group<'a>> {
    let mut projects: Vec<&str> = Vec::new();
    for card in cards {
        if !projects.contains(&card.project.as_str()) {
            projects.push(card.project.as_str());
        }
    }
    if let Some(at) = projects
        .iter()
        .position(|project| *project == launch_project)
    {
        projects.remove(at);
        projects.insert(0, launch_project);
    }
    projects
        .into_iter()
        .map(|project| {
            let cards: Vec<&Row> = cards
                .iter()
                .copied()
                .filter(|card| card.project == project)
                .collect();
            let name = cards
                .first()
                .and_then(|card| card.workspace.split('/').rfind(|part| !part.is_empty()))
                .unwrap_or_default()
                .to_owned();
            // A crashed card is not live, so its spend is left out.
            let spend = cards
                .iter()
                .filter(|card| card.left.is_none())
                .fold(0.0, |sum, card| sum + card.spend);
            Group { name, spend, cards }
        })
        .collect()
}

/// How long since `since_ms`, at `now_ms`, in one unit: seconds under a
/// minute, minutes under an hour, hours under a day, else days. A
/// `since` after now reads `0s`.
pub(crate) fn elapsed(since_ms: u64, now_ms: u64) -> String {
    let seconds = now_ms.saturating_sub(since_ms) / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// A card's state word (`docs/tui.md`, "State glyphs"): a crashed card
/// is CRASHED whatever its last state.
pub(crate) fn word(row: &Row) -> &'static str {
    match (&row.left, &row.state) {
        (Some(Left::Crashed), _) => "CRASHED",
        (_, State::Working | State::Jobs) => "WORKING",
        (_, State::Retrying) => "RETRYING",
        (_, State::Waiting) => "NEEDS INPUT",
        (_, State::Idle) => "READY",
        (_, State::Unreadable) => "CANNOT ATTACH",
    }
}

/// A card's state colour (`docs/tui.md`, "State glyphs"): a crashed card
/// is the error colour whatever its last state.
pub(crate) fn tone(row: &Row) -> Role {
    match (&row.left, &row.state) {
        (Some(Left::Crashed), _) => Role::Error,
        (_, State::Working | State::Jobs) => Role::Accent,
        (_, State::Retrying) => Role::Warning,
        (_, State::Waiting) => Role::Attention,
        (_, State::Idle | State::Unreadable) => Role::Muted,
    }
}

/// One drawn row of the rail: its line from the card's first column, its
/// background, the targets starting on it, and whether it starts a card.
pub(crate) struct RailRow {
    /// The row's spans from the card's first column.
    pub(crate) line: Line<'static>,
    /// The card's tint behind a text row; none for a header or an edge.
    pub(crate) tint: Option<Role>,
    /// The targets whose top row this is: column offset from the card's
    /// first column, width, height in rows, and what a click does.
    pub(crate) spots: Vec<(u16, u16, u16, Spot)>,
    /// The key of the card this row starts: the wheel's stops.
    pub(crate) start: Option<u64>,
}

/// Every group's rows, top to bottom; `width` is the rail rect's.
pub(crate) fn rows(app: &App, width: u16) -> Vec<RailRow> {
    let Some((cards, launch_project)) = app.rail_cards() else {
        return Vec::new();
    };
    let card_w = width.saturating_sub(1);
    let text = usize::from(card_w).saturating_sub(3);
    let mut out = Vec::new();
    // The text's last column: a header's "+" and a crashed card's ✕.
    let last = u16::try_from(text.saturating_add(1)).unwrap_or(u16::MAX);
    for group in groups(&cards, launch_project) {
        let mut header = vec![Span::raw("  ")];
        header.extend(sides(
            vec![Span::raw(group.name.clone())],
            vec![Span::raw(format!("{} +", format::money(group.spend)))],
            text,
        ));
        let spots = group
            .cards
            .first()
            .map(|card| (last, 1, 1, Spot::Start(card.key)))
            .into_iter()
            .collect();
        out.push(RailRow {
            line: Line::from(header),
            tint: None,
            spots,
            start: None,
        });
        for row in &group.cards {
            card(app, row, width, &mut out);
        }
    }
    out
}

/// A card's six rows: its top edge, its four text rows on its tint with
/// the stripe in its state's colour, and its bottom edge.
fn card(app: &App, row: &Row, width: u16, out: &mut Vec<RailRow>) {
    let card_w = usize::from(width.saturating_sub(1));
    let text = card_w.saturating_sub(3);
    let tint = if app.session() == Some(&row.id) {
        Role::SurfaceRaised
    } else {
        Role::Surface
    };
    // debt: a waiting card holds still; its 10-second pulse comes with the working line's tick; upgrade trigger: #686 lands.
    let stripe = crate::surface::stripe_cell(tone(row), tint, false);
    let texts = [
        first_row(app, row, text),
        vec![Span::raw(format::cut(&title(row), text))],
        third_row(row, text),
        usage(row, text, width >= BAR_FROM),
    ];
    out.push(RailRow {
        line: crate::surface::edge_row(card_w, tint, true),
        tint: None,
        spots: Vec::new(),
        start: Some(row.key),
    });
    let mut spots = Vec::new();
    // The ✕ target follows the card's, so a click on it dismisses.
    if row.left == Some(Left::Crashed) {
        let last = u16::try_from(text.saturating_add(1)).unwrap_or(u16::MAX);
        spots.push((last, 1, 1, Spot::Dismiss(row.key)));
    }
    for spans in texts {
        let mut line = vec![stripe.clone(), Span::raw(" ")];
        line.extend(spans);
        out.push(RailRow {
            line: Line::from(line),
            tint: Some(tint),
            spots: std::mem::take(&mut spots),
            start: None,
        });
    }
    out.push(RailRow {
        line: crate::surface::edge_row(card_w, tint, false),
        tint: None,
        spots: Vec::new(),
        start: None,
    });
}

/// A card's first row: its number dim, the glyph and word in its state's
/// colour, and at the right how long it has been in that state, dim; a
/// crashed card's ✕ takes the time's place, and an unreadable row has no
/// time.
fn first_row(app: &App, row: &Row, text: usize) -> Vec<Span<'static>> {
    let number = app
        .rail_state()
        .number(row.key)
        .map(|number| number.to_string())
        .unwrap_or_default();
    let right = if row.left == Some(Left::Crashed) {
        "✕".to_owned()
    } else {
        row.status
            .as_ref()
            .map(|status| elapsed(status.since, app.rail_state().wall()))
            .unwrap_or_default()
    };
    sides(
        vec![
            Span::styled(number, style(Role::Muted)),
            Span::styled(format!(" {} {}", glyph(row), word(row)), style(tone(row))),
        ],
        vec![Span::styled(right, style(Role::Muted))],
        text,
    )
}

/// A card's third row: what it waits on, in the attention colour, while
/// it waits; otherwise its git branch, `detached` without one, and
/// nothing outside git.
fn third_row(row: &Row, text: usize) -> Vec<Span<'static>> {
    if let Some(waiting) = &row.waiting {
        return vec![Span::styled(
            format::cut(waiting, text),
            style(Role::Attention),
        )];
    }
    let branch = row
        .status
        .as_ref()
        .and_then(|status| status.git.as_ref())
        .map(|git| git.branch.as_deref().unwrap_or("detached"))
        .unwrap_or_default();
    vec![Span::raw(format::cut(branch, text))]
}

/// A card's fourth row: its spend, and with a context window its
/// percentage at the right, with the bar between them when `bar` and
/// there is room. The bar's filled cells turn the warning colour from 60%
/// and the error colour from 85%.
fn usage(row: &Row, text: usize, bar: bool) -> Vec<Span<'static>> {
    let spend = format::money(row.spend);
    let context = row
        .status
        .as_ref()
        .and_then(|status| status.context.as_ref())
        .filter(|context| context.window > 0);
    let Some(context) = context else {
        return vec![Span::raw(format::cut(&spend, text))];
    };
    let pct = context.tokens.saturating_mul(100) / context.window;
    let shown = format!("{pct}%");
    let cells = text.saturating_sub(format::width(&spend) + format::width(&shown) + 2);
    if !bar || cells == 0 {
        return sides(vec![Span::raw(spend)], vec![Span::raw(shown)], text);
    }
    let wide = u64::try_from(cells).unwrap_or(u64::MAX);
    let filled = context
        .tokens
        .saturating_mul(wide)
        .saturating_div(context.window)
        .min(wide);
    let filled = usize::try_from(filled).unwrap_or(cells);
    let fill = if pct >= 85 {
        Role::Error
    } else if pct >= 60 {
        Role::Warning
    } else {
        Role::Accent
    };
    vec![
        Span::raw(format!("{spend} ")),
        Span::styled("▆".repeat(filled), style(fill)),
        Span::styled("░".repeat(cells.saturating_sub(filled)), style(Role::Muted)),
        Span::raw(format!(" {shown}")),
    ]
}

/// `left` and `right` on one row of `text` columns: `right` whole at the
/// right, `left` cut to the columns left less one.
fn sides(left: Vec<Span<'static>>, right: Vec<Span<'static>>, text: usize) -> Vec<Span<'static>> {
    let right_w: usize = right.iter().map(Span::width).sum();
    let mut out = cut_spans(left, text.saturating_sub(right_w + 1));
    let left_w: usize = out.iter().map(Span::width).sum();
    out.push(Span::raw(" ".repeat(text.saturating_sub(left_w + right_w))));
    out.extend(right);
    out
}

/// `spans` cut to at most `max` columns, each keeping its style.
fn cut_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let mut room = max;
    spans
        .into_iter()
        .map(|span| {
            let cut = format::cut(&span.content, room);
            room = room.saturating_sub(format::width(&cut));
            Span::styled(cut, span.style)
        })
        .collect()
}

/// Draws the rail's rows into `area` past its scroll, each row's tint
/// across the card's columns, with each target cut at the rail's foot.
/// A card's target is its text rows met with the drawn rows, so a card
/// cut at the top keeps a target over the rows left. Styles set the
/// foreground only, so the tint stays. With `pointer` on a card, the
/// rail's last row shows its full name, workspace and model, dim.
pub(crate) fn draw(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    pointer: Option<(u16, u16)>,
    targets: &mut Vec<Target>,
) {
    let card_w = area.width.saturating_sub(1);
    let rows = rows(app, area.width);
    // A screen that grew never shows a gap: a scroll past the end clamps
    // when drawn.
    let height = usize::from(area.height);
    let skip = app
        .rail_state()
        .scroll()
        .min(rows.len().saturating_sub(height));
    let end = skip.saturating_add(height);
    // Each card's text rows met with the drawn rows: the top row left
    // and the rows left, by card key.
    let mut visible: Vec<(usize, u16, u16, u64)> = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        let Some(key) = row.start else {
            continue;
        };
        let first = at.saturating_add(1);
        let past = at.saturating_add(CARD_ROWS).saturating_sub(1);
        let top = first.max(skip);
        let bottom = past.min(end);
        if top < bottom {
            let y = area
                .y
                .saturating_add(u16::try_from(top.saturating_sub(skip)).unwrap_or(u16::MAX));
            let high = u16::try_from(bottom.saturating_sub(top)).unwrap_or(u16::MAX);
            visible.push((top, y, high, key));
        }
    }
    let mut hovered = None;
    for ((at, row), y) in rows
        .iter()
        .enumerate()
        .skip(skip)
        .zip(area.y..area.bottom())
    {
        if let Some(tint) = row.tint {
            buf.set_style(
                Rect::new(area.x, y, card_w, 1),
                Style::new().bg(tint.color()),
            );
        }
        buf.set_line(area.x, y, &row.line, card_w);
        if let Some((_, card_y, card_h, key)) =
            visible.iter().find(|(top, _, _, _)| *top == at).copied()
        {
            debug_assert_eq!(card_y, y);
            let rect = Rect::new(area.x, card_y, card_w, card_h);
            targets.push(Target {
                id: TargetId::Rail(Spot::Card(key)),
                rect,
            });
            if pointer.is_some_and(|(x, y)| rect.contains((x, y).into())) {
                hovered = Some(key);
            }
        }
        for (col, wide, high, spot) in &row.spots {
            let rect = Rect::new(
                area.x.saturating_add(*col),
                y,
                *wide,
                (*high).min(area.bottom().saturating_sub(y)),
            );
            targets.push(Target {
                id: TargetId::Rail(*spot),
                rect,
            });
        }
    }
    if let Some(row) = hovered.and_then(|key| {
        app.rail_cards()
            .and_then(|(cards, _)| cards.into_iter().find(|card| card.key == key))
    }) {
        foot(row, area, buf);
    }
}

/// The hover line on the rail's last row: the card's full name, its
/// workspace and its model when its status names one, dim, over
/// whatever is there.
fn foot(row: &Row, area: Rect, buf: &mut Buffer) {
    let text = usize::from(area.width.saturating_sub(1)).saturating_sub(3);
    let mut parts = vec![title(row), row.workspace.clone()];
    parts.extend(row.status.as_ref().map(|status| status.model.clone()));
    let line = format::cut(&parts.join(" · "), text);
    let pad = " ".repeat(text.saturating_sub(format::width(&line)));
    buf.set_stringn(
        area.x.saturating_add(2),
        area.bottom().saturating_sub(1),
        format!("{line}{pad}"),
        text,
        style(Role::Muted),
    );
}

#[cfg(test)]
#[path = "rail_tests.rs"]
mod tests;
