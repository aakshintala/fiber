//! The model picker in the overlay frame under its view's top row
//! (`docs/tui.md`, "Swapped views", "Look", "Overlays"). The picker's top
//! row stays the view's header; the filter, the buttons and the sections
//! draw in the shared frame, centred under it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::{
    overlay::{self, Place, Row},
    to_u16,
};
use crate::format::width as cells;
use crate::model_picker::{Fresh, ModelRow, PickerView};
use crate::mouse::{Target, TargetId};
use crate::swapped::Spot;
use crate::theme::Role;

/// The picker wraps at this content width before shrinking to what the
/// wrapped rows need (`docs/tui.md`, "Look", "Overlays").
const PREFER: u16 = 96;
/// The overlay chrome around the picker body: two edges, two pad rows,
/// and the footer's gap and row. The picker draws no title: the view's
/// top row holds it.
const CHROME: u16 = 6;

/// One dim body row with no bar and no targets.
fn dim_row(text: String) -> Row {
    Row {
        spans: vec![Span::styled(text, Style::new().add_modifier(Modifier::DIM))],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The filter row: "Type to search" dim until something is typed, then
/// "›" dim and the query bold, with a dim block cursor either way.
fn filter_row(view: &PickerView) -> Row {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut spans = if view.filter.is_empty() {
        vec![Span::styled("Type to search ".to_owned(), dim)]
    } else {
        vec![
            Span::styled("› ".to_owned(), dim),
            Span::styled(
                view.filter.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
        ]
    };
    spans.push(Span::styled("█".to_owned(), dim));
    Row {
        spans,
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The buttons row: the count dim, the scope toggle bold, and "⟳ refresh
/// all" dim at the right end. The refresh and toggle cells take clicks
/// as their keys do; the count takes none.
fn buttons_row(view: &PickerView, refresh_at: Option<(u16, u16)>) -> Row {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut spans = vec![Span::styled(view.count.clone(), dim)];
    let mut targets = Vec::new();
    let mut x = to_u16(cells(&view.count));
    if let Some(toggle) = view.toggle.as_deref() {
        spans.push(Span::raw(" ".to_owned()));
        x = x.saturating_add(1);
        let wide = to_u16(cells(toggle));
        spans.push(Span::styled(
            toggle.to_owned(),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        targets.push((
            x,
            x.saturating_add(wide),
            TargetId::View(Spot::Cell(view.buttons_at, 1)),
        ));
    }
    let mut row = Row {
        spans,
        right: vec![Span::styled("⟳ refresh all".to_owned(), dim)],
        targets,
        barred: false,
    };
    if let Some((start, end)) = refresh_at {
        row.targets
            .push((start, end, TargetId::View(Spot::Cell(view.buttons_at, 0))));
    }
    row
}

/// One provider heading: its name dim, then its age dim, or "⟳ refreshing"
/// and the spinner in `attention` while its list refreshes.
fn heading_row(provider: &str, state: &Fresh, spinner: &str) -> Row {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut spans = vec![Span::styled(provider.to_owned(), dim)];
    match state {
        Fresh::Updated(age) => spans.push(Span::styled(format!(" · updated {age} ago"), dim)),
        Fresh::Refreshing => spans.push(Span::styled(
            format!(" ⟳ refreshing {spinner}"),
            Style::new().fg(Role::Attention.color()),
        )),
        Fresh::Unknown => {}
    }
    Row {
        spans,
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// A choice row's gutter: "› " on the focused row, two blank columns
/// elsewhere (`docs/tui.md`, "Look", "Overlays", "Choices").
fn gutter(focused: bool) -> Span<'static> {
    if focused {
        Span::styled("› ".to_owned(), Style::new().add_modifier(Modifier::BOLD))
    } else {
        Span::raw("  ".to_owned())
    }
}

/// The id's spans: in `accent`, with each query hit bold and underlined.
fn id_spans(id: &str, hits: &[bool]) -> Vec<Span<'static>> {
    let accent = Style::new().fg(Role::Accent.color());
    if !hits.iter().any(|hit| *hit) {
        return vec![Span::styled(id.to_owned(), accent)];
    }
    let bold = accent
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::UNDERLINED);
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut hit = String::new();
    for (got, matched) in id.chars().zip(hits.iter().copied()) {
        if matched {
            if !plain.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut plain), accent));
            }
            hit.push(got);
        } else {
            if !hit.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut hit), bold));
            }
            plain.push(got);
        }
    }
    if !hit.is_empty() {
        spans.push(Span::styled(hit, bold));
    }
    if !plain.is_empty() {
        spans.push(Span::styled(plain, accent));
    }
    spans
}

