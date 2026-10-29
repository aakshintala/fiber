//! The rows a card draws, each with how its text joins the row before
//! (`docs/tui.md`, "Selection and copy": a selection copies the text
//! unwrapped). Producers push into [`Rows`] as they would into a
//! `Vec<Row>`; a plain push is a row that starts a line of its own.

use std::collections::HashMap;
use std::ops::{Deref, Range};
use std::sync::Arc;

use contract::SessionId;

use contract::Seq;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::Target;
use crate::ext::spans::FittedLine;
use crate::ext::{Bound, Input, RawSpan};
use crate::surface::{Edges, edge_row};
use crate::theme::Role;
use crate::turn::Row;

/// How a row's text follows the row before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Join {
    /// A new logical line.
    Break,
    /// The same line, with nothing between: a word broken at the edge.
    Wrap,
    /// The same line, with the one space the wrap dropped between.
    WrapSpace,
}

/// What a drawn row adds to its logical line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowText {
    /// How it joins the row before.
    pub(crate) join: Join,
    /// The leading cells that are decoration, not text: quote bars, a hang
    /// indent, a code gutter, the bubble's pad.
    pub(crate) skip: u16,
    /// The trailing cells that are decoration, not text: the bubble's
    /// right pad and stripe.
    pub(crate) tail: u16,
    /// The whole row is decoration and adds no text: a code block's header.
    pub(crate) decoration: bool,
    /// The links drawn on the row: each one's cells in the line and its
    /// destination (`docs/tui.md`, "Links": links are handled on click).
    pub(crate) links: Vec<(Range<u16>, String)>,
    /// The collapsible sections the row is inside, outermost first: what
    /// a search draw opens, and what a match names (`docs/tui.md`,
    /// "Search").
    pub(crate) scopes: Vec<Target>,
    /// The column of the one cell that spins while its line runs, on the
    /// working line's tick (`docs/tui.md`, "The working line"); none in
    /// [`RowText::plain`].
    pub(crate) spin: Option<u16>,
    /// The animated item that drew the row, if any: what the paint pass
    /// marks as visible (`docs/tui.md`, "Animation").
    pub(crate) anim: Option<crate::ext::anim::AnimKey>,
}

impl RowText {
    /// A row that starts a line of its own, all of it text.
    pub(crate) fn plain() -> Self {
        Self {
            join: Join::Break,
            skip: 0,
            tail: 0,
            decoration: false,
            links: Vec::new(),
            scopes: Vec::new(),
            spin: None,
            // A still row draws no animated item until an animated
            // replacement tags it.
            anim: None,
        }
    }
}

/// The rows drawn so far and, one each, their texts.
#[derive(Debug, Default)]
pub(crate) struct Rows {
    rows: Vec<Row>,
    texts: Vec<RowText>,
    /// One per row: the `seq` of the durable line whose piece drew it,
    /// `None` for decoration and rows with no source. A wrapped row's
    /// pieces keep its source; edges and gaps take none, so they belong
    /// to the piece above.
    sources: Vec<Option<Seq>>,
    /// The source rows pushed from now on take.
    source: Option<Seq>,
    /// The collapsible sections the next row lands inside, outermost
    /// first.
    scopes: Vec<Target>,
    /// A search draw: every collapsible body draws, open or not
    /// (`docs/tui.md`, "Search": every match is counted at once).
    all_open: bool,
    /// The renderer seam bound to the page these rows draw on, if any:
    /// the ledger asks it for each call's lines.
    pub(crate) seam: Option<Bound>,
    /// Current actual session names used by message entries.
    pub(crate) session_names: Arc<HashMap<SessionId, String>>,
    /// Count-only fold: built-in Markdown replies may insert empty row placeholders.
    count_only: bool,
}

impl Rows {
    /// Rows for a count-only fold.
    pub(crate) fn count_only() -> Self {
        Self {
            count_only: true,
            ..Self::default()
        }
    }

    /// Whether this is the count-only fold path.
    pub(crate) fn is_count_only(&self) -> bool {
        self.count_only
    }

