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
use crate::model_picker::{Fresh, InstallRow, LeftOutRow, LoggedOutRow, ModelRow, PickerView};
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

/// One rendered body row: each variant carries the row it renders, so
/// the renderer matches on the part alone and no incompatible slot/part
/// pair is representable. The walk emits only the parts its slot's data
/// supports, so the count agrees with the renderer by construction.
enum Part<'view> {
    ModelFirst(&'view Flat<'view>),
    ModelSecond(&'view Flat<'view>),
    ScopeFirst(&'view Flat<'view>),
    ScopeSecond(&'view Flat<'view>),
    SessionNote(&'view Flat<'view>),
    Count(&'view Count<'view>),
    LoggedOut(&'view LoggedOutRow),
    Install(&'view InstallRow),
    InstallError(&'view InstallRow),
}

/// One laid-out step: an item or an install in draw order.
#[derive(Clone, Copy)]
enum Slot<'view> {
    Item(&'view Item<'view>),
    Install(&'view InstallRow),
}

impl<'view> Slot<'view> {
    /// The only place an item's own row count is decided: a plain model
    /// takes two rows and a third for a session-only choice, a checklist
    /// model takes two dim rows whatever the third-row flag says, and a
    /// count, a logged-out row or an install takes one, two with its
    /// error. Every list is non-empty, so a step always emits at least
    /// one row. Each part carries its row's data, so only the part that
    /// matches this slot's data can be built.
    fn parts(self) -> impl Iterator<Item = Part<'view>> {
        let (first, second, third): (
            Option<Part<'view>>,
            Option<Part<'view>>,
            Option<Part<'view>>,
        ) = match self {
            Slot::Item(item) => match item {
                Item::Model(flat) if flat.row.mark.is_some() => (
                    Some(Part::ScopeFirst(flat)),
                    Some(Part::ScopeSecond(flat)),
                    None,
                ),
                Item::Model(flat) if flat.row.session_only => (
                    Some(Part::ModelFirst(flat)),
                    Some(Part::ModelSecond(flat)),
                    Some(Part::SessionNote(flat)),
                ),
                Item::Model(flat) => (
                    Some(Part::ModelFirst(flat)),
                    Some(Part::ModelSecond(flat)),
                    None,
                ),
                Item::Count(count) => (Some(Part::Count(count)), None, None),
                Item::LoggedOut(row) => (Some(Part::LoggedOut(row)), None, None),
            },
            Slot::Install(install) if install::error_row(install).is_some() => (
                Some(Part::Install(install)),
                Some(Part::InstallError(install)),
                None,
            ),
            Slot::Install(install) => (Some(Part::Install(install)), None, None),
        };
        [first, second, third].into_iter().flatten()
    }
}

/// One windowed step in draw order: an item or an install with its
/// heading when it opens a provider section, and whether a blank
/// separates it from the earlier rows inside the window.
struct Step<'view> {
    slot: Slot<'view>,
    heading: Option<(&'view str, &'view Fresh)>,
    blank: bool,
}

impl Step<'_> {
    /// A step's rows: its blank, its heading and one row per part.
    fn rows(&self) -> usize {
        usize::from(self.blank) + usize::from(self.heading.is_some()) + self.slot.parts().count()
    }
}

