//! The attached session's offer of a repository's code (`docs/tui.md`,
//! "Approving what a repository ships"): what it holds, the keys and clicks
//! that choose each item's decision, the one `reply` that answers it, and
//! the rows the view draws.

mod source;

use contract::commands::{Reply, ReplyAnswer};
use contract::events::{
    CommandAccepted, CommandResult, OfferDecision, OfferedItem, OfferedKind, RepositoryCodeOffered,
    RepositoryCodeResolved,
};
use contract::{Envelope, ErrorCode, RequestId, SessionId};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{read, session_command};
use crate::diff::{diff_lines, gutter_prefix, gutter_width, styled_spans};
use crate::keys::{Edit, Key};
use crate::markdown::{Role, style};
use crate::swapped::Ink;

/// The line every offer shows.
pub(crate) const TUI_FILES: &str = "A package's TUI files are never installed from a repository.";

/// The hint closing every offer.
pub(crate) const HINT: &str =
    "↑↓ choose an item · ←→ its answer · Send answers them all · Esc puts it aside";

/// The decisions in the order ← → step through them.
const DECISIONS: [OfferDecision; 3] = [
    OfferDecision::Approve,
    OfferDecision::Skip,
    OfferDecision::Never,
];

/// A click target in the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spot {
    /// One item's decision chip: sets it and moves the cursor there.
    Choice {
        item: usize,
        decision: OfferDecision,
    },
    /// The Send row.
    Send,
    /// The header's ✕: puts the offer aside.
    Close,
}

/// What a key or click did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OfferKey {
    /// Taken; nothing to send.
    Handled,
    /// Send the answer.
    Send,
    /// Open the cursor item's source.
    Source,
}

/// A source request from the offer's keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileAction {
    Open,
    Prev,
    Next,
}

/// One drawn row before wrapping, with its click targets as column
/// ranges. A row that carries targets is clipped at the view's width and
/// never wraps, so a target never covers cells it does not draw.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub(crate) line: Line<'static>,
    pub(crate) spots: Vec<(u16, u16, Spot)>,
    /// Clipped at the width rather than wrapped.
    pub(crate) clip: bool,
}

impl Row {
    /// A row that wraps and carries no target.
    fn text(line: Line<'static>) -> Self {
        Self {
            line,
            spots: Vec::new(),
            clip: false,
        }
    }

    /// The rows this row takes at `width`.
    pub(crate) fn height(&self, width: u16) -> usize {
        if self.clip {
            1
        } else {
            crate::view::rows(self.line.clone(), width)
        }
    }
}

/// Where the held offer stands. Put aside and answered at once cannot be
/// written.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// Shown; it takes the keys.
    Open,
    /// Put aside with Esc; it counts on the badge.
    Aside,
    /// A `reply` with command id `by` is in flight.
    Answered { by: String },
}

/// The offer held, with the person's choices.
#[derive(Debug)]
struct Held {
    request_id: RequestId,
    items: Vec<OfferedItem>,
    /// One per item, in the items' order.
    decisions: Vec<OfferDecision>,
    /// An item's index, or `items.len()` for the Send row.
    cursor: usize,
    /// The top wrapped row shown.
    top: usize,
    status: Status,
    source: Option<source::Source>,
}

/// The attached session's pending offer, if any.
#[derive(Debug, Default)]
pub(crate) struct Offer {
    held: Option<Held>,
}

impl Offer {
    /// Folds `repository_code_offered` and `repository_code_resolved`. An
    /// offer with the held id changes nothing, so a re-raised offer keeps
    /// the person's choices; one with another id replaces it. A resolution
    /// of the held id, whoever answered, drops it.
    pub(crate) fn fold(&mut self, envelope: &Envelope) {
        match envelope.kind.as_str() {
            "repository_code_offered" => {
                let Some(offered) = read!(envelope, RepositoryCodeOffered) else {
                    return;
                };
                if self.held_id() == Some(&offered.request_id) {
                    return;
                }
                self.held = Some(Held {
                    request_id: offered.request_id,
                    decisions: vec![OfferDecision::Skip; offered.items.len()],
                    items: offered.items,
                    cursor: 0,
                    top: 0,
                    status: Status::Open,
                    source: None,
                });
            }
            "repository_code_resolved" => {
                if let Some(resolved) = read!(envelope, RepositoryCodeResolved)
                    && self.held_id() == Some(&resolved.request_id)
                {
                    self.held = None;
                }
            }
            _ => {}
        }
    }