    /// Inserts one empty placeholder per counted Markdown row.
    pub(crate) fn push_counted(&mut self, count: usize, targets: &[(usize, Target)]) {
        for at in 0..count {
            let target = targets
                .iter()
                .find_map(|(line, target)| (*line == at).then_some(*target));
            self.push((Line::default(), target));
        }
    }

    /// Rows for a search draw: every collapsible body draws.
    pub(crate) fn all_open() -> Self {
        Self {
            all_open: true,
            ..Self::default()
        }
    }

    /// Binds the renderer seam these rows draw through.
    pub(crate) fn with_seam(mut self, seam: Bound) -> Self {
        self.seam = Some(seam);
        self
    }

    /// Selects before lookup and forwards notices even without a drawing winner.
    pub(crate) fn fills_for(&self, slot: &str) -> crate::ext::SlotSelection {
        let Some(seam) = &self.seam else {
            return crate::ext::SlotSelection::Absent;
        };
        let Some(renderer) = seam.handle.get() else {
            return crate::ext::SlotSelection::Absent;
        };
        let selected = renderer.fills_for(&seam.ctx, slot);
        for notice in renderer.take_pending() {
            seam.cache.queue_notice(notice);
        }
        selected
    }

    /// The bound renderer's lines for `slot`: none without a seam,
    /// without an installed renderer, or when the item falls back to
    /// its built-in row. A miss calls the renderer once and validates
    /// what it returns before storing or drawing any of it.
    pub(crate) fn ext_lines(
        &self,
        slot: &str,
        extension: &str,
        input: &Input<'_>,
        width: u16,
        builtin: &dyn Fn(&Input<'_>, u16) -> Vec<Vec<RawSpan>>,
    ) -> Option<Arc<Vec<FittedLine>>> {
        let seam = self.seam.as_ref()?;
        let renderer = seam.handle.get()?;
        let result = match renderer.animate_interval(&seam.ctx, slot) {
            // An animated slot draws through the animation schedule:
            // open paint advances; a closed draw caches a miss in the
            // ordinary entries without changing animation state.
            Some(interval) => seam.cache.lines_animated(
                seam.page.clone(),
                slot,
                extension,
                renderer.half(&seam.ctx, extension),
                input,
                width,
                interval,
                |frame| renderer.render_animated(&seam.ctx, slot, input, width, frame, builtin),
            ),
            None => seam.cache.lines(
                seam.page.clone(),
                slot,
                extension,
                renderer.half(&seam.ctx, extension),
                input,
                width,
                || renderer.render(&seam.ctx, slot, input, width, builtin),
            ),
        };
        for notice in renderer.take_pending() {
            seam.cache.queue_notice(notice);
        }
        result
    }

    /// Enters what `target` opens: a collapsible body draws when `open`
    /// on a normal draw, and always on a search draw. Every row pushed
    /// until [`Rows::end_scope`] names `target` among its scopes,
    /// outermost first. A body with no target draws as it would closed:
    /// nothing could reveal a match there.
    pub(crate) fn open_scope(&mut self, target: Target, open: bool) -> bool {
        self.scopes.push(target);
        open || self.all_open
    }

    /// Leaves the innermost open section; nothing without one.
    pub(crate) fn end_scope(&mut self) {
        self.scopes.pop();
    }

    /// Adds a row that starts a line of its own.
    pub(crate) fn push(&mut self, row: Row) {
        self.push_text(row, RowText::plain());
    }

    /// Adds a fitted replacement line as its own row: the line's cells
    /// with its links, the pad count in the row text's tail, and
    /// `target`. The pad is decoration, never text. The line's animated
    /// tag rides along, so the paint pass marks the item visible.
    pub(crate) fn push_fitted(&mut self, line: &FittedLine, target: Option<Target>) {
        self.push_text(
            (line.line.clone(), target),
            RowText {
                tail: line.pad,
                links: line.links.clone(),
                anim: line.anim.clone(),
                ..RowText::plain()
            },
        );
    }

    /// Adds a row with what it adds to its logical line.
    pub(crate) fn push_text(&mut self, row: Row, mut text: RowText) {
        text.scopes = self.scopes.clone();
        self.rows.push(row);
        self.texts.push(text);
        self.sources.push(self.source);
    }

    /// Sets the source the rows pushed from now on take: the `seq` of
    /// the line that began the piece they draw, or none.
    pub(crate) fn set_source(&mut self, source: Option<Seq>) {
        self.source = source;
    }

    /// The rows, their texts and their sources, one each.
    pub(crate) fn into_sourced_parts(mut self) -> (Vec<Row>, Vec<RowText>, Vec<Option<Seq>>) {
        self.sources.resize(self.rows.len(), None);
        (self.rows, self.texts, self.sources)
    }

    /// The rows and their texts, one each.
    pub(crate) fn into_parts(self) -> (Vec<Row>, Vec<RowText>) {
        (self.rows, self.texts)
    }

    /// Takes the row at `at`, leaving blank placeholders: the pad
    /// loop moves each row out once instead of cloning it per row.
    /// None when either side has no row there.
    fn take_row(&mut self, at: usize) -> Option<((Line<'static>, Option<Target>), RowText)> {
        let row = self.rows.get_mut(at)?;
        let text = self.texts.get_mut(at)?;
        let line = std::mem::take(&mut row.0);
        let target = row.1.take();
        let meta = std::mem::replace(text, RowText::plain());
        Some(((line, target), meta))
    }

    /// Prefixes every row from `from` with `pad` blank cells, splitting
    /// any row wider than `inner` first (`docs/tui.md`, "Turns":
    /// nothing inside the card is indented past its one-column padding).
    /// A split breaks after the last space in its window, dropping that
    /// space, or cuts hard with no space there; no glyph is cut, and a
    /// glyph wider than `inner` draws as one blank cell. Each piece keeps
    /// its spans' styles and the line's style; the row's target stays on
    /// the first piece, while `skip`, `tail`, `links` and `spin` are
    /// redistributed by original cells and then shifted by the pad. The
    /// blank prefix takes the line's background, so call this before
    /// [`Rows::on_surface`], whose edge rows are never padded.
    pub(crate) fn pad(&mut self, from: usize, pad: u16, inner: u16) {
        let mut at = from;
        let original_len = self.rows.len();
        for _ in from..original_len {
            // The row moves out without cloning: fitting rows move back
            // below, split rows move into their pieces.
            let Some(((line, target), text)) = self.take_row(at) else {
                break;
            };
            let (_, total) = row_widths(line.spans.iter().map(|span| span.content.as_ref()));
            let room = usize::from(inner);
            if total <= room || room == 0 {
                let mut row = (line, target);
                let mut row_text = text;
                prefix_row(&mut row.0, &mut row_text, pad);
                if let (Some(slot), Some(meta)) = (self.rows.get_mut(at), self.texts.get_mut(at)) {
                    *slot = row;
                    *meta = row_text;
                }
                at = at.saturating_add(1);
                continue;
            }
            let cells = cells_of(&line);
            let off = offsets(&cells);
            let plans = plan(&cells, room);
            debug_assert!(!plans.is_empty());
            let mut pieces = Vec::with_capacity(plans.len());
            for (index, planned) in plans.iter().enumerate() {
                let (start, end) = (planned.start, planned.end);
                let start_cell = off.get(start).copied().unwrap_or(total);
                let end_cell = off.get(end).copied().unwrap_or(total);
                let piece = build_piece(&line, &cells, planned, inner);
                let mut piece_text = RowText {
                    join: if index == 0 { text.join } else { planned.join },
                    skip: text
                        .skip
                        .min(u16::try_from(end_cell).unwrap_or(u16::MAX))
                        .saturating_sub(u16::try_from(start_cell).unwrap_or(u16::MAX)),
                    tail: tail_in(&text, total, start_cell, end_cell),
                    decoration: text.decoration,
                    links: text
                        .links
                        .iter()
                        .filter_map(|(range, to)| {
                            let start = u16::try_from(start_cell).unwrap_or(u16::MAX);
                            let end = u16::try_from(end_cell).unwrap_or(u16::MAX);
                            let from = range.start.max(start);
                            let to_cell = range.end.min(end);
                            (from < to_cell).then(|| (from - start..to_cell - start, to.clone()))
                        })
                        .collect(),
                    scopes: text.scopes.clone(),
                    spin: text
                        .spin
                        .filter(|spin| {
                            let spin = usize::from(*spin);
                            spin >= start_cell && spin < end_cell
                        })
                        .map(|spin| {
                            spin.saturating_sub(u16::try_from(start_cell).unwrap_or(u16::MAX))
                        }),
                    // Every piece paints the same item, so each keeps
                    // its tag: a split row still marks its item visible.
                    anim: text.anim.clone(),
                };
                let target = (index == 0).then_some(target).flatten();
                let mut row = (piece, target);
                prefix_row(&mut row.0, &mut piece_text, pad);
                pieces.push((row, piece_text));
            }
            let end = at + 1;
            let mut grown_rows = Vec::with_capacity(pieces.len());
            let mut grown_texts = Vec::with_capacity(pieces.len());
            for ((line, target), text) in pieces {
                grown_rows.push((line, target));
                grown_texts.push(text);
            }
            let grown = grown_rows.len();
            self.rows.splice(at..end, grown_rows);
            self.texts.splice(at..end, grown_texts);
            if let Some(source) = self.sources.get(at).copied() {
                self.sources
                    .splice(at..end, std::iter::repeat_n(source, grown));
            }
            at = at.saturating_add(grown);
        }
    }

    /// Inserts one plain empty row at `at`, with the scopes current at
    /// the call (`docs/tui.md`, "Turns": blocks inside the card are
    /// separated by a blank row).
    pub(crate) fn gap(&mut self, at: usize) {
        let at = at.min(self.rows.len());
        if at <= self.sources.len() {
            self.sources.insert(at, None);
        }
        self.rows.insert(at, (Line::default(), None));
        self.texts.insert(
            at,
            RowText {
                scopes: self.scopes.clone(),
                ..RowText::plain()
            },
        );
    }

    /// The rows from `from` on sit on `tint`: each takes it as its line's
    /// background; with `edges`, an `edge_row` (`width` cells,
    /// decoration) is inserted at `from` and pushed after the last. No
    /// rows from `from`: nothing (`docs/tui.md`, "Look").
    pub(crate) fn on_surface(&mut self, from: usize, width: u16, tint: Role, edges: Edges) {
        let Some(body) = self.rows.get_mut(from..) else {
            return;
        };
        if body.is_empty() {
            return;
        }
        for (line, _) in body {
            line.style.bg = Some(tint.color());
        }
        let width = usize::from(width);
        if edges.bottom {
            self.rows.push((edge_row(width, tint, false), None));
            self.sources.push(None);
            self.texts.push(RowText {
                decoration: true,
                scopes: self.scopes.clone(),
                ..RowText::plain()
            });
        }
        if edges.top {
            if from <= self.sources.len() {
                self.sources.insert(from, None);
            }
            self.rows.insert(from, (edge_row(width, tint, true), None));
            self.texts.insert(
                from,
                RowText {
                    decoration: true,
                    scopes: self.scopes.clone(),
                    ..RowText::plain()
                },
            );
        }
    }
}

/// The draw-equivalent span width and the exact grapheme width `Rows::pad`
/// uses for `parts`, kept separate because styled span boundaries matter.
pub(crate) fn row_widths<'a>(parts: impl IntoIterator<Item = &'a str>) -> (usize, usize) {
    parts
        .into_iter()
        .fold((0usize, 0usize), |(spans, pad), part| {
            if part.is_ascii() && !part.bytes().any(|byte| byte.is_ascii_control()) {
                return (
                    spans.saturating_add(part.len()),
                    pad.saturating_add(part.len()),
                );
            }
            let span = Span::raw(part);
            let pad = span
                .styled_graphemes(Style::default())
                .fold(pad, |total, grapheme| {
                    total.saturating_add(crate::format::width(grapheme.symbol))
                });
            (spans.saturating_add(span.width()), pad)
        })
}

/// How many pieces [`Rows::pad`] splits `line` into at `inner` (1 when
/// it fits): the one split rule both the draw and the row counts use
/// (`docs/tui.md`, "Turns").
pub(crate) fn fitted(line: &Line<'_>, inner: u16) -> usize {
    fitted_count(line, usize::from(inner)).max(1)
}

/// How many pieces [`plan`] cuts `line` into at `inner`: the same split
/// rule over the same glyphs [`cells_of`] flattens, counted in one pass
/// without building the cell vector. Only the open piece's used width,
/// its last space and the widths after that space are kept, so the count
/// path allocates nothing per glyph. A piece holds at most `inner`
/// widths, breaking after its last space (dropped) or cutting hard, and
/// a glyph wider than `inner` stands alone. Zero when the line is empty
/// or `inner` is 0; the caller maps that to one piece.
fn fitted_count(line: &Line<'_>, inner: usize) -> usize {
    if inner == 0 {
        return 0;
    }
    let mut pieces: usize = 0;
    // First cell of the open piece, its used widths, its last space,
    // the widths after that space, and the cells seen so far.
    let mut at: usize = 0;
    let mut used: usize = 0;
    let mut space: Option<usize> = None;
    let mut after: usize = 0;
    let mut cells: usize = 0;
    for span in &line.spans {
        // Ratatui excludes `\n` here because a line break is not a drawn glyph.
        for grapheme in span.styled_graphemes(Style::default()) {
            let here = cells;
            cells = cells.saturating_add(1);
            let width = crate::format::width(grapheme.symbol);
            if used.saturating_add(width) <= inner {
                used = used.saturating_add(width);
                if grapheme.symbol == " " {
                    space = Some(here);
                    after = 0;
                } else {
                    // Before the first space this is garbage the next
                    // space wipes; a split only reads `after` past one.
                    after = after.saturating_add(width);
                }
                continue;
            }
            // The window from `at` is full: this glyph starts a new piece.
            match space {
                Some(found) if found > at => {
                    pieces = pieces.saturating_add(1);
                    // The next piece restarts after the dropped space over
                    // the glyphs this window already scanned, whose widths
                    // `after` totals (within `inner`, so they fit).
                    used = after;
                    space = None;
                    after = 0;
                    at = found.saturating_add(1);
                    if used.saturating_add(width) <= inner {
                        used = used.saturating_add(width);
                        if grapheme.symbol == " " {
                            space = Some(here);
                        }
                        continue;
                    }
                    // The glyph fits no restarted window either: the
                    // restart is one hard-cut piece holding the scanned
                    // glyphs, unless it is empty (the space ended the
                    // window).
                    if at < here {
                        pieces = pieces.saturating_add(1);
                    }
                    at = here;
                    used = 0;
                }
                _ => {
                    // Hard cut (a space at the piece start cuts hard too).
                    if here > at {
                        pieces = pieces.saturating_add(1);
                    }
                    at = here;
                    used = 0;
                    space = None;
                    after = 0;
                }
            }
            // The glyph opens the piece at `at`: wider than `inner` it
            // stands alone, else it starts the used widths. No space is
            // recorded: the piece starts at this glyph, and a split needs
            // a space strictly inside its window, so a space here could
            // never split (a later space overwrites it as usual).
            if width > inner {
                pieces = pieces.saturating_add(1);
                at = here.saturating_add(1);
            } else {
                used = width;
            }
        }
    }
    if at < cells {
        pieces = pieces.saturating_add(1);
    }
    pieces
}

/// One drawn glyph with its style.
struct Cell<'a> {
    ch: &'a str,
    width: usize,
    style: Style,
}