/// One walk over the steps `span` covers: a step per item and install in
/// draw order, each with its blank, its heading and its parts. `span.end`
/// clamps to the item total, so an empty or inverted span yields nothing.
/// A window starting inside a section continues it with no heading: the
/// previous provider seeds from the item before the window. A step takes
/// a blank when an in-window row precedes it and it opens a section or a
/// row group.
fn walk<'view>(
    view: &'view PickerView,
    items: &'view [Item<'view>],
    span: std::ops::Range<usize>,
) -> impl Iterator<Item = Step<'view>> {
    let total = items.len() + view.installs.len();
    let end = span.end.min(total);
    let logged_out_start = items
        .iter()
        .position(|item| matches!(item, Item::LoggedOut(_)))
        .unwrap_or(items.len());
    // A window starting inside a provider continues it with no heading;
    // any other provider change opens one. Logged-out rows and installs
    // have no heading: the seed is none at 0 and past the items.
    let mut before = span
        .start
        .checked_sub(1)
        .and_then(|at| items.get(at))
        .and_then(Item::provider);
    (span.start..end).filter_map(move |at| {
        if let Some(item) = items.get(at) {
            let heading = match item {
                Item::Model(flat) => {
                    let headed = before.is_none_or(|previous| previous != flat.provider);
                    before = Some(flat.provider);
                    headed.then_some((flat.provider, flat.state))
                }
                Item::Count(count) => {
                    let headed = before.is_none_or(|previous| previous != count.provider);
                    before = Some(count.provider);
                    headed.then_some((count.provider, count.state))
                }
                Item::LoggedOut(_) => None,
            };
            let blank = at > span.start
                && (heading.is_some()
                    || at == logged_out_start && matches!(item, Item::LoggedOut(_)));
            Some(Step {
                slot: Slot::Item(item),
                heading,
                blank,
            })
        } else if let Some(install) = view.installs.get(at.saturating_sub(items.len())) {
            let blank = at > span.start && at == items.len();
            Some(Step {
                slot: Slot::Install(install),
                heading: None,
                blank,
            })
        } else {
            None
        }
    })
}

