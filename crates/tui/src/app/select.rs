//! Drag to select conversation text and copy it on release (`docs/tui.md`,
//! "Selection and copy"; "History and paging": a selection's ends are row
//! indices). [`Selection`] owns the drag and a copy waiting on dropped
//! pages; the app hands it points in conversation rows and the pages to
//! read the text from.

use ratatui::layout::{Position, Rect};

use super::{App, Effect};
use crate::cells;
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::logical;
use crate::mouse::{self, TargetId};

/// What a copy waiting on dropped pages says when they cannot load.
const COPY_FAILED: &str = "Could not copy: history could not be loaded.";

/// A cell of the conversation: its conversation row and its column in the
/// conversation area. Ordered by row, then column, as text reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Point {
    pub(crate) row: usize,
    pub(crate) col: u16,
}

/// A selection's copy waiting on dropped pages: its ends and the pages it
/// pinned, which it unpins exactly once.
#[derive(Debug)]
pub(in crate::app) struct PendingCopy {
    from: Point,
    to: Point,
    pages: Vec<usize>,
}

/// The drag and a copy waiting on dropped pages.
#[derive(Debug, Default)]
pub(in crate::app) struct Selection {
    /// Where the left press that began the drag went down.
    anchor: Option<Point>,
    /// Where the drag is, once it moved off the press's cell.
    head: Option<Point>,
    /// The left button is held since a press that anchored a selection:
    /// `anchor` is set.
    held: bool,
    pending: Option<PendingCopy>,
}

impl Selection {
    /// A left press at `point` anchors a new selection.
    fn press(&mut self, point: Point) {
        self.anchor = Some(point);
        self.head = None;
        self.held = true;
    }

    /// The pointer dragged to `point` with the button held since a press
    /// that anchored: the selection shows once it leaves the press's cell,
    /// and stays shown when it returns there.
    fn drag(&mut self, point: Point) {
        if self.held && (Some(point) != self.anchor || self.head.is_some()) {
            self.head = Some(point);
        }
    }

    /// The button came up: the selection's ends, when it dragged; a press
    /// released without a drag selects nothing.
    fn release(&mut self) -> Option<(Point, Point)> {
        self.held = false;
        self.span()
    }

    /// Drops the drag; a pending copy stays.
    fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
        self.held = false;
    }

    /// The selection's ends in reading order, while one shows.
    pub(in crate::app) fn span(&self) -> Option<(Point, Point)> {
        let (anchor, head) = (self.anchor?, self.head?);
        Some((anchor.min(head), anchor.max(head)))
    }

    /// The pages a pending copy pinned.
    pub(in crate::app) fn pending_pages(&self) -> &[usize] {
        self.pending
            .as_ref()
            .map_or(&[][..], |pending| pending.pages.as_slice())
    }
}

/// A drawn grapheme's bytes in its line, and whether the selection covers
/// its cell.
type Covered = (std::ops::Range<usize>, bool);

/// What a char of a selected line is: under a selected cell, under one
/// outside the selection, or drawn in no cell (a space a wrap dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Under {
    Selected,
    Outside,
    Unplaced,
}

impl App {
    /// A mouse report against the last frame's targets: a left press in
    /// the conversation's text anchors a selection, a drag extends it, and
    /// the release copies it. Any left press clears the selection shown.
    pub(crate) fn on_select(&mut self, mouse: &Mouse, targets: &[mouse::Target]) -> Effect {
        match mouse.kind {
            MouseKind::Press(Button::Left) => {
                self.clear_selection();
                let target_ok = mouse::under(targets, mouse.col, mouse.row)
                    .is_none_or(|target| matches!(target.id, TargetId::Line(_)));
                if let Some(point) = self.point_at(mouse.col, mouse.row)
                    && self.session().is_some()
                    && self.home_screen().is_none()
                    && !self.conversation_covered()
                    && target_ok
                {
                    self.select.press(point);
                    self.notices.hold();
                }
                Effect::None
            }
            MouseKind::Drag(Button::Left) => {
                if let Some(point) = self.clamped(mouse.col, mouse.row) {
                    self.select.drag(point);
                }
                Effect::None
            }
            MouseKind::Release => {
                self.notices.release();
                match self.select.release() {
                    Some((from, to)) => self.copy_selection(from, to),
                    None => Effect::None,
                }
            }
            MouseKind::Press(_)
            | MouseKind::Drag(_)
            | MouseKind::Motion
            | MouseKind::WheelUp
            | MouseKind::WheelDown => Effect::None,
        }
    }