/// One choosing model's first row: the gutter, the id in `accent`,
/// each role dim in brackets, "● current" bold in `accent` on the
/// session's model, "· scoped" dim while every model shows, and the
/// rebuild cost dim at the right end. The name and roles cells take
/// clicks; the row takes clicks anywhere, selecting it. The focused row
/// sits on the bar.
fn model_first_row(model: &ModelRow, focused: bool) -> Row {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut spans = vec![gutter(focused)];
    spans.extend(id_spans(&model.id, &model.hits));
    let x = to_u16(2 + cells(&model.id));
    let mut targets = vec![(0, u16::MAX, TargetId::View(Spot::Row(model.at)))];
    targets.push((2, x, TargetId::View(Spot::Cell(model.at, 0))));
    if !model.roles.is_empty() {
        let roles = format!(" [{}]", model.roles.join("] ["));
        let wide = to_u16(cells(&roles));
        spans.push(Span::styled(roles, dim));
        targets.push((
            x,
            x.saturating_add(wide),
            TargetId::View(Spot::Cell(model.at, 1)),
        ));
    }
    if model.current {
        spans.push(Span::styled(
            " ● current".to_owned(),
            Style::new()
                .fg(Role::Accent.color())
                .add_modifier(Modifier::BOLD),
        ));
    }
    if model.scoped {
        spans.push(Span::styled(" · scoped".to_owned(), dim));
    }
    Row {
        spans,
        right: model
            .cost
            .clone()
            .map(|cost| vec![Span::styled(cost, dim)])
            .unwrap_or_default(),
        targets,
        barred: focused,
    }
}

/// One choosing model's second row, hung under the id: the gutter and
/// four more columns, then "thinking" dim and the levels, or "fixed
/// thinking" dim for a model with none.
/// On the focused model the levels are in `attention`, its saved level
/// bold, and the chosen chip bold in brackets; elsewhere they are dim.
/// Each chip takes clicks at its level.
fn model_second_row(model: &ModelRow, focused: bool) -> Row {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let attention = Style::new().fg(Role::Attention.color());
    let mut spans = vec![Span::raw("  ".to_owned()), Span::raw("    ".to_owned())];
    let mut targets = vec![(0, u16::MAX, TargetId::View(Spot::Row(model.at)))];
    if model.levels.is_empty() {
        spans.push(Span::styled("fixed thinking".to_owned(), dim));
        return Row {
            spans,
            right: Vec::new(),
            targets,
            barred: false,
        };
    }
    spans.push(Span::styled("thinking ".to_owned(), dim));
    let mut x = 2 + 4 + to_u16(cells("thinking "));
    // The chips start past the name, and past the roles when the row
    // shows any: the click numbering names cells, not rows.
    let chips_at = if model.roles.is_empty() { 1 } else { 2 };
    for (at, level) in model.levels.iter().enumerate() {
        if at != 0 {
            spans.push(Span::raw(" ".to_owned()));
            x = x.saturating_add(1);
        }
        let chosen = model.chip == Some(at);
        let text = if chosen {
            format!("[{level}]")
        } else {
            level.clone()
        };
        let wide = to_u16(cells(&text));
        let style = if focused && (chosen || model.saved == Some(at)) {
            attention.add_modifier(Modifier::BOLD)
        } else if focused {
            attention
        } else {
            dim
        };
        spans.push(Span::styled(text, style));
        targets.push((
            x,
            x.saturating_add(wide),
            TargetId::View(Spot::Cell(model.at, chips_at + at)),
        ));
        x = x.saturating_add(wide);
    }
    Row {
        spans,
        right: Vec::new(),
        targets,
        barred: false,
    }
}

