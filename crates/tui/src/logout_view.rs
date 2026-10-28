//! `/logout` (`docs/tui.md`, "Logging out"): the stored provider labels
//! and the declared secrets, a confirm step, and the same delete steps as
//! `fiber logout`. The delete removes the file only, leaving
//! `providers."<name>".credential` as is; no key or secret reaches a
//! notice or a log.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::configure::LogoutRow;
use crate::format;
use crate::keys::Key;
use crate::mouse::{Target, TargetId};
use crate::settings_view::{Act, Ctx};
use crate::swapped::Spot;
use crate::view::overlay;

/// A heading's group.
// debt: Group, Item, rows_key, target_rows, the list assembly in draw,
// list_chrome, shown_body_rows, text_row, blank_row and wrapped_rows
// mirror login_view.rs; share one list both views call once login_view.rs
// is in the fence. Upgrade trigger: the next ticket that touches
// login_view.rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    /// The stored and configured provider labels.
    Providers,
    /// The declared secrets.
    Secrets,
}

/// One row of the view: a heading, one target, the blank row between
/// the groups, or the note in place of every row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    /// A group's heading; Enter on it does nothing.
    Heading(Group),
    /// One provider label or secret: its index in the rows.
    Target(usize),
    /// The blank row between the providers and the secrets.
    Gap,
    /// The note shown with no label and no secret.
    Note,
}

/// What the view shows below its header.
#[derive(Debug)]
enum Mode {
    /// The rows.
    Rows,
    /// The delete confirm over the selected row: its index in the rows.
    Confirm {
        /// The row the confirm deletes.
        target: usize,
    },
}

/// The `/logout` view's state.
#[derive(Debug)]
pub(crate) struct Logout {
    rows: Vec<LogoutRow>,
    items: Vec<Item>,
    /// The focused row, an index into `items`: a target whenever one
    /// exists, else 0. Headings, the gap and the note never take it.
    selected: usize,
    /// The first body row shown: the wheel moves it three rows a step,
    /// and the keys move it only as far as needed to keep the focus
    /// shown.
    top: usize,
    mode: Mode,
    /// What the last action said, shown below the rows.
    said: Vec<String>,
}

impl Logout {
    /// The view, its rows read.
    pub(crate) fn open(ctx: &Ctx<'_>) -> Self {
        let (rows, items, said) = match ctx.seam.logout_rows() {
            Ok(rows) => {
                let items = build(&rows);
                (rows, items, Vec::new())
            }
            Err(error) => (Vec::new(), Vec::new(), vec![error.message]),
        };
        let mut logout = Self {
            rows,
            items,
            selected: 0,
            top: 0,
            mode: Mode::Rows,
            said,
        };
        logout.refocus();
        logout
    }

    /// Focuses the first deletable target; with none the first target,
    /// with no target 0, where the note sits.
    fn refocus(&mut self) {
        self.selected = self
            .items
            .iter()
            .position(|item| match item {
                Item::Target(at) => self.rows.get(*at).is_some_and(|row| row.deletable()),
                Item::Heading(_) | Item::Gap | Item::Note => false,
            })
            .or_else(|| {
                self.items
                    .iter()
                    .position(|item| matches!(item, Item::Target(_)))
            })
            .unwrap_or(0);
    }

    /// The selected row.
    fn selected(&self) -> Option<Item> {
        self.items.get(self.selected).copied()
    }