/// One piece of a split row: its chars, and how it joins the row before.
struct Planned {
    start: usize,
    end: usize,
    join: Join,
}

/// `cells`' starting cell offsets, with the total width last: cell `i`
/// covers original cells `off[i]` on.
fn offsets(cells: &[Cell<'_>]) -> Vec<usize> {
    let mut off = Vec::with_capacity(cells.len().saturating_add(1));
    let mut at: usize = 0;
    off.push(at);
    for cell in cells {
        at = at.saturating_add(cell.width);
        off.push(at);
    }
    off
}
/// `line` flattened to its glyphs with their widths and styles.
fn cells_of<'a>(line: &'a Line<'_>) -> Vec<Cell<'a>> {
    let mut cells = Vec::new();
    for span in &line.spans {
        // Ratatui excludes `\n` here because a line break is not a drawn glyph.
        for grapheme in span.styled_graphemes(Style::default()) {
            cells.push(Cell {
                ch: grapheme.symbol,
                width: crate::format::width(grapheme.symbol),
                style: grapheme.style,
            });
        }
    }
    cells
}

/// Where `cells` breaks at `inner`: each piece holds at most `inner`
/// cells, breaking after the last space in its window (that space is
/// dropped and the next piece joins with it) or cutting hard with no
/// space there. No glyph is cut; a glyph wider than `inner` takes a
/// piece of its own. One plan when the row fits, none when it is empty.
fn plan(cells: &[Cell<'_>], inner: usize) -> Vec<Planned> {
    if inner == 0 || cells.is_empty() {
        return Vec::new();
    }
    let mut plans = Vec::new();
    let mut at = 0;
    let mut join = Join::Break;
    for _ in 0..cells.len() {
        let mut end = at;
        let mut used = 0;
        for (index, cell) in cells.iter().enumerate().skip(at) {
            if used + cell.width > inner {
                break;
            }
            used = used.saturating_add(cell.width);
            end = index.saturating_add(1);
        }
        let end = end.max(at.saturating_add(1));
        if end == cells.len() {
            plans.push(Planned {
                start: at,
                end,
                join,
            });
            break;
        }
        let space = cells
            .get(at..end)
            .unwrap_or_default()
            .iter()
            .rposition(|cell| cell.ch == " ")
            .map(|found| found + at);
        match space {
            Some(found) if found > at => {
                plans.push(Planned {
                    start: at,
                    end: found,
                    join,
                });
                at = found.saturating_add(1);
                join = Join::WrapSpace;
            }
            _ => {
                plans.push(Planned {
                    start: at,
                    end,
                    join,
                });
                at = end;
                join = Join::Wrap;
            }
        }
    }
    plans
}

/// The piece's line: its cells' text under their styles, keeping the
/// line's style and alignment. A glyph wider than `inner` draws as one
/// blank cell, so the piece fits the room.
fn build_piece(
    line: &Line<'_>,
    cells: &[Cell<'_>],
    planned: &Planned,
    inner: u16,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current = (Style::default(), String::new());
    let mut started = false;
    for cell in cells.get(planned.start..planned.end).unwrap_or_default() {
        let (text, style) = if cell.width > usize::from(inner) {
            (" ".to_owned(), cell.style)
        } else {
            (cell.ch.to_owned(), cell.style)
        };
        if !started || current.0 != style {
            if !current.1.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut current.1), current.0));
            }
            current.0 = style;
            started = true;
        }
        current.1.push_str(&text);
    }
    if !current.1.is_empty() {
        spans.push(Span::styled(std::mem::take(&mut current.1), current.0));
    }
    let mut piece = Line::from(spans);
    piece.style = line.style;
    piece.alignment = line.alignment;
    piece
}