/// The body rows the items from `start` to `end` take: the walk's steps
/// summed, so the prediction is the renderer's own count. The window
/// boundary is a parameter of the walk: a window starting inside a
/// section continues it with no heading.
fn rows_for(view: &PickerView, items: &[Item], start: usize, end: usize) -> usize {
    walk(view, items, start..end).map(|step| step.rows()).sum()
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

/// The filter and buttons rows, status lines, no-match line and outcomes
/// under the list, which are not windowed.
fn reserved_rows(view: &PickerView) -> usize {
    2 + view.status.len() + usize::from(view.no_match) + outcome_rows(&view.outcomes).len()
}

/// The body rows available for the items with the footer and its gap.
fn model_fit(view: &PickerView, height: u16) -> usize {
    usize::from(height.saturating_sub(CHROME)).saturating_sub(reserved_rows(view))
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

/// One blank separator row: no spans, no focus, trimmed only with a
/// count heading.
fn blank_row(trim_count: bool) -> DisplayRow {
    DisplayRow {
        row: Row {
            spans: Vec::new(),
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        },
        focus: None,
        trim_count,
    }
}

/// Every rendered list row in order, with selectable starts mapped back
/// to their layout rows. A count row and its provider heading are both
/// marked for the existing short-viewport trim.
fn display_rows(view: &PickerView, priced: bool) -> DisplayRows {
    let items = items(view);
    let total = items.len() + view.installs.len();
    let mut rows = Vec::new();
    let mut item_starts = Vec::new();
    for step in walk(view, &items, 0..total) {
        if let Some((provider, state)) = step.heading {
            // A headed step starts at its blank, while the first
            // logged-out and install rows start past theirs, so the
            // scroll top reads past the group blank.
            item_starts.push(rows.len());
            let trim_count = matches!(step.slot, Slot::Item(Item::Count(_)));
            if step.blank {
                rows.push(blank_row(trim_count));
            }
            rows.push(DisplayRow {
                row: heading_row(provider, state, &view.spinner),
                focus: None,
                trim_count,
            });
        } else {
            if step.blank {
                rows.push(blank_row(false));
            }
            item_starts.push(rows.len());
        }
        for part in step.slot.parts() {
            rows.push(match part {
                Part::ModelFirst(flat) => {
                    let focused = Some(flat.row.at) == view.focused;
                    DisplayRow {
                        row: model_first_row(flat.row, focused, priced),
                        focus: Some(flat.row.at),
                        trim_count: false,
                    }
                }
                Part::ModelSecond(flat) => {
                    let focused = Some(flat.row.at) == view.focused;
                    DisplayRow {
                        row: model_second_row(flat.row, focused),
                        focus: None,
                        trim_count: false,
                    }
                }
                Part::ScopeFirst(flat) => {
                    let focused = Some(flat.row.at) == view.focused;
                    DisplayRow {
                        row: scope_row(flat.row, focused, priced),
                        focus: Some(flat.row.at),
                        trim_count: false,
                    }
                }
                Part::ScopeSecond(flat) => DisplayRow {
                    row: scope_second_row(flat.row),
                    focus: None,
                    trim_count: false,
                },
                Part::SessionNote(flat) => DisplayRow {
                    row: session_row(flat.row),
                    focus: None,
                    trim_count: false,
                },
                Part::Count(count) => DisplayRow {
                    row: count_row(count.row),
                    focus: None,
                    trim_count: true,
                },
                Part::LoggedOut(row) => DisplayRow {
                    row: logged_out_row(row, Some(row.at) == view.focused),
                    focus: Some(row.at),
                    trim_count: false,
                },
                Part::Install(install) => {
                    let focused = Some(install.at) == view.focused;
                    DisplayRow {
                        row: install_row(install, focused, &view.spinner),
                        focus: Some(install.at),
                        trim_count: false,
                    }
                }
                Part::InstallError(install) => {
                    let Some(row) = error_row(install) else {
                        unreachable!("the walk emits an error part only for a failed install")
                    };
                    DisplayRow {
                        row,
                        focus: None,
                        trim_count: false,
                    }
                }
            });
        }
    }
    DisplayRows { rows, item_starts }
}

/// The model whose provider heading must stay with it in a short viewport.
fn compact_model_item(view: &PickerView, items: &[Item], display: &DisplayRows) -> Option<usize> {
    if let Some(focused) = view.focused {
        return items
            .iter()
            .position(|item| matches!(item, Item::Model(flat) if flat.row.at == focused));
    }
    if view.top > 0
        && let Some(at) = display.rows.iter().skip(view.top).find_map(|row| row.focus)
        && let Some(item) = items
            .iter()
            .position(|item| matches!(item, Item::Model(flat) if flat.row.at == at))
    {
        return Some(item);
    }
    items
        .iter()
        .position(|item| matches!(item, Item::Model(flat) if flat.row.current))
        .or_else(|| items.iter().position(|item| matches!(item, Item::Model(_))))
}

/// The rows the picker can use without its footer and gap.
fn footerless_fit(view: &PickerView, height: u16) -> usize {
    usize::from(height.saturating_sub(CHROME.saturating_sub(2))).saturating_sub(reserved_rows(view))
}

/// The rows the picker can use after yielding its filter and buttons too.
fn footerless_fit_without_controls(view: &PickerView, height: u16) -> usize {
    footerless_fit(view, height).saturating_add(2)
}

/// Whether the short picker must give up the footer, and if needed its
/// controls, to keep a model and its provider heading visible.
fn short_model_fit(view: &PickerView, height: u16) -> Option<usize> {
    let fit = model_fit(view, height);
    let items = items(view);
    let display = display_rows(view, false);
    let item = compact_model_item(view, &items, &display)?;
    let Item::Model(flat) = items.get(item)? else {
        return None;
    };
    let model = display
        .rows
        .iter()
        .position(|row| row.focus == Some(flat.row.at))?;
    let heading = (0..model).rev().find(|at| {
        display.rows.get(*at).is_some_and(|row| {
            row.focus.is_none()
                && row
                    .row
                    .spans
                    .first()
                    .is_some_and(|span| span.content == flat.provider)
        })
    })?;
    let visible = fit.max(usize::from(view.focused.is_some()));
    let top = keep_focus_visible_top(view, height);
    let end = top.saturating_add(visible);
    let footerless = footerless_fit(view, height);
    let compact = if footerless < 2 {
        footerless_fit_without_controls(view, height)
    } else {
        footerless
    };
    (fit < 2 || heading < top || model >= end).then_some(compact)
}

/// The minimum list window, with the chosen model's provider heading first.
fn compact_model_rows(view: &PickerView, priced: bool, fit: usize) -> Option<Vec<Row>> {
    let items = items(view);
    let display = display_rows(view, priced);
    let item = compact_model_item(view, &items, &display)?;
    let Item::Model(flat) = items.get(item)? else {
        return None;
    };
    let start = display
        .rows
        .iter()
        .position(|row| row.focus == Some(flat.row.at))?;
    let focused = Some(flat.row.at) == view.focused;
    if fit < 2 {
        return Some(vec![model_first_row(flat.row, focused, priced)]);
    }
    let mut rows = vec![heading_row(flat.provider, flat.state, &view.spinner)];
    rows.extend(
        display
            .rows
            .into_iter()
            .skip(start)
            .take(fit.saturating_sub(1))
            .map(|row| row.row),
    );
    Some(rows)
}

/// The picker footer, which yields when its rows are needed for a model.
fn picker_footer(view: &PickerView, height: u16) -> Option<Row> {
    short_model_fit(view, height)
        .is_none()
        .then(|| overlay::legend(&view.footer))
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
/// keep the focused row's first display row visible at `fit` rows.
fn keep_focus_visible_top_at_fit(view: &PickerView, fit: usize) -> usize {
    let display = display_rows(view, false);
    let fit = fit.max(usize::from(view.focused.is_some()));
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

/// The stored top clamped to the current list and moved only enough to
/// keep the focused row's first display row visible.
pub(crate) fn keep_focus_visible_top(view: &PickerView, height: u16) -> usize {
    keep_focus_visible_top_at_fit(
        view,
        model_fit(view, height).max(usize::from(view.focused.is_some())),
    )
}

/// The next body-row top and the nearest visible selectable layout row,
/// if wheel movement carries the current focus offscreen.
pub(crate) fn picker_wheel_window(
    view: &PickerView,
    up: bool,
    height: u16,
) -> (usize, Option<usize>) {
    let display = display_rows(view, false);
    // When fewer than two body rows fit, a short draw yields the footer
    // and, if needed, its controls to keep a model with its heading. Wheel
    // against that expanded window or the step can land on a nonfocusable
    // detail row and leave the focused model pinned.
    let fit = model_fit(view, height);
    let fit = if fit < 2 {
        short_model_fit(view, height).unwrap_or(fit)
    } else {
        fit
    }
    .max(usize::from(view.focused.is_some()));
    let max = display.rows.len().saturating_sub(fit);
    let effective = keep_focus_visible_top_at_fit(view, fit);
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
    let short_fit = short_model_fit(view, height);
    let controls = short_fit.is_none_or(|_| footerless_fit(view, height) >= 2);
    let list = if let Some(fit) = short_fit {
        let mut list = compact_model_rows(view, priced, fit).unwrap_or_default();
        if controls {
            list.extend(outcome_rows(&view.outcomes));
            if view.no_match {
                list.push(dim_row("No models match".to_owned()));
            }
            for status in &view.status {
                list.push(dim_row(status.clone()));
            }
        }
        list
    } else {
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
        list
    };

    // The filter and button rows stay fixed; only the provider/list rows
    // above are windowed. Width is measured from the rows actually shown.
    // Width is measured from the spans alone: the footer legend always
    // outruns any right column here, so testing it adds only an
    // equivalent mutant on emptiness (removed).
    let mut rows = if controls {
        vec![filter_row(view)]
    } else {
        Vec::new()
    };
    let mut content: usize = 0;
    let mut widen = |row: &Row| {
        content = content.max(spans_width(&row.spans));
    };
    for row in &rows {
        widen(row);
    }
    if let Some(footer) = picker_footer(view, height) {
        widen(&footer);
    }
    for row in &list {
        widen(row);
    }
    let buttons = buttons_row(view, None);
    if controls {
        widen(&buttons);
    }
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
    if controls {
        rows.push(buttons_row(view, refresh_at));
    }
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
        footer: picker_footer(view, area.height),
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
        footer: picker_footer(view, area.height),
        prefer: PREFER,
    };
    overlay::draw(buf, area, &framed, Place::Under { top: area.y }, targets)
}

#[cfg(test)]
#[path = "model_picker_tests.rs"]
mod tests;