    /// Handles one key.
    pub(crate) fn key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if matches!(self.mode, Mode::Confirm { .. }) {
            self.confirm_key(key, ctx)
        } else {
            self.rows_key(key, ctx)
        }
    }

    /// A key over the rows: move over the target rows only, ask before
    /// deleting the selected row, or close.
    fn rows_key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            return Act::Close;
        }
        if *key == Key::Enter {
            if let Some(Item::Target(at)) = self.selected()
                && let Some(row) = self.rows.get(at)
                && row.deletable()
            {
                // Enter on a heading, the gap, the note or a dim row
                // does nothing: only a deletable row confirms.
                self.said.clear();
                self.mode = Mode::Confirm { target: at };
            }
            return Act::Stay;
        }
        if !matches!(*key, Key::Up | Key::Down | Key::PageUp | Key::PageDown) {
            return Act::Stay;
        }
        // With no body row shown both selections stay put.
        if shown_body_rows(ctx.height) == 0 {
            return Act::Stay;
        }
        let targets = self.target_rows();
        if targets.is_empty() {
            return Act::Stay;
        }
        let before = self.selected;
        if *key == Key::Up {
            // The nearest target above, clamped at the first.
            self.selected = targets
                .iter()
                .copied()
                .rfind(|at| *at < before)
                .unwrap_or(before);
        } else if *key == Key::Down {
            // The nearest target below, clamped at the last.
            self.selected = targets
                .iter()
                .copied()
                .find(|at| *at > before)
                .unwrap_or(before);
        } else {
            // A page is the shown body rows less one, at least one.
            let page = shown_body_rows(ctx.height).saturating_sub(1).max(1);
            if *key == Key::PageUp {
                let land = before.saturating_sub(page);
                // The nearest target at or above the landing row, else
                // the first target below it.
                self.selected = targets
                    .iter()
                    .copied()
                    .rfind(|at| *at <= land)
                    .or(targets.first().copied())
                    .unwrap_or(before);
            } else {
                let land = before
                    .saturating_add(page)
                    .min(self.items.len().saturating_sub(1));
                // The nearest target at or below the landing row, else
                // the last target above it.
                self.selected = targets
                    .iter()
                    .copied()
                    .find(|at| *at >= land)
                    .or(targets.last().copied())
                    .unwrap_or(before);
            }
        }
        Act::Stay
    }

    /// Shifts the provider list's window one wheel step: three body
    /// rows, clamped at either end, keeping the focus while it shows,
    /// else moving it to the nearest shown target row. The confirm
    /// takes no wheel: it does not scroll, so the wheel is swallowed
    /// there.
    pub(crate) fn wheel(&mut self, up: bool, ctx: &Ctx<'_>) -> Act {
        if !matches!(self.mode, Mode::Rows) {
            return Act::Stay;
        }
        let shown = shown_body_rows(ctx.height);
        if shown == 0 {
            return Act::Stay;
        }
        let rows = self.body_len(ctx.width);
        let end = rows.saturating_sub(shown);
        let next = if up {
            self.top.saturating_sub(3)
        } else {
            self.top.saturating_add(3).min(end)
        };
        self.top = next;
        // Clamp the focus into the window, onto a target row; with no
        // target shown the focus stays where it was.
        let targets = self.target_rows();
        if !targets.is_empty()
            && (self.selected < next || self.selected >= next.saturating_add(shown))
        {
            if self.selected < next {
                if let Some(at) = targets.iter().copied().find(|at| *at >= next) {
                    self.selected = at;
                }
            } else if let Some(at) = targets
                .iter()
                .copied()
                .rev()
                .find(|at| *at < next.saturating_add(shown))
            {
                self.selected = at;
            }
        }
        Act::Stay
    }

    /// How many body rows the provider list holds at `width` wide: one
    /// per item, then what the last action said with its blank row, at
    /// least three so the window never clips them.
    fn body_len(&self, width: usize) -> usize {
        let mut rows = self.items.len();
        if !self.said.is_empty() {
            let width = u16::try_from(width).unwrap_or(u16::MAX);
            let overlay = overlay::width(u16::MAX, 96, width);
            let inner = overlay::inner(overlay);
            rows = rows.saturating_add(1);
            for line in &self.said {
                rows = rows.saturating_add(format::wrap(line, usize::from(inner)).len());
            }
        }
        rows.max(3)
    }

    /// A key over the confirm: delete the confirmed row, or go back to
    /// the rows deleting nothing.
    fn confirm_key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            self.mode = Mode::Rows;
            self.said.clear();
            return Act::Stay;
        }
        if *key == Key::Enter {
            self.confirm(ctx);
        }
        Act::Stay
    }

    /// Deletes the confirmed row through the same steps as `fiber
    /// logout`, then re-lists the rows: success removes the stored row
    /// and notes the removed file, naming the providers that read the
    /// same stored directory; failure keeps the row and notes the
    /// seam's message. The overlay stays open either way.
    fn confirm(&mut self, ctx: &Ctx<'_>) {
        let Mode::Confirm { target } = &self.mode else {
            return;
        };
        let target = *target;
        let Some(row) = self.rows.get(target).cloned() else {
            self.mode = Mode::Rows;
            return;
        };
        match ctx
            .seam
            .delete_stored(row.name(), row.label().map(str::to_owned))
        {
            Ok(path) => {
                let mut line = format!("Removed {path}");
                if !row.siblings().is_empty() {
                    line.push_str(&format!(", which {} also reads", row.siblings().join(", ")));
                }
                line.push('.');
                match ctx.seam.logout_rows() {
                    Ok(rows) => {
                        self.rows = rows;
                        self.items = build(&self.rows);
                        self.refocus();
                        self.said = vec![line];
                    }
                    Err(error) => {
                        // The delete landed but the re-list failed: the
                        // row is gone, and both lines stay to say so.
                        self.rows.retain(|kept| {
                            kept.name() != row.name() || kept.label() != row.label()
                        });
                        self.items = build(&self.rows);
                        self.refocus();
                        self.said = vec![line, error.message];
                    }
                }
            }
            Err(error) => {
                self.said = vec![error.message];
            }
        }
        self.mode = Mode::Rows;
    }

    /// The target rows' indices in display order.
    fn target_rows(&self) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(at, item)| matches!(item, Item::Target(_)).then_some(at))
            .collect()
    }

    /// A click: the ✕ closes, a deletable target row selects it, and a
    /// click on a heading, the gap, the note, a dim row or past the last
    /// item does nothing. A click while the confirm is open keeps it.
    pub(crate) fn click(&mut self, spot: Spot, _ctx: &Ctx<'_>) -> Act {
        match spot {
            Spot::Close => Act::Close,
            Spot::Switch { .. } | Spot::Revoke(_) | Spot::Cell(_, _) | Spot::Item(_) => Act::Stay,
            Spot::Row(at) => {
                if matches!(self.mode, Mode::Confirm { .. }) {
                    return Act::Stay;
                }
                match self.items.get(at) {
                    Some(Item::Target(t)) => {
                        let deletable = self.rows.get(*t).is_some_and(|row| row.deletable());
                        if deletable {
                            self.selected = at;
                            self.said.clear();
                        }
                    }
                    Some(Item::Heading(_)) | Some(Item::Gap) | Some(Item::Note) | None => {}
                }
                Act::Stay
            }
        }
    }
}