    fn held_id(&self) -> Option<&RequestId> {
        self.held.as_ref().map(|held| &held.request_id)
    }

    /// Held and open: the view shows and takes keys.
    pub(crate) fn open(&self) -> bool {
        self.held
            .as_ref()
            .is_some_and(|held| held.status == Status::Open)
    }

    /// Held and put aside: it counts on the badge.
    pub(crate) fn aside(&self) -> bool {
        self.held
            .as_ref()
            .is_some_and(|held| held.status == Status::Aside)
    }

    /// A key while open, the view `width` wide and `height` tall: ↑ ↓ move
    /// the cursor, Enter opens source or sends on Send, PageUp and PageDown
    /// scroll. `None` while not open, and for Esc, F1, ⌥A and Ctrl+C,
    /// which are the caller's; every other key is swallowed.
    pub(crate) fn on_key(&mut self, key: &Key, width: u16, height: usize) -> Option<OfferKey> {
        if !self.open() {
            return None;
        }
        let total = self.total(width);
        let held = self.held.as_mut()?;
        if let Some(source) = held.source.as_mut() {
            if matches!(key, Key::Esc | Key::F1 | Key::AltA | Key::CtrlC) {
                return None;
            }
            source.on_key(key, total, height);
            return Some(OfferKey::Handled);
        }
        let send = held.items.len();
        let page = height.saturating_sub(1).max(1);
        match key {
            Key::Esc | Key::F1 | Key::AltA | Key::CtrlC => return None,
            Key::Up => held.cursor = held.cursor.saturating_sub(1),
            Key::Down => held.cursor = held.cursor.saturating_add(1).min(send),
            Key::Enter if held.cursor == send => return Some(OfferKey::Send),
            Key::Enter => return Some(OfferKey::Source),
            Key::PageUp => {
                held.top = held.top.saturating_sub(page);
                return Some(OfferKey::Handled);
            }
            Key::PageDown => {
                let last = total.saturating_sub(height);
                held.top = held.top.saturating_add(page).min(last);
                return Some(OfferKey::Handled);
            }
            Key::Char(_)
            | Key::Backspace
            | Key::CtrlO
            | Key::End
            | Key::Tab
            | Key::BackTab
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlL
            | Key::CtrlF
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => return Some(OfferKey::Handled),
        }
        self.keep_in_view(width, height);
        Some(OfferKey::Handled)
    }

    /// An edit while open: ← → step the cursor item's decision through
    /// approve, skip and never, clamped; every other edit is swallowed.
    /// `false` while not open.
    pub(crate) fn on_edit(&mut self, edit: &Edit) -> bool {
        if !self.open() {
            return false;
        }
        let Some(held) = self.held.as_mut() else {
            return false;
        };
        if held.source.is_some() {
            return true;
        }
        let step = |decision: OfferDecision, right: bool| {
            let at = DECISIONS
                .iter()
                .position(|each| *each == decision)
                .unwrap_or_default();
            let next = if right {
                at.saturating_add(1)
            } else {
                at.saturating_sub(1)
            };
            DECISIONS.get(next).copied().unwrap_or(decision)
        };
        match edit {
            Edit::Left | Edit::Right => {
                if let Some(decision) = held.decisions.get_mut(held.cursor) {
                    *decision = step(*decision, *edit == Edit::Right);
                }
            }
            Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::DeleteToLineStart
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete
            | Edit::Paste(_) => {}
        }
        true
    }

