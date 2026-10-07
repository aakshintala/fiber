//! The attached session's offer of a repository's code (`docs/tui.md`,
//! "Approving what a repository ships"): what it holds, the keys and clicks
//! that choose each item's decision, the one `reply` that answers it, and
//! the rows the view draws.

use contract::commands::{Reply, ReplyAnswer};
use contract::events::{
    OfferDecision, OfferedItem, OfferedKind, RepositoryCodeOffered, RepositoryCodeResolved,
};
use contract::{Envelope, RequestId, SessionId};
use ratatui::text::{Line, Span};

use crate::app::{read, session_command};
use crate::keys::{Edit, Key};
use crate::markdown::{Role, style};

/// The line every offer shows.
pub(crate) const TUI_FILES: &str = "A package's TUI files are never installed from a repository.";

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
    /// the cursor, Enter moves down or sends on Send, PageUp and PageDown
    /// scroll. `None` while not open, and for Esc, F1, ⌥A and Ctrl+C,
    /// which are the caller's; every other key is swallowed.
    pub(crate) fn on_key(&mut self, key: &Key, width: u16, height: usize) -> Option<OfferKey> {
        if !self.open() {
            return None;
        }
        let total = self.total(width);
        let held = self.held.as_mut()?;
        let send = held.items.len();
        let page = height.saturating_sub(1).max(1);
        match key {
            Key::Esc | Key::F1 | Key::AltA | Key::CtrlC => return None,
            Key::Up => held.cursor = held.cursor.saturating_sub(1),
            Key::Down => held.cursor = held.cursor.saturating_add(1).min(send),
            Key::Enter if held.cursor == send => return Some(OfferKey::Send),
            Key::Enter => held.cursor = held.cursor.saturating_add(1),
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
            | Key::AltUp
            | Key::AltDown
            | Key::AltX => return Some(OfferKey::Handled),
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
        Some((layout(held, width).0, held.top))
    }

    /// Every wrapped row at `width`.
    fn total(&self, width: u16) -> usize {
        self.held.as_ref().map_or(0, |held| {
            layout(held, width)
                .0
                .iter()
                .map(|row| row.height(width))
                .sum()
        })
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
/// item's header, then the Send row.
fn layout(held: &Held, width: u16) -> (Vec<Row>, Vec<usize>) {
    let count = held.items.len();
    let noun = if count == 1 { "item" } else { "items" };
    let cross = width.saturating_sub(1);
    // The title gives way so the ✕ always draws in the last column.
    let mut title = String::new();
    for ch in format!("Repository code · {count} {noun}").chars() {
        title.push(ch);
        if crate::format::width(&title) > usize::from(cross) {
            title.pop();
            break;
        }
    }
    let pad = usize::from(cross).saturating_sub(crate::format::width(&title));
    let mut rows = vec![
        clipped(
            Line::raw(format!("{title}{}✕", " ".repeat(pad))),
            vec![(cross, width, Spot::Close)],
            width,
        ),
        Row::text(Line::raw(TUI_FILES)),
        Row::text(Line::raw("")),
    ];
    let mut cursor_rows = Vec::with_capacity(count.saturating_add(1));
    for (at, (item, decision)) in held.items.iter().zip(&held.decisions).enumerate() {
        cursor_rows.push(rows.len());
        rows.push(Row::text(Line::raw(format!(
            "{} {}",
            mark(held.cursor == at),
            header(item)
        ))));
        rows.push(choices(at, *decision, width));
        rows.extend(
            item.summary
                .lines()
                .map(|line| Row::text(Line::raw(format!("    {line}")))),
        );
        if let Some(diff) = &item.diff {
            rows.extend(diff_rows(diff));
        }
        rows.push(Row::text(Line::raw("")));
    }
    cursor_rows.push(rows.len());
    let tally = |wanted: OfferDecision| {
        held.decisions
            .iter()
            .filter(|decision| **decision == wanted)
            .count()
    };
    let send = format!(
        "{} Send · {} approve, {} skip, {} never",
        mark(held.cursor == count),
        tally(OfferDecision::Approve),
        tally(OfferDecision::Skip),
        tally(OfferDecision::Never)
    );
    let end = to_u16(crate::format::width(&send));
    rows.push(clipped(Line::raw(send), vec![(0, end, Spot::Send)], width));
    (rows, cursor_rows)
}

/// The cursor's mark, or a space.
fn mark(at: bool) -> char {
    if at { '›' } else { ' ' }
}

/// An item's header: its kind, name, version and whether it is required.
fn header(item: &OfferedItem) -> String {
    let kind = match item.kind {
        OfferedKind::Extension => "extension",
        OfferedKind::Hook => "hook",
        OfferedKind::McpServer => "MCP server",
    };
    let mut text = format!("{kind} {}", item.name);
    if let Some(version) = &item.version {
        text.push_str(&format!(" · {version}"));
    }
    if item.required {
        text.push_str(" · required");
    }
    text
}

/// An item's choice row: each decision a chip, the chosen one in brackets.
fn choices(item: usize, chosen: OfferDecision, width: u16) -> Row {
    let mut text = String::from("   ");
    let mut spots = Vec::new();
    for decision in DECISIONS {
        let word = match decision {
            OfferDecision::Approve => "approve",
            OfferDecision::Skip => "skip",
            OfferDecision::Never => "never",
        };
        let chip = if decision == chosen {
            format!("[{word}]")
        } else {
            format!(" {word} ")
        };
        text.push(' ');
        let start = to_u16(crate::format::width(&text));
        text.push_str(&chip);
        let end = to_u16(crate::format::width(&text));
        spots.push((start, end, Spot::Choice { item, decision }));
    }
    clipped(Line::raw(text), spots, width)
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

/// A diff's rows, each line in its diff role. `str::lines` drops a
/// trailing `\r`, so a CRLF diff draws as an LF one.
fn diff_rows(diff: &str) -> Vec<Row> {
    let text = diff.lines().collect::<Vec<_>>().join("\n");
    let runs = crate::highlight::spans("diff", &text).unwrap_or_default();
    runs.into_iter()
        .map(|line| {
            let mut spans = vec![Span::raw("    ")];
            spans.extend(
                line.into_iter()
                    .map(|(role, run): (Role, String)| Span::styled(run, style(role))),
            );
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