/// Whether the row lists under Providers: a provider's label, stored or
/// configured. Secrets list under Secrets.
fn is_provider(row: &LogoutRow) -> bool {
    matches!(row, LogoutRow::Provider { .. })
}

fn build(rows: &[LogoutRow]) -> Vec<Item> {
    let mut items = Vec::new();
    if rows.iter().any(is_provider) {
        items.push(Item::Heading(Group::Providers));
        for (at, row) in rows.iter().enumerate() {
            if is_provider(row) {
                items.push(Item::Target(at));
            }
        }
    }
    if rows.iter().any(|row| !is_provider(row)) {
        // The blank row between the groups, only when both have rows.
        if !items.is_empty() {
            items.push(Item::Gap);
        }
        items.push(Item::Heading(Group::Secrets));
        for (at, row) in rows.iter().enumerate() {
            if !is_provider(row) {
                items.push(Item::Target(at));
            }
        }
    }
    if items.is_empty() {
        items.push(Item::Note);
    }
    items
}

impl Logout {
    /// Draws the list in the shared overlay frame, centred: dim
    /// headings, one choice row per label or secret with its default
    /// mark, dim rows for the origins no delete can take, the gap, then
    /// anything the last action said, at least three body rows before
    /// the window clips them, and the legend footer. The confirm draws
    /// its own overlay.
    pub(crate) fn draw(&self, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) -> Rect {
        if matches!(self.mode, Mode::Confirm { .. }) {
            return self.draw_confirm(area, buf, targets);
        }
        let width = overlay::width(u16::MAX, 96, area.width);
        let inner_w = overlay::inner(width);
        let budget = usize::from(inner_w).saturating_sub(4);
        // One choice row per target, each exactly one row: the cut
        // names leave every tag a single row at any area width.
        let mut cut: Vec<(usize, String, bool)> = Vec::new();
        for (at, item) in self.items.iter().enumerate() {
            if let Item::Target(t) = *item
                && let Some(row) = self.rows.get(t)
            {
                cut.push((at, format::cut(&row_text(row), budget), !row.deletable()));
            }
        }
        let listed: Vec<(&str, &str)> =
            cut.iter().map(|(_, name, _)| (name.as_str(), "")).collect();
        let focused = cut
            .iter()
            .position(|(at, _, _)| *at == self.selected)
            .unwrap_or(0);
        let mut choices = overlay::choices(&listed, focused, u16::MAX);
        // Only a deletable target row takes clicks over its whole row;
        // headings, the gap, the note and dim rows carry none. Dim rows
        // draw dim, focused or not.
        let dim = Style::new().add_modifier(Modifier::DIM);
        for (row, (at, _, dimmed)) in choices.iter_mut().zip(cut.iter()) {
            if *dimmed {
                row.targets.clear();
                row.spans = row
                    .spans
                    .iter()
                    .map(|span| Span::styled(span.content.clone().into_owned(), dim))
                    .collect();
            } else {
                row.targets
                    .push((0, u16::MAX, TargetId::View(Spot::Row(*at))));
            }
        }
        let mut choices = choices.into_iter();
        let mut body: Vec<overlay::Row> = Vec::new();
        for item in &self.items {
            match *item {
                Item::Heading(Group::Providers) => body.push(text_row("Providers", dim)),
                Item::Heading(Group::Secrets) => body.push(text_row("Secrets", dim)),
                Item::Target(_) => body.push(choices.next().unwrap_or_else(blank_row)),
                Item::Gap => body.push(blank_row()),
                Item::Note => body.push(text_row(
                    "No provider is installed, and no installed extension declares a secret.",
                    dim,
                )),
            }
        }
        // What the last action said follows the items, one blank row
        // between; with no items it is the whole body. The rows wrap
        // at the preferred width and are never cut sideways.
        if !self.said.is_empty() {
            if !body.is_empty() {
                body.push(blank_row());
            }
            for line in &self.said {
                for row in format::wrap(line, usize::from(inner_w)) {
                    body.push(text_row(row, Style::new()));
                }
            }
        }
        for _ in body.len()..3 {
            body.push(blank_row());
        }
        // The footer carries trailing blanks up to the widest of every
        // body row, not only the shown ones, so the overlay's width
        // never depends on which rows the window shows.
        let wide = body
            .iter()
            .map(|row| {
                row.spans
                    .iter()
                    .map(|span| format::width(&span.content))
                    .sum()
            })
            .max()
            .unwrap_or(0);
        let mut footer = overlay::legend(&[("↑↓", "move"), ("Enter", "delete"), ("Esc", "close")]);
        let have: usize = footer
            .spans
            .iter()
            .map(|span| format::width(&span.content))
            .sum();
        footer
            .spans
            .push(Span::raw(" ".repeat(wide.saturating_sub(have))));
        let shown = shown_body_rows(usize::from(area.height));
        // The window starts at the wheel's top row, moved only as far
        // as needed to keep the focus shown: the keys never re-derive
        // the window from the focus, so a wheeled position survives
        // them, and a shorter body clamps it back into range.
        let start = if body.is_empty() || shown == 0 {
            0
        } else {
            let end = body.len().saturating_sub(shown);
            let low = self.selected.saturating_add(1).saturating_sub(shown);
            self.top.clamp(low, self.selected).min(end)
        };
        let end = start.saturating_add(shown).min(body.len());
        let framed = overlay::Overlay {
            title: Some(("Log out".to_owned(), Some(Span::raw("✕")))),
            close: Some(TargetId::View(Spot::Close)),
            body: body
                .into_iter()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect(),
            footer: Some(footer),
            prefer: 96,
        };
        overlay::draw(buf, area, &framed, overlay::Place::Centre, targets)
    }

