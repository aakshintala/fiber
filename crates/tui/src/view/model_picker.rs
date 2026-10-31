//! The model picker in the overlay frame under its view's top row
//! (`docs/tui.md`, "Swapped views", "Look", "Overlays"). The picker's top
//! row stays the view's header; the filter, the buttons and the sections
//! draw in the shared frame, centred under it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use self::install::{error_row, install_row, logged_out_row, outcome_rows};
use super::overlay::{self, Place, Row};
use crate::format::width as cells;
use crate::model_picker::{Fresh, LeftOutRow, LoggedOutRow, ModelRow, PickerView, Progress};
use crate::mouse::{Target, TargetId};
use crate::swapped::Spot;
use crate::text::to_u16;
use crate::theme::Role;

mod install;

/// The picker wraps at this content width before shrinking to what the
/// wrapped rows need (`docs/tui.md`, "Look", "Overlays").
const PREFER: u16 = 96;
/// Below this inner width, names and switch costs take priority over prices.
const PRICE_MIN_INNER: u16 = 60;
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
/// "›" dim and the query bold, with the cursor after it either way.
fn filter_row(view: &PickerView) -> Row {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let (_, cursor) = crate::view::cursor::cursor_cell(None, Role::Surface, Role::Text);
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
    spans.push(Span::styled("█".to_owned(), cursor));
    Row {
        spans,
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The buttons row: the count dim, the scope toggle bold, and, outside
/// the checklist, "⟳ refresh all" dim at the right end. The refresh
/// and toggle cells take clicks as their keys do; the count takes none.
/// The checklist draws no refresh button and no refresh target.
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
        right: Vec::new(),
        targets,
        barred: false,
    };
    // The checklist draws no refresh button and no refresh target.
    if view.refresh {
        row.right = vec![Span::styled("⟳ refresh all".to_owned(), dim)];
        if let Some((start, end)) = refresh_at {
            row.targets
                .push((start, end, TargetId::View(Spot::Cell(view.buttons_at, 0))));
        }
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
/// input and output prices dim, and the labelled switch cost dim at the
/// right end. The name and roles cells take
/// clicks; the row takes clicks anywhere, selecting it. The focused row
/// sits on the bar.
fn model_first_row(model: &ModelRow, focused: bool, priced: bool) -> Row {
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
    if priced && let Some(price) = &model.price {
        spans.push(Span::styled(format!(" {price}"), dim));
    }
    Row {
        spans,
        right: model
            .cost
            .clone()
            .map(|cost| {
                let text = if model.current {
                    cost
                } else {
                    format!("switch {cost}")
                };
                vec![Span::styled(text, dim)]
            })
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

/// One checklist row: the gutter, the mark, a space, the id in `accent`
/// with each query hit bold and underlined, each role dim in brackets,
/// "● current" bold in `accent` on the on-screen model, prices dim, and the
/// labelled switch cost dim at the right end. The mark reads dim exactly when unmarked
/// and takes no bold of its own: the bar bolds the focused row. The mark
/// and id cells take clicks as Enter does; the roles cell and the row
/// take clicks anywhere, selecting it. The focused row sits on the bar.
fn scope_row(model: &ModelRow, focused: bool, priced: bool) -> Row {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let marked = model.mark.unwrap_or(false);
    let mut spans = vec![gutter(focused)];
    spans.push(Span::styled(
        if marked {
            "[x] ".to_owned()
        } else {
            "[ ] ".to_owned()
        },
        if marked { Style::new() } else { dim },
    ));
    spans.extend(id_spans(&model.id, &model.hits));
    let id_end = to_u16(6 + cells(&model.id));
    let mut targets = vec![(0, u16::MAX, TargetId::View(Spot::Row(model.at)))];
    targets.push((2, 6, TargetId::View(Spot::Cell(model.at, 0))));
    targets.push((6, id_end, TargetId::View(Spot::Cell(model.at, 1))));
    if !model.roles.is_empty() {
        let roles = format!(" [{}]", model.roles.join("] ["));
        let wide = to_u16(cells(&roles));
        spans.push(Span::styled(roles, dim));
        targets.push((
            id_end,
            id_end.saturating_add(wide),
            TargetId::View(Spot::Cell(model.at, 2)),
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
    if priced && let Some(price) = &model.price {
        spans.push(Span::styled(format!(" {price}"), dim));
    }
    Row {
        spans,
        right: model
            .cost
            .clone()
            .map(|cost| {
                let text = if model.current {
                    cost
                } else {
                    format!("switch {cost}")
                };
                vec![Span::styled(text, dim)]
            })
            .unwrap_or_default(),
        targets,
        barred: focused,
    }
}

/// A checklist model's second row, hung under the id like the choosing
/// one: "thinking" dim and the levels dim, or "fixed thinking" dim for
/// a model with none. No level is chosen here, so no chip is bracketed
/// and no chip takes clicks: the row only selects its model.
fn scope_second_row(model: &ModelRow) -> Row {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut spans = vec![Span::raw("  ".to_owned()), Span::raw("    ".to_owned())];
    if model.levels.is_empty() {
        spans.push(Span::styled("fixed thinking".to_owned(), dim));
    } else {
        spans.push(Span::styled("thinking ".to_owned(), dim));
        for (at, level) in model.levels.iter().enumerate() {
            if at != 0 {
                spans.push(Span::raw(" ".to_owned()));
            }
            spans.push(Span::styled(level.clone(), dim));
        }
    }
    Row {
        spans,
        right: Vec::new(),
        targets: vec![(0, u16::MAX, TargetId::View(Spot::Row(model.at)))],
        barred: false,
    }
}

/// One flat model with its provider and freshness, in show order.
struct Flat<'view> {
    provider: &'view str,
    state: &'view Fresh,
    row: &'view ModelRow,
}

/// One flat count row with its provider and freshness, in show order.
struct Count<'view> {
    provider: &'view str,
    state: &'view Fresh,
    row: &'view LeftOutRow,
}

/// One windowed item, in draw order: a section's count rows stand
/// after its heading and before its models, followed by logged-out rows
/// and then the not-installed rows.
enum Item<'view> {
    Model(Flat<'view>),
    Count(Count<'view>),
    LoggedOut(&'view LoggedOutRow),
}

impl<'view> Item<'view> {
    /// The item's provider, when it opens or continues a model section.
    fn provider(&self) -> Option<&'view str> {
        match self {
            Item::Model(flat) => Some(flat.provider),
            Item::Count(count) => Some(count.provider),
            Item::LoggedOut(_) => None,
        }
    }

    /// The item's layout index. A count row's is never focused, so the
    /// focus lookup skips it exactly as the frame does.
    fn at(&self) -> usize {
        match self {
            Item::Model(flat) => flat.row.at,
            Item::Count(count) => count.row.at,
            Item::LoggedOut(row) => row.at,
        }
    }

    /// The body rows the item takes past its heading: two per model,
    /// three for a session-only choice, and one per count or logged-out row.
    fn rows(&self) -> usize {
        match self {
            Item::Model(flat) => 2 + usize::from(flat.row.session_only),
            Item::Count(_) | Item::LoggedOut(_) => 1,
        }
    }
}

/// The windowed items in draw order.
fn items(view: &PickerView) -> Vec<Item<'_>> {
    let mut out = Vec::new();
    for section in &view.sections {
        for row in &section.left_out {
            out.push(Item::Count(Count {
                provider: section.provider.as_str(),
                state: &section.state,
                row,
            }));
        }
        for row in &section.models {
            out.push(Item::Model(Flat {
                provider: section.provider.as_str(),
                state: &section.state,
                row,
            }));
        }
    }
    out.extend(view.logged_out.iter().map(Item::LoggedOut));
    out
}

/// The body rows the items from `start` to `end` take. The items are the
/// section items, logged-out rows and then the not-installed rows. A model
/// takes two rows, three for a session-only choice, a count or logged-out
/// row takes one, with a heading above each provider section and a blank
/// between sections and row groups. A not-installed row takes one, two
/// with its error, and the first in the window takes a blank above it when
/// anything precedes it there.
fn rows_for(view: &PickerView, items: &[Item], start: usize, end: usize) -> usize {
    let mut rows = 0;
    let logged_out_start = items
        .iter()
        .position(|item| matches!(item, Item::LoggedOut(_)))
        .unwrap_or(items.len());
    // A window starting inside a provider continues it with no heading;
    // any other provider change opens one. Logged-out rows have no heading.
    let mut before = start
        .checked_sub(1)
        .and_then(|at| items.get(at))
        .and_then(Item::provider);
    for at in start..end {
        if let Some(item) = items.get(at) {
            if let Some(provider) = item.provider() {
                let headed = before.is_none_or(|previous| previous != provider);
                before = Some(provider);
                if headed {
                    rows += if at == start { 1 } else { 2 };
                }
            } else if at == logged_out_start && at > start {
                rows += 1;
            }
            rows += item.rows();
        } else if let Some(install) = view.installs.get(at.saturating_sub(items.len())) {
            if at == items.len() && at > start {
                rows += 1;
            }
            rows += 1 + usize::from(matches!(install.progress, Some(Progress::Failed(_))));
        }
    }
    rows
}

/// The items around the focused one that fit `fit` body rows: the window
/// starts at the focused item and takes earlier items while they fit,
/// then later ones, so the focus stays visible.
fn window(view: &PickerView, fit: usize) -> (usize, usize) {
    let items = items(view);
    let total = items.len() + view.installs.len();
    if total == 0 {
        return (0, 0);
    }
    let pos = items
        .iter()
        .position(|item| Some(item.at()) == view.focused)
        .or_else(|| {
            view.installs
                .iter()
                .position(|install| Some(install.at) == view.focused)
                .map(|at| items.len() + at)
        })
        .unwrap_or(0)
        .min(total.saturating_sub(1));
    let (mut start, mut end) = (pos, pos.saturating_add(1));
    // Iterators bound each walk by `total`, so no comparison mutant can spin.
    for s in (0..pos).rev() {
        if rows_for(view, &items, s, end) > fit {
            break;
        }
        start = s;
    }
    for e in pos.saturating_add(2)..=total {
        if rows_for(view, &items, start, e) > fit {
            break;
        }
        end = e;
    }
    (start, end)
}

/// The body rows available for the items: the area less the chrome, the
/// filter and buttons rows above it, the status lines, the no-match
/// line, and the outcome lines under the list, which are not windowed.
fn model_fit(view: &PickerView, height: u16) -> usize {
    let reserved =
        2 + view.status.len() + usize::from(view.no_match) + outcome_rows(&view.outcomes).len();
    usize::from(height.saturating_sub(CHROME)).saturating_sub(reserved)
}

/// The page step over `view` at `height`: the windowed focusable model,
/// logged-out and install stops, which the list pages by less one. Count
/// rows consume viewport and window rows but never focus, so they stay in
/// the window without growing the step.
pub(crate) fn page_step(view: &PickerView, height: u16) -> usize {
    let fit = model_fit(view, height);
    let shown = items(view);
    let (start, end) = window(view, fit);
    let mut step = 0;
    for at in start..end {
        if let Some(item) = shown.get(at) {
            if matches!(item, Item::Model(_) | Item::LoggedOut(_)) {
                step += 1;
            }
        } else if view.installs.get(at.saturating_sub(shown.len())).is_some() {
            step += 1;
        }
    }
    step.max(1)
}

/// One scrollable body row and the selectable row, if this row is its
/// first display row. A short viewport trims a count with its heading.
struct DisplayRow {
    row: Row,
    focus: Option<usize>,
    trim_count: bool,
}

/// The flattened list body, focusable starts and each item's display
/// offset. Headings, blanks and nonfocusable counts all take offsets.
struct DisplayRows {
    rows: Vec<DisplayRow>,
    item_starts: Vec<usize>,
}

/// The spans' width in columns.
fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| cells(&span.content)).sum()
}

/// One left-out count row, hung under its heading like a model's second
/// row. It has no bar or targets: focus skips it and clicks ignore it.
fn count_row(row: &LeftOutRow) -> Row {
    Row {
        spans: vec![
            Span::raw("    ".to_owned()),
            Span::styled(row.text.clone(), Style::new().add_modifier(Modifier::DIM)),
        ],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// Every rendered list row in order, with selectable starts mapped back
/// to their layout rows. A count row and its provider heading are both
/// marked for the existing short-viewport trim.
fn display_rows(view: &PickerView, priced: bool) -> DisplayRows {
    let items = items(view);
    let logged_out_start = items
        .iter()
        .position(|item| matches!(item, Item::LoggedOut(_)))
        .unwrap_or(items.len());
    let mut rows = Vec::new();
    let mut item_starts = Vec::new();
    let mut before: Option<&str> = None;
    let mut first = true;
    for (at, item) in items.iter().enumerate() {
        let Some(provider) = item.provider() else {
            if at == logged_out_start && !rows.is_empty() {
                rows.push(DisplayRow {
                    row: Row {
                        spans: Vec::new(),
                        right: Vec::new(),
                        targets: Vec::new(),
                        barred: false,
                    },
                    focus: None,
                    trim_count: false,
                });
            }
            item_starts.push(rows.len());
            if let Item::LoggedOut(logged_out) = item {
                rows.push(DisplayRow {
                    row: logged_out_row(logged_out, Some(logged_out.at) == view.focused),
                    focus: Some(logged_out.at),
                    trim_count: false,
                });
            }
            continue;
        };
        let headed = before.is_none_or(|previous| previous != provider);
        item_starts.push(rows.len());
        let trim_count = headed && matches!(item, Item::Count(_));
        if headed {
            if !first {
                rows.push(DisplayRow {
                    row: Row {
                        spans: Vec::new(),
                        right: Vec::new(),
                        targets: Vec::new(),
                        barred: false,
                    },
                    focus: None,
                    trim_count,
                });
            }
            let state = match item {
                Item::Model(flat) => flat.state,
                Item::Count(count) => count.state,
                Item::LoggedOut(_) => unreachable!("logged-out rows have no provider heading"),
            };
            rows.push(DisplayRow {
                row: heading_row(provider, state, &view.spinner),
                focus: None,
                trim_count,
            });
            before = Some(provider);
            first = false;
        }
        match item {
            Item::Count(count) => rows.push(DisplayRow {
                row: count_row(count.row),
                focus: None,
                trim_count: true,
            }),
            Item::Model(flat) => {
                let focused = Some(flat.row.at) == view.focused;
                if flat.row.mark.is_some() {
                    rows.push(DisplayRow {
                        row: scope_row(flat.row, focused, priced),
                        focus: Some(flat.row.at),
                        trim_count: false,
                    });
                    rows.push(DisplayRow {
                        row: scope_second_row(flat.row),
                        focus: None,
                        trim_count: false,
                    });
                } else {
                    rows.push(DisplayRow {
                        row: model_first_row(flat.row, focused, priced),
                        focus: Some(flat.row.at),
                        trim_count: false,
                    });
                    rows.push(DisplayRow {
                        row: model_second_row(flat.row, focused),
                        focus: None,
                        trim_count: false,
                    });
                    if flat.row.session_only {
                        rows.push(DisplayRow {
                            row: session_row(flat.row),
                            focus: None,
                            trim_count: false,
                        });
                    }
                }
            }
            Item::LoggedOut(_) => unreachable!("handled without a provider heading"),
        }
        debug_assert!(at < items.len());
    }
    for (at, install) in view.installs.iter().enumerate() {
        if at == 0 && !rows.is_empty() {
            rows.push(DisplayRow {
                row: Row {
                    spans: Vec::new(),
                    right: Vec::new(),
                    targets: Vec::new(),
                    barred: false,
                },
                focus: None,
                trim_count: false,
            });
        }
        item_starts.push(rows.len());
        rows.push(DisplayRow {
            row: install_row(install, Some(install.at) == view.focused, &view.spinner),
            focus: Some(install.at),
            trim_count: false,
        });
        rows.extend(error_row(install).into_iter().map(|row| DisplayRow {
            row,
            focus: None,
            trim_count: false,
        }));
    }
    DisplayRows { rows, item_starts }
}

/// The focus-derived list start, using the same item window as the
/// keyboard page step but returning a displayed body-row offset.
pub(crate) fn focus_window_top(view: &PickerView, height: u16) -> usize {
    let fit = model_fit(view, height);
    let (start, _) = window(view, fit);
    display_rows(view, false)
        .item_starts
        .get(start)
        .copied()
        .unwrap_or(0)
}

/// The stored top clamped to the current list and moved only enough to
/// keep the focused row's first display row visible.
pub(crate) fn keep_focus_visible_top(view: &PickerView, height: u16) -> usize {
    let display = display_rows(view, false);
    let fit = model_fit(view, height).max(usize::from(view.focused.is_some()));
    let max = display.rows.len().saturating_sub(fit);
    let top = view.top.min(max);
    if fit == 0 {
        return top;
    }
    let Some(focused) = view
        .focused
        .and_then(|at| display.rows.iter().position(|row| row.focus == Some(at)))
    else {
        return top;
    };
    // Clamp the window to keep the focus visible; separate `<` and `>=`
    // branches would leave equivalent mutants on equality (removed).
    let low = focused.saturating_add(1).saturating_sub(fit);
    top.clamp(low, focused).min(max)
}

/// The next body-row top and the nearest visible selectable layout row,
/// if wheel movement carries the current focus offscreen.
pub(crate) fn picker_wheel_window(
    view: &PickerView,
    up: bool,
    height: u16,
) -> (usize, Option<usize>) {
    let display = display_rows(view, false);
    let fit = model_fit(view, height).max(usize::from(view.focused.is_some()));
    let max = display.rows.len().saturating_sub(fit);
    let effective = keep_focus_visible_top(view, height);
    let top = if up {
        effective.saturating_sub(3)
    } else {
        effective.saturating_add(3).min(max)
    };
    let end = top.saturating_add(fit);
    let Some(focused) = view.focused else {
        return (top, None);
    };
    let current = display
        .rows
        .iter()
        .position(|row| row.focus == Some(focused));
    if current.is_some_and(|row| row >= top && row < end) {
        return (top, None);
    }
    // Nearest by distance only; a directional tie-break would leave
    // mutants on arithmetic with no consumer-visible distinction, since
    // an offscreen focus has a unique nearest visible row (removed).
    let nearest = display
        .rows
        .iter()
        .enumerate()
        .filter(|(row, entry)| *row >= top && *row < end && entry.focus.is_some())
        .min_by_key(|(row, _)| current.map_or(0, |old| old.abs_diff(*row)))
        .and_then(|(_, entry)| entry.focus);
    (top, nearest)
}

fn body(view: &PickerView, width: u16, height: u16) -> Vec<Row> {
    let priced = overlay::inner(overlay::width(PREFER, PREFER, width)) >= PRICE_MIN_INNER;
    let fit = model_fit(view, height);
    let visible = fit.max(usize::from(view.focused.is_some()));
    let display = display_rows(view, priced);
    let top = keep_focus_visible_top(view, height).min(display.rows.len());
    let mut list: Vec<Row> = display
        .rows
        .into_iter()
        .skip(top)
        .take(visible)
        .filter_map(|row| {
            if fit < 2 && row.trim_count {
                None
            } else {
                Some(row.row)
            }
        })
        .collect();
    list.extend(outcome_rows(&view.outcomes));
    if view.no_match {
        list.push(dim_row("No models match".to_owned()));
    }
    for status in &view.status {
        list.push(dim_row(status.clone()));
    }

    // The filter and button rows stay fixed; only the provider/list rows
    // above are windowed. Width is measured from the rows actually shown.
    // Width is measured from the spans alone: the footer legend always
    // outruns any right column here, so testing it adds only an
    // equivalent mutant on emptiness (removed).
    let mut rows = vec![filter_row(view)];
    let mut content: usize = 0;
    let mut widen = |row: &Row| {
        content = content.max(spans_width(&row.spans));
    };
    for row in &rows {
        widen(row);
    }
    let footer = overlay::legend(&view.footer);
    widen(&footer);
    for row in &list {
        widen(row);
    }
    let buttons = buttons_row(view, None);
    widen(&buttons);
    let inner = overlay::inner(overlay::width(
        u16::try_from(content).unwrap_or(u16::MAX),
        PREFER,
        width,
    )) as usize;
    let refresh = cells("⟳ refresh all");
    let refresh_at = if view.refresh {
        inner
            .checked_sub(refresh)
            .map(|from| (to_u16(from), to_u16(inner)))
    } else {
        None
    };
    rows.push(buttons_row(view, refresh_at));
    rows.extend(list);
    rows
}

/// Whether `content` is a chosen chip: `[text]` with a non-empty
/// spaceless middle, whose brackets never copy
/// (`docs/tui.md`, "Selection and copy").
// Exempt: the only mutant no test tells apart is `len() > 2` to `<`,
// and a string under two bytes long cannot both start with `[` and
// end with `]`; every other shape of chip or bracket reaches the
// noted-rows test below.
#[cfg_attr(false, mutants::skip)]
fn is_chip(content: &str) -> bool {
    content.len() > 2
        && content.starts_with('[')
        && content.ends_with(']')
        && !content[1..content.len() - 1].contains(' ')
}

/// The picker's noted text rows in `area` with the drawn `slab`: each
/// kept body row's drawn cells with a chosen chip's brackets left out,
/// the row's targets and bars untouched
/// (`docs/tui.md`, "Selection and copy"). A span that is nothing but
/// `[text]` is a chosen chip; any other brackets stay, as drawn. The
/// body rebuilds from the same pure rows the draw keeps, so the noted
/// rows sit where the draw put them.
pub(crate) fn note_selection(
    view: &PickerView,
    area: Rect,
    slab: Rect,
) -> Vec<crate::app::SpanRow> {
    let framed = overlay::Overlay {
        title: None,
        close: None,
        body: body(view, area.width, area.height),
        footer: Some(overlay::legend(&view.footer)),
        prefer: PREFER,
    };
    let (_, placed) = overlay::body_rows(&framed, area, Place::Under { top: area.y });
    let (text_x, inner) = overlay::inner_width(slab);
    let room = usize::from(inner);
    placed
        .into_iter()
        .filter_map(|(y, at)| {
            let row = framed.body.get(at)?;
            if row.spans.is_empty() {
                return None;
            }
            let spans: Vec<Span<'static>> = row
                .spans
                .iter()
                .map(|span| {
                    let content = span.content.as_ref();
                    if is_chip(content) {
                        let inner = content[1..content.len() - 1].to_owned();
                        Span::styled(inner, span.style)
                    } else {
                        span.clone()
                    }
                })
                .collect();
            Some(crate::app::SpanRow {
                y,
                x: text_x,
                end: slab.x.saturating_add(slab.width),
                text: overlay::cut_text(&spans, room),
                join: crate::rows::Join::Break,
            })
        })
        .collect()
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