    /// The text of a copy that waited on dropped pages, once every page it
    /// covers is resident; it then unpins them and "Copied" shows. A page
    /// that failed to load, or a lost link, abandons it with a notice.
    /// `None` at no cost while nothing waits.
    pub(crate) fn take_copy(&mut self) -> Option<String> {
        let pending = self.select.pending.as_ref()?;
        let (from, to) = (pending.from, pending.to);
        let failed = pending
            .pages
            .iter()
            .any(|at| self.screen.pages().page_failed(*at));
        if failed || !self.connected() {
            self.abandon_copy();
            return None;
        }
        match self.selection_text(from, to) {
            Ok(text) => {
                self.drop_pending_copy();
                self.copied = true;
                Some(text)
            }
            Err(missing) => {
                // A page of its range dropped anyway: pinned again, and
                // asked for in the next frame.
                let fresh: Vec<usize> = missing
                    .into_iter()
                    .filter(|at| !self.select.pending_pages().contains(at))
                    .collect();
                for at in &fresh {
                    self.screen.pages_mut().want(*at);
                }
                if let Some(pending) = &mut self.select.pending {
                    pending.pages.extend(fresh);
                }
                None
            }
        }
    }

    /// The rect the conversation draws in: the chrome's body while the
    /// layout applies, else the whole screen, cut to the conversation's
    /// rows from its top.
    pub(crate) fn conversation_area(&self) -> Rect {
        let screen = Rect::new(0, 0, self.screen.width(), self.screen.height());
        let area = self
            .chrome
            .layout()
            .map_or(screen, |layout| crate::view::chrome::body(&layout));
        let height = u16::try_from(self.conversation_height()).unwrap_or(u16::MAX);
        Rect {
            height: height.min(area.height),
            ..area
        }
    }

    /// Whether something covers the conversation: the key map, the notice
    /// overlay or the repository offer's swapped view. A press there
    /// starts no selection.
    pub(crate) fn conversation_covered(&self) -> bool {
        self.keymap_top().is_some() || self.notice_overlay().is_some() || self.offer_open()
    }

    /// The cells the selection highlights in `area`, one rect per row, in
    /// reading order: from its start cell to the row's end, whole rows
    /// between, the last row's start to its end cell. Never the "↓ New
    /// messages below" row.
    pub(crate) fn selection_cells(&self, area: Rect) -> Vec<Rect> {
        let Some((from, to)) = self.select.span() else {
            return Vec::new();
        };
        let (top, y0, shown) = self.view_rows(area);
        let last = self.last_row(area, y0, shown);
        let mut out = Vec::new();
        for row in from.row.max(top)..=to.row {
            let Some(y) = row
                .checked_sub(top)
                .and_then(|at| u16::try_from(at).ok())
                .map(|at| y0.saturating_add(at))
                .filter(|y| last.is_some_and(|last| *y <= last))
            else {
                continue;
            };
            let start = if row == from.row { from.col } else { 0 };
            let end = if row == to.row {
                to.col.saturating_add(1).min(area.width)
            } else {
                area.width
            };
            if start < end {
                out.push(Rect::new(
                    area.x.saturating_add(start),
                    y,
                    end.saturating_sub(start),
                    1,
                ));
            }
        }
        out
    }

    /// Esc clears a selection shown, before anything it would interrupt;
    /// with the notice overlay open the overlay closes first.
    pub(in crate::app) fn select_key(&mut self, key: &Key) -> Option<Effect> {
        if *key != Key::Esc || self.select.span().is_none() || self.notice_overlay().is_some() {
            return None;
        }
        self.clear_selection();
        Some(Effect::None)
    }

    /// Clears the selection and drops a copy waiting on dropped pages,
    /// unpinning them; notices a drag held show.
    pub(in crate::app) fn clear_selection(&mut self) {
        self.select.clear();
        self.notices.release();
        self.drop_pending_copy();
    }

    /// Drops a copy waiting on dropped pages with the notice that says it
    /// could not run: the pages failed to load or the link went down.
    pub(in crate::app) fn abandon_copy(&mut self) {
        if self.select.pending.is_some() {
            self.drop_pending_copy();
            self.notices.push(COPY_FAILED.to_owned());
        }
    }

    /// Unpins exactly the pages a pending copy pinned, once.
    fn drop_pending_copy(&mut self) {
        if let Some(pending) = self.select.pending.take() {
            for at in pending.pages {
                self.screen.pages_mut().unwant(at);
            }
        }
    }

    /// A released selection's copy: at once when its pages are resident,
    /// else its dropped pages pinned and the copy left to
    /// [`App::take_copy`]. A copy already waiting is replaced.
    fn copy_selection(&mut self, from: Point, to: Point) -> Effect {
        self.drop_pending_copy();
        match self.selection_text(from, to) {
            Ok(text) => {
                self.copied = true;
                Effect::Copy(text)
            }
            Err(missing) => {
                for at in &missing {
                    self.screen.pages_mut().want(*at);
                }
                self.select.pending = Some(PendingCopy {
                    from,
                    to,
                    pages: missing,
                });
                Effect::None
            }
        }
    }

    /// The top row shown, the screen row the first shown row draws on (the
    /// view bottom-aligns a short conversation) and how many rows show, in
    /// `area`, as the view draws them.
    fn view_rows(&self, area: Rect) -> (usize, u16, usize) {
        let (top, total) = self.scroll();
        let height = usize::from(area.height);
        let shown = total.min(top.saturating_add(height)).saturating_sub(top);
        let blank = u16::try_from(height.saturating_sub(shown)).unwrap_or(u16::MAX);
        (top, area.y.saturating_add(blank), shown)
    }