/// A model chosen for this session only takes a third row, hung under
/// the id like the second.
fn session_row(model: &ModelRow) -> Row {
    Row {
        spans: vec![
            Span::raw("  ".to_owned()),
            Span::raw("    ".to_owned()),
            Span::styled(
                "ⓢ this session only · nothing saved".to_owned(),
                Style::new().add_modifier(Modifier::DIM),
            ),
        ],
        right: Vec::new(),
        targets: vec![(0, u16::MAX, TargetId::View(Spot::Row(model.at)))],
        barred: false,
    }
}

/// One checklist row: the gutter, the mark cell with its own target,
/// then the id. A marked row draws bold. Any other cell selects the row.
fn scope_row(model: &ModelRow, focused: bool) -> Row {
    let marked = model.mark.unwrap_or(false);
    let ink = if marked {
        Style::new().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let spans = vec![
        gutter(focused),
        Span::styled(
            if marked {
                "[x] ".to_owned()
            } else {
                "[ ] ".to_owned()
            },
            ink,
        ),
        Span::styled(model.id.clone(), ink),
    ];
    let x = 2 + 4 + to_u16(cells(&model.id));
    let mut targets = vec![(0, u16::MAX, TargetId::View(Spot::Row(model.at)))];
    targets.push((2, 6, TargetId::View(Spot::Cell(model.at, 0))));
    targets.push((6, x, TargetId::View(Spot::Cell(model.at, 1))));
    let mut spans = spans;
    if !model.roles.is_empty() {
        let roles = format!(" [{}]", model.roles.join("] ["));
        let wide = to_u16(cells(&roles));
        spans.push(Span::styled(
            roles,
            Style::new().add_modifier(Modifier::DIM),
        ));
        targets.push((
            x,
            x.saturating_add(wide),
            TargetId::View(Spot::Cell(model.at, 2)),
        ));
    }
    Row {
        spans,
        right: Vec::new(),
        targets,
        barred: focused,
    }
}

/// One flat model with its provider and freshness, in show order.
struct Flat<'view> {
    provider: &'view str,
    state: &'view Fresh,
    row: &'view ModelRow,
}

/// The shown models flat, in show order.
fn flats(view: &PickerView) -> Vec<Flat<'_>> {
    view.sections
        .iter()
        .flat_map(|section| {
            section.models.iter().map(|row| Flat {
                provider: section.provider.as_str(),
                state: &section.state,
                row,
            })
        })
        .collect()
}

/// The body rows the flat models from `start` to `end` take: each model
/// two rows, three for a session-only choice, with a heading row above
/// each provider's first shown model and a blank row between sections.
fn rows_for(flats: &[Flat], start: usize, end: usize) -> usize {
    let mut rows = 0;
    // A window starting inside a provider continues it with no heading;
    // any other provider change opens one.
    let mut before = start
        .checked_sub(1)
        .and_then(|at| flats.get(at))
        .map(|flat| flat.provider);
    for (at, flat) in flats.iter().enumerate().take(end).skip(start) {
        let headed = before.is_none_or(|provider| provider != flat.provider);
        before = Some(flat.provider);
        if headed {
            rows += if at == start { 1 } else { 2 };
        }
        rows += 2 + usize::from(flat.row.session_only);
    }
    rows
}

/// The flat models around the focused one that fit `fit` body rows: the
/// window starts at the focused model and takes earlier models while
/// they fit, then later ones, so the selection stays visible.
fn window(view: &PickerView, fit: usize) -> (usize, usize) {
    let flats = flats(view);
    if flats.is_empty() {
        return (0, 0);
    }
    let pos = flats
        .iter()
        .position(|flat| Some(flat.row.at) == view.focused)
        .unwrap_or(0)
        .min(flats.len().saturating_sub(1));
    let (mut start, mut end) = (pos, pos.saturating_add(1));
    while start > 0 && rows_for(&flats, start.saturating_sub(1), end) <= fit {
        start = start.saturating_sub(1);
    }
    while end < flats.len() && rows_for(&flats, start, end.saturating_add(1)) <= fit {
        end = end.saturating_add(1);
    }
    (start, end)
}