/// The tail cells of the original tail run `[total - tail, total)` that
/// fall in `[start, end)`.
fn tail_in(text: &RowText, total: usize, start: usize, end: usize) -> u16 {
    let tail = usize::from(text.tail);
    let from = total.saturating_sub(tail).max(start);
    let to = total.min(end);
    u16::try_from(to.saturating_sub(from)).unwrap_or(u16::MAX)
}

/// Prefixes `line` with `pad` blank cells, which take the line's
/// background, and shifts the row's text marks past them.
fn prefix_row(line: &mut Line<'static>, text: &mut RowText, pad: u16) {
    if pad == 0 {
        return;
    }
    line.spans
        .insert(0, Span::raw(" ".repeat(usize::from(pad))));
    text.skip = text.skip.saturating_add(pad);
    for (range, _) in &mut text.links {
        range.start = range.start.saturating_add(pad);
        range.end = range.end.saturating_add(pad);
    }
    if let Some(spin) = text.spin {
        text.spin = Some(spin.saturating_add(pad));
    }
}

impl Deref for Rows {
    type Target = [Row];

    fn deref(&self) -> &[Row] {
        &self.rows
    }
}

impl Extend<Row> for Rows {
    fn extend<I: IntoIterator<Item = Row>>(&mut self, rows: I) {
        for row in rows {
            self.push(row);
        }
    }
}

#[cfg(test)]
#[path = "rows_tests.rs"]
mod tests;