    /// Draws the confirm over `area`: one overlay titled with the
    /// provider or secret name, naming the label it deletes and the
    /// providers reading the same stored directory. The body holds the
    /// confirm's own lines only, never the list's row count or width.
    fn draw_confirm(&self, area: Rect, buf: &mut Buffer, targets_out: &mut Vec<Target>) -> Rect {
        let Mode::Confirm { target } = &self.mode else {
            return Rect::default();
        };
        let Some(row) = self.rows.get(*target) else {
            return Rect::default();
        };
        let width = overlay::width(u16::MAX, 96, area.width);
        let inner = usize::from(overlay::inner(width));
        let mut body = wrapped_rows(&delete_line(row), inner, Style::new());
        if !row.siblings().is_empty() {
            body.extend(wrapped_rows(
                &format!("Also used by: {}", row.siblings().join(", ")),
                inner,
                Style::new(),
            ));
        }
        body.truncate(shown_body_rows(usize::from(area.height)));
        let footer = overlay::legend(&[("Enter", "confirm"), ("Esc", "back")]);
        let framed = overlay::Overlay {
            title: Some((row.name().to_owned(), Some(Span::raw("✕")))),
            close: Some(TargetId::View(Spot::Close)),
            body,
            footer: Some(footer),
            prefer: 96,
        };
        overlay::draw(buf, area, &framed, overlay::Place::Centre, targets_out)
    }
}