/// The body rows available for the models: the area less the chrome,
/// the buttons row with the filter above it, the status lines, and the
/// no-match line. The checklist takes no filter row.
fn model_fit(view: &PickerView, height: u16) -> usize {
    let reserved =
        1 + usize::from(view.buttons_at != 0) + view.status.len() + usize::from(view.no_match);
    usize::from(height.saturating_sub(CHROME)).saturating_sub(reserved)
}

/// The page step over `view` at `height`: the windowed models, which
/// the list pages by less one.
pub(crate) fn page_step(view: &PickerView, height: u16) -> usize {
    let (start, end) = window(view, model_fit(view, height));
    end.saturating_sub(start).max(1)
}

/// The spans' width in columns.
fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| cells(&span.content)).sum()
}

/// The overlay body for `view`: the filter, the buttons, the windowed
/// sections, and the status lines. Exactly one row is barred while a
/// model shows; none with only a message.
fn body(view: &PickerView, width: u16, height: u16) -> Vec<Row> {
    // The checklist takes no filter: its layout starts at the buttons
    // row, so `buttons_at` is 0 only there.
    let mut rows = if view.buttons_at == 0 {
        Vec::new()
    } else {
        vec![filter_row(view)]
    };
    let flats = flats(view);
    let (start, end) = window(view, model_fit(view, height));
    // The content's natural width sets the refresh click's columns: the
    // overlay cuts both alike on a narrower screen.
    let mut content: usize = 0;
    let mut widen = |row: &Row| {
        let mut wide = spans_width(&row.spans);
        if !row.right.is_empty() {
            wide = wide.saturating_add(spans_width(&row.right).saturating_add(1));
        }
        content = content.max(wide);
    };
    for row in &rows {
        widen(row);
    }
    let footer = overlay::legend(&view.footer);
    widen(&footer);
    let mut sections = Vec::new();
    let mut first = true;
    // A window starting inside a provider continues it with no
    // heading; any other provider change opens one.
    let mut before = start
        .checked_sub(1)
        .and_then(|at| flats.get(at))
        .map(|flat| flat.provider);
    for flat in flats.iter().take(end).skip(start) {
        let headed = before.is_none_or(|provider| provider != flat.provider);
        before = Some(flat.provider);
        if headed {
            if !first {
                sections.push(Row {
                    spans: Vec::new(),
                    right: Vec::new(),
                    targets: Vec::new(),
                    barred: false,
                });
            }
            sections.push(heading_row(flat.provider, flat.state, &view.spinner));
            first = false;
        }
        let focused = Some(flat.row.at) == view.focused;
        if flat.row.mark.is_some() {
            sections.push(scope_row(flat.row, focused));
        } else {
            sections.push(model_first_row(flat.row, focused));
            sections.push(model_second_row(flat.row, focused));
            if flat.row.session_only {
                sections.push(session_row(flat.row));
            }
        }
    }
    if view.no_match {
        sections.push(dim_row("No models match".to_owned()));
    }
    for status in &view.status {
        sections.push(dim_row(status.clone()));
    }
    for section in &sections {
        widen(section);
    }
    // The buttons row plugs the measured width: the refresh text rides
    // at the content's right end.
    let buttons = buttons_row(view, None);
    widen(&buttons);
    let inner = overlay::inner(overlay::width(
        u16::try_from(content).unwrap_or(u16::MAX),
        PREFER,
        width,
    )) as usize;
    let refresh = cells("⟳ refresh all");
    let refresh_at = inner
        .checked_sub(refresh)
        .map(|from| (to_u16(from), to_u16(inner)));
    rows.push(buttons_row(view, refresh_at));
    rows.extend(sections);
    rows
}

/// Draws the picker in the shared frame under the view's top row `area`
/// covers: the slab's ▄ edge on the area's first row, centred across it,
/// with the screen behind reading around the side margins. Returns the
/// slab rect.
pub(crate) fn draw(
    view: &PickerView,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
    let framed = overlay::Overlay {
        title: None,
        close: None,
        body: body(view, area.width, area.height),
        footer: Some(overlay::legend(&view.footer)),
        prefer: PREFER,
    };
    overlay::draw(buf, area, &framed, Place::Under { top: area.y }, targets)
}

#[cfg(test)]
#[path = "model_picker_tests.rs"]
mod tests;