    /// The last screen row a selection reaches in `area`: the last shown
    /// row, above "↓ New messages below" when it shows; `None` with no row.
    fn last_row(&self, area: Rect, y0: u16, shown: usize) -> Option<u16> {
        let shown = u16::try_from(shown).unwrap_or(u16::MAX);
        let mut last = y0.saturating_add(shown).checked_sub(1)?;
        if self.has_new() {
            last = last.min(area.bottom().checked_sub(2)?);
        }
        (last >= y0).then_some(last)
    }

    /// The conversation cell under screen cell `col`, `row`: inside the
    /// conversation area, on a shown row, and not the "↓ New messages
    /// below" row.
    fn point_at(&self, col: u16, row: u16) -> Option<Point> {
        let area = self.conversation_area();
        if !area.contains(Position::new(col, row)) {
            return None;
        }
        let (top, y0, shown) = self.view_rows(area);
        let last = self.last_row(area, y0, shown)?;
        if row < y0 || row > last {
            return None;
        }
        Some(Point {
            row: top.saturating_add(usize::from(row.saturating_sub(y0))),
            col: col.saturating_sub(area.x),
        })
    }

    /// The conversation cell nearest screen cell `col`, `row`: a drag
    /// outside the area is clamped to its edge cell.
    fn clamped(&self, col: u16, row: u16) -> Option<Point> {
        let area = self.conversation_area();
        let (top, y0, shown) = self.view_rows(area);
        let last = self.last_row(area, y0, shown)?;
        let right = area.right().checked_sub(1)?;
        // Past the right or bottom edge is its last cell; the subtractions
        // saturate, so past the left or top edge is its first.
        Some(Point {
            row: top.saturating_add(usize::from(row.min(last).saturating_sub(y0))),
            col: col.min(right).saturating_sub(area.x),
        })
    }

    /// The text under the cells from `from` to `to`, unwrapped: each
    /// logical line's chars under a selected cell, the spaces a wrap
    /// dropped between two of them, and blank lines wholly inside, joined
    /// by newlines. The pages it covers that are dropped, on `Err`.
    fn selection_text(&self, from: Point, to: Point) -> Result<String, Vec<usize>> {
        let pages = self.screen.pages();
        let index = pages.index();
        let last_page = index.pages().len().saturating_sub(1);
        let first = index.locate(from.row).map_or(last_page, |(at, _)| at);
        let last = index.locate(to.row).map_or(last_page, |(at, _)| at);
        let missing: Vec<usize> = (first..=last)
            .filter(|at| pages.page_text(*at).is_none())
            .collect();
        if !missing.is_empty() {
            return Err(missing);
        }
        let width = pages.wrap_width();
        let mut out: Vec<String> = Vec::new();
        for at in first..=last {
            let Some((rows, texts)) = pages.page_text(at) else {
                continue;
            };
            // Each line's first row, and its graphemes' cells with whether
            // the selection covers them.
            let mut row = index.start(at);
            let mut placed: Vec<(usize, Vec<Covered>)> = Vec::new();
            for (line, _) in &rows {
                let cells = cells::place(line, width)
                    .into_iter()
                    .map(|cell| {
                        let point = Point {
                            row: row.saturating_add(usize::from(cell.row)),
                            col: cell.col,
                        };
                        (cell.bytes, from <= point && point <= to)
                    })
                    .collect();
                placed.push((row, cells));
                row = row.saturating_add(crate::view::rows(line.clone(), width));
            }
            for line in logical::logical(&rows, &texts) {
                let under = |from: &Option<(usize, usize)>| -> Under {
                    let Some((at, byte)) = from else {
                        return Under::Unplaced;
                    };
                    let Some((_, cells)) = placed.get(*at) else {
                        return Under::Unplaced;
                    };
                    let next = cells.partition_point(|(bytes, _)| bytes.end <= *byte);
                    match cells.get(next) {
                        Some((bytes, true)) if bytes.start <= *byte => Under::Selected,
                        Some((bytes, false)) if bytes.start <= *byte => Under::Outside,
                        Some(_) | None => Under::Unplaced,
                    }
                };
                let mut text = String::new();
                let mut gap = String::new();
                let mut started = false;
                for (ch, at) in line.text.chars().zip(&line.from) {
                    match under(at) {
                        Under::Selected => {
                            text.push_str(&gap);
                            gap.clear();
                            text.push(ch);
                            started = true;
                        }
                        Under::Unplaced if started => gap.push(ch),
                        // The selection is one run in reading order: a
                        // char outside it, or one drawn nowhere before it,
                        // adds nothing.
                        Under::Outside | Under::Unplaced => {}
                    }
                }
                // A blank line has no char to place: it is copied when the
                // selection covers its row.
                let blank = line.text.is_empty()
                    && placed
                        .get(line.row)
                        .is_some_and(|(row, _)| from.row <= *row && *row <= to.row);
                if started || blank {
                    out.push(text);
                }
            }
        }
        Ok(out.join("\n"))
    }
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