    /// A click on `spot` while open: a chip sets its item's decision and
    /// moves the cursor there, Send sends, ✕ puts the offer aside.
    pub(crate) fn click(&mut self, spot: Spot) -> OfferKey {
        if !self.open() {
            return OfferKey::Handled;
        }
        match spot {
            Spot::Choice { item, decision } => {
                if let Some(held) = self.held.as_mut()
                    && let Some(chosen) = held.decisions.get_mut(item)
                {
                    *chosen = decision;
                    held.cursor = item;
                }
                OfferKey::Handled
            }
            Spot::Send => OfferKey::Send,
            Spot::Close => {
                if self.close_source() {
                    return OfferKey::Handled;
                }
                self.put_aside();
                OfferKey::Handled
            }
        }
    }

    /// An open offer goes aside, behind the badge.
    pub(crate) fn put_aside(&mut self) {
        if let Some(held) = self.held.as_mut()
            && held.status == Status::Open
        {
            held.source = None;
            held.status = Status::Aside;
        }
    }

    /// A put-aside offer opens again. `false` for any other status or no
    /// offer.
    pub(crate) fn reopen(&mut self) -> bool {
        match self.held.as_mut() {
            Some(held) if held.status == Status::Aside => {
                held.status = Status::Open;
                true
            }
            Some(_) | None => false,
        }
    }

    /// The `reply` line answering the open offer with command `id`, one
    /// decision per item in the offer's order; the offer closes until the
    /// answer settles. `None` unless open.
    pub(crate) fn answer(&mut self, id: &str, session: &SessionId) -> Option<String> {
        if !self.open() {
            return None;
        }
        let held = self.held.as_mut()?;
        let reply = Reply {
            request_id: held.request_id.clone(),
            answer: ReplyAnswer::Decisions {
                decisions: held.decisions.clone(),
            },
        };
        let args = serde_json::to_value(reply).ok();
        held.source = None;
        held.status = Status::Answered { by: id.to_owned() };
        Some(session_command(id, "reply", session, args).to_string())
    }

    /// The reply `id` failed: the offer it answered opens again with the
    /// person's choices. Any other id or status changes nothing.
    pub(crate) fn restore(&mut self, id: &str) {
        if let Some(held) = self.held.as_mut()
            && matches!(&held.status, Status::Answered { by } if by == id)
        {
            held.status = Status::Open;
        }
    }

    /// The rows at `width` and the top wrapped row, while open.
    pub(crate) fn rows(&self, width: u16) -> Option<(Vec<Row>, usize)> {
        if !self.open() {
            return None;
        }
        let held = self.held.as_ref()?;
        if let Some(source) = &held.source {
            let item = held.items.get(source.item)?;
            let mut rows = vec![title_row(width)];
            rows.extend(source.rows(item, width));
            return Some((rows, source.top()));
        }
        Some((layout(held, width).0, held.top))
    }

    /// Every wrapped row at `width`.
    fn total(&self, width: u16) -> usize {
        self.rows(width).map_or(0, |(rows, _)| {
            rows.iter().map(|row| row.height(width)).sum()
        })
    }

    pub(crate) fn source_open(&self) -> bool {
        self.held.as_ref().is_some_and(|held| held.source.is_some())
    }

    pub(crate) fn close_source(&mut self) -> bool {
        self.held
            .as_mut()
            .is_some_and(|held| held.source.take().is_some())
    }

    pub(crate) fn ask_file(&mut self, id: &str, session: &SessionId) -> Option<String> {
        if !self.open() {
            return None;
        }
        let held = self.held.as_mut()?;
        let path = held.items.get(held.cursor)?.files.first()?;
        let mut source = source::Source::new(held.cursor);
        let line = source.ask(id, session, &held.request_id, path);
        held.source = Some(source);
        Some(line)
    }