/// The list overlay's chrome in rows, measured with a one-row probe so
/// the count never depends on the body: the edges, the pads, the title
/// and its gap, and the footer and its gap.
fn list_chrome() -> usize {
    let probe = overlay::Overlay {
        title: Some(("Log out".to_owned(), Some(Span::raw("✕")))),
        close: None,
        body: vec![overlay::Row {
            spans: Vec::new(),
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        }],
        footer: Some(overlay::legend(&[
            ("↑↓", "move"),
            ("Enter", "delete"),
            ("Esc", "close"),
        ])),
        prefer: 96,
    };
    usize::from(overlay::height(&probe)).saturating_sub(1)
}

/// How many body rows the list overlay shows at `height` rows tall:
/// the height less its chrome, or none when the chrome fills it.
fn shown_body_rows(height: usize) -> usize {
    height.saturating_sub(list_chrome())
}

/// One row's text as drawn: the provider and its label with the
/// default mark, and the origin a dim row's key comes from; or the
/// secret's name.
fn row_text(row: &LogoutRow) -> String {
    match row {
        LogoutRow::Provider {
            name,
            label,
            default,
            deletable,
            origin,
            ..
        } => {
            let mut text = format!("{name} {label}");
            if *default {
                text.push_str(" (default)");
            }
            if !deletable && let Some(origin) = origin {
                text.push_str(&format!(" ({origin})"));
            }
            text
        }
        LogoutRow::Secret { name, .. } => name.clone(),
    }
}

/// The confirm's first line: the exact provider and label, or the
/// secret, the delete names.
fn delete_line(row: &LogoutRow) -> String {
    match row {
        LogoutRow::Provider { name, label, .. } => format!("Delete {name}/{label}?"),
        LogoutRow::Secret { name, .. } => format!("Delete {name}?"),
    }
}

/// One overlay body row holding `text` in `style`.
fn text_row(text: impl Into<String>, style: Style) -> overlay::Row {
    overlay::Row {
        spans: vec![Span::styled(text.into(), style)],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// One blank overlay body row.
fn blank_row() -> overlay::Row {
    overlay::Row {
        spans: Vec::new(),
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// One plain row per wrapped line of `text`.
fn wrapped_rows(text: &str, inner: usize, style: Style) -> Vec<overlay::Row> {
    format::wrap(text, inner.max(1))
        .into_iter()
        .map(|row| text_row(row, style))
        .collect()
}

#[cfg(test)]
#[path = "logout_view_tests.rs"]
mod tests;