    /// Opens the cursor item's first file or steps the open source view.
    pub(crate) fn step_file(
        &mut self,
        action: FileAction,
        id: &str,
        session: &SessionId,
    ) -> Option<String> {
        let right = match action {
            FileAction::Open => return self.ask_file(id, session),
            FileAction::Prev => false,
            FileAction::Next => true,
        };
        let held = self.held.as_mut()?;
        let source = held.source.as_mut()?;
        let item = held.items.get(source.item)?;
        let next = source.step(right, item.files.len())?;
        Some(source.ask(id, session, &held.request_id, item.files.get(next)?))
    }

    pub(crate) fn file_answered(&mut self, accepted: &CommandAccepted) -> bool {
        let Some(source) = self.held.as_mut().and_then(|held| held.source.as_mut()) else {
            return false;
        };
        if let Some(CommandResult::OfferedFile { text }) = &accepted.result {
            return source.answered(&accepted.command_id.0, text.clone());
        }
        false
    }

    pub(crate) fn file_refused(&mut self, id: &str, code: &ErrorCode, message: &str) -> bool {
        self.held
            .as_mut()
            .and_then(|held| held.source.as_mut())
            .is_some_and(|source| source.refused(id, code, message))
    }

    /// Scrolls so the cursor's row shows in `height` rows.
    fn keep_in_view(&mut self, width: u16, height: usize) {
        let Some(held) = self.held.as_mut() else {
            return;
        };
        let (rows, cursor_rows) = layout(held, width);
        let Some(at) = cursor_rows.get(held.cursor) else {
            return;
        };
        let row: usize = rows.iter().take(*at).map(|row| row.height(width)).sum();
        if row < held.top {
            held.top = row;
        } else if row >= held.top.saturating_add(height) {
            held.top = row.saturating_add(1).saturating_sub(height);
        }
    }
}

/// The rows at `width`, and the row index of each cursor position: each
/// item's header, then the Send row (`docs/tui.md`, "Approving what a
/// repository ships").
fn layout(held: &Held, width: u16) -> (Vec<Row>, Vec<usize>) {
    let mut rows = vec![title_row(width), Row::text(Line::raw(""))];
    let mut cursor_rows = Vec::with_capacity(held.items.len().saturating_add(1));
    for (at, (item, decision)) in held.items.iter().zip(&held.decisions).enumerate() {
        cursor_rows.push(rows.len());
        rows.push(Row::text(header_line(item, held.cursor == at)));
        rows.extend(
            item.summary
                .lines()
                .map(|line| dim_row(&format!("  {line}"))),
        );
        if let Some(diff) = &item.diff {
            rows.push(dim_row("  the declaration changed"));
            rows.extend(diff_rows(diff));
        }
        rows.push(choices(at, *decision, width));
        rows.push(Row::text(Line::raw("")));
    }
    rows.push(dim_row(TUI_FILES));
    rows.push(Row::text(Line::raw("")));
    rows.push(dim_row(HINT));
    cursor_rows.push(rows.len());
    let tally = |wanted: OfferDecision| {
        held.decisions
            .iter()
            .filter(|decision| **decision == wanted)
            .count()
    };
    let at_send = held.cursor == held.items.len();
    let send = format!(
        " Send · {} approve, {} skip, {} never",
        tally(OfferDecision::Approve),
        tally(OfferDecision::Skip),
        tally(OfferDecision::Never)
    );
    // The mark and the tally share one ink, so the cursor paints the
    // whole row and the rest draws plain.
    let ink = if at_send {
        style(Role::Secondary)
    } else {
        Style::new()
    };
    let end = to_u16(crate::format::width(&send).saturating_add(1));
    let line = Line::from(vec![
        Span::styled(mark(at_send).to_string(), ink),
        Span::styled(send, ink),
    ]);
    rows.push(clipped(line, vec![(0, end, Spot::Send)], width));
    (rows, cursor_rows)
}

/// The cursor's mark.
fn mark(at: bool) -> char {
    if at { '▸' } else { ' ' }
}

/// Bold on the default colour, as the top row's name and a chosen
/// answer's label draw.
fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// One dim row of wrapped text.
fn dim_row(text: &str) -> Row {
    Row::text(Line::from(Span::styled(
        text.to_owned(),
        style(Role::Muted),
    )))
}

/// The top row's layout at `width`: the name, the command when the
/// name, the command and `Esc returns` all fit, Esc when it fits
/// beside the name, and the trailing padding. `" " + name` always
/// draws, truncated at its cells when narrower than the row; nothing
/// else truncates, so the command gives way whole before Esc does
/// (`docs/tui.md`, "Swapped views").
pub(crate) struct TitleLayout {
    /// `" " + name`, cut to `width` when narrower.
    pub(crate) name: String,
    /// `"  " + command`, when it fits beside the name and Esc.
    pub(crate) command: Option<String>,
    /// Whether `Esc returns` ends the row.
    pub(crate) esc: bool,
    /// The trailing padding after the last part.
    pub(crate) pad: usize,
}

/// The top row's parts for `name` and `command` at `width` columns.
pub(crate) fn title_layout(name: &str, command: &str, width: usize) -> TitleLayout {
    const ESC: &str = "Esc returns";
    let name_full = format!(" {name}");
    let command_full = format!("  {command}");
    let (name_cells, command_cells, esc_cells) = (
        crate::format::width(&name_full),
        crate::format::width(&command_full),
        crate::format::width(ESC),
    );
    if name_cells + command_cells + 1 + esc_cells <= width {
        TitleLayout {
            name: name_full,
            command: Some(command_full),
            esc: true,
            pad: width - name_cells - command_cells - esc_cells,
        }
    } else if name_cells + 1 + esc_cells <= width {
        TitleLayout {
            name: name_full,
            command: None,
            esc: true,
            pad: width - name_cells - esc_cells,
        }
    } else {
        let name = crate::format::cut(&name_full, width);
        let shown = crate::format::width(&name);
        TitleLayout {
            name,
            command: None,
            esc: false,
            pad: width.saturating_sub(shown),
        }
    }
}

/// The swapped view's top row: `Offer` bold, `repository code` dim and
/// `Esc returns` dim right-aligned, on the `surface` tint across the full
/// width including padding (`docs/tui.md`, "Swapped views"). A click
/// anywhere on it puts the offer aside: a click on the top row returns.
/// The command gives way first, then Esc, so `Offer` always draws.
fn title_row(width: u16) -> Row {
    const ESC: &str = "Esc returns";
    let tint = Style::new().bg(Role::Surface.color());
    let fit = usize::from(width);
    let layout = title_layout("Offer", "repository code", fit);
    let mut spans = Vec::new();
    if let Some(rest) = layout.name.strip_prefix(' ') {
        spans.push(Span::styled(" ".to_owned(), tint));
        if !rest.is_empty() {
            spans.push(Span::styled(
                rest.to_owned(),
                bold().bg(Role::Surface.color()),
            ));
        }
    }
    if let Some(command) = layout.command {
        spans.push(Span::styled(
            command,
            style(Role::Muted).bg(Role::Surface.color()),
        ));
    }
    // Esc needs a space before it, or it stays off.
    if layout.pad > 0 {
        spans.push(Span::styled(" ".repeat(layout.pad), tint));
    }
    if layout.esc {
        spans.push(Span::styled(
            ESC,
            style(Role::Muted).bg(Role::Surface.color()),
        ));
    }
    let mut line = Line::from(spans);
    line.style = tint;
    clipped(line, vec![(0, width, Spot::Close)], width)
}

/// An item's header row: `▸ ` in secondary and the row in secondary on
/// the cursor item, naming its kind, name and what identifies it.
fn header_line(item: &OfferedItem, cursor: bool) -> Line<'static> {
    let ink = if cursor {
        style(Role::Secondary)
    } else {
        Style::new()
    };
    Line::from(vec![
        Span::styled(if cursor { "▸ " } else { "  " }, ink),
        Span::styled(header(item), ink),
    ])
}

/// An item's header: its kind, name and what identifies it: an
/// extension's version, otherwise the path its declaration comes from.
fn header(item: &OfferedItem) -> String {
    let mut text = format!("{} {}", kind_label(item.kind), item.name);
    if let Some(detail) = detail(item) {
        text.push_str(&format!(" · {detail}"));
    }
    text
}

/// The name of a repository item's kind in both offer views.
fn kind_label(kind: OfferedKind) -> &'static str {
    match kind {
        OfferedKind::Extension => "Extension",
        OfferedKind::Hook => "Hook",
        OfferedKind::McpServer => "MCP server",
    }
}

/// What identifies an item after its kind and name: an extension's
/// version, otherwise the declaration path the summary carries. `None`
/// when the item carries neither.
fn detail(item: &OfferedItem) -> Option<String> {
    if let Some(version) = &item.version {
        return Some(version.clone());
    }
    item.summary.lines().find_map(|line| {
        line.strip_prefix("declared in:")
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(|path| format!("declared in {path}"))
    })
}

/// An item's choice row: every decision with its `( )` mark, the chosen
/// one `(•)` in secondary with its label bold and the other marks dim.
/// Each decision keeps its click target.
fn choices(item: usize, chosen: OfferDecision, width: u16) -> Row {
    const LEAD: &str = "  ";
    const GAP: &str = "   ";
    let label = |decision: OfferDecision| match decision {
        OfferDecision::Approve => "approve",
        OfferDecision::Skip => "skip for this session",
        OfferDecision::Never => "never",
    };
    let mut spans = vec![Span::raw(LEAD)];
    let mut spots = Vec::new();
    let mut col = crate::format::width(LEAD);
    for (at, decision) in DECISIONS.into_iter().enumerate() {
        if at > 0 {
            spans.push(Span::raw(GAP));
            col += crate::format::width(GAP);
        }
        let on = decision == chosen;
        let mark_text = if on { "(•) " } else { "( ) " };
        let word = label(decision);
        let start = to_u16(col);
        col += crate::format::width(mark_text) + crate::format::width(word);
        spans.push(Span::styled(
            mark_text,
            if on {
                style(Role::Secondary)
            } else {
                style(Role::Muted)
            },
        ));
        spans.push(Span::styled(
            word,
            if on {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            },
        ));
        spots.push((start, to_u16(col), Spot::Choice { item, decision }));
    }
    clipped(Line::from(spans), spots, width)
}

/// A row clipped at `width`: a target starting at or past it is dropped,
/// and one crossing it ends there.
fn clipped(line: Line<'static>, spots: Vec<(u16, u16, Spot)>, width: u16) -> Row {
    let spots = spots
        .into_iter()
        .filter(|(start, _, _)| *start < width)
        .map(|(start, end, spot)| (start, end.min(width), spot))
        .collect();
    Row {
        line,
        spots,
        clip: true,
    }
}

/// A changed item's diff through the shared renderer, each row
/// indented four columns: the dim gutter, `@@` lines dim, `-` lines
/// in `removed` and `+` lines in `added`, the words that differ in
/// reverse video when one removed line pairs with one added line, as
/// "Changed files" draws a hunk (`docs/tui.md`, "Swapped views").
fn diff_rows(diff: &str) -> Vec<Row> {
    let drawn = diff_lines(diff);
    let gutter = gutter_width(drawn.iter().filter_map(|line| line.number));
    drawn
        .into_iter()
        .map(|line| {
            let mut spans = vec![Span::raw("    ")];
            if let Some(prefix) = gutter_prefix(line.number, gutter) {
                spans.push(Span::styled(prefix, Ink::Muted.style()));
            }
            spans.extend(styled_spans(&line));
            Row::text(Line::from(spans))
        })
        .collect()
}

/// A column count as a screen coordinate.
fn to_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
#[path = "offer_tests.rs"]
mod tests;
