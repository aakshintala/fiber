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

/// Which text surface a selection lives on: the press claims one, and
/// the drag stays inside it (`docs/tui.md`, "Selection and copy").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceId {
    /// The conversation's rows.
    Conversation,
    /// The input box's draft, in piece indices.
    Input,
    /// The notice rows above the working line.
    Notices,
    /// A panel card.
    Panel,
    /// The rail's text.
    Rail,
    /// The approval panel and question forms.
    Request,
    /// A swapped view: a picker, `/settings`, the login view, results.
    Swapped,
    /// A framed overlay: the notice overlay, the quit question.
    Overlay,
    /// The home screen's text.
    Home,
}

/// One drawn text row a surface noted while the last frame drew it:
/// the visible text cells with everything decorative left out by the
/// renderer that recorded it (`docs/tui.md`, "Selection and copy").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpanRow {
    /// The row's screen row.
    pub(crate) y: u16,
    /// The screen column of its first text cell.
    pub(crate) x: u16,
    /// The screen column past its last text cell.
    pub(crate) end: u16,
    /// Its visible text, in reading order.
    pub(crate) text: String,
    /// How its text follows the row before it: a new line, a word
    /// broken at the edge, or a dropped wrap space
    /// (`docs/tui.md`, "Selection and copy").
    pub(crate) join: crate::rows::Join,
}

/// A surface's noted text rows, in reading order, from the frame that
/// drew them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotedSurface {
    pub(crate) surface: SurfaceId,
    pub(crate) rows: Vec<SpanRow>,
}

/// The hit-test order over noted surfaces: a framed overlay covers
/// everything under it, then the swapped view, the request panel, the
/// panel and the rail beside the conversation and the notices above the
/// working line. The input box and the conversation are claimed
/// geometrically, not from notes.
const NOTE_ORDER: [SurfaceId; 7] = [
    SurfaceId::Overlay,
    SurfaceId::Swapped,
    SurfaceId::Request,
    SurfaceId::Panel,
    SurfaceId::Rail,
    SurfaceId::Notices,
    SurfaceId::Home,
];

/// A cell of a surface: its surface-local row and its column in the
/// surface's area. Ordered by row, then column, as text reads. On the
/// input box the row is a draft piece index and the column is unused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Point {
    pub(crate) row: usize,
    pub(crate) col: u16,
}

/// The screen columns of `text`'s chars in cells `from..to` past `x`:
/// `None` when no char shows there.
fn cells_range(text: &str, x: u16, from: u16, to: u16) -> Option<(u16, u16)> {
    let mut col = 0u16;
    let mut start = None;
    let mut end = None;
    for ch in text.chars() {
        let wide = u16::try_from(crate::format::width(&ch.to_string())).unwrap_or(u16::MAX);
        if col < to && col.saturating_add(wide) > from {
            if start.is_none() {
                start = Some(x.saturating_add(col));
            }
            end = Some(x.saturating_add(col).saturating_add(wide));
        }
        col = col.saturating_add(wide);
    }
    match (start, end) {
        (Some(start), Some(end)) if end > start => Some((start, end.min(x.saturating_add(col)))),
        _ => None,
    }
}

/// The chars of `text` drawn in cells `from..to`: whole graphemes
/// only, a wide char kept when its first cell is in. Cell offsets count
/// display columns, as the noted rows do.
fn slice_cells(text: &str, from: u16, to: u16) -> String {
    let mut out = String::new();
    let mut col = 0u16;
    for ch in text.chars() {
        let wide = u16::try_from(crate::format::width(&ch.to_string())).unwrap_or(u16::MAX);
        if col >= to {
            break;
        }
        if col.saturating_add(wide) > from {
            out.push(ch);
        }
        col = col.saturating_add(wide);
    }
    out
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
    /// The surface the press claimed; the drag stays inside it.
    surface: Option<SurfaceId>,
    /// Where the left press that began the drag went down, in the
    /// surface's local cells.
    anchor: Option<Point>,
    /// Where the drag is, once it moved off the press's cell.
    head: Option<Point>,
    /// The left button is held since a press that anchored a selection:
    /// `anchor` is set.
    held: bool,
    pending: Option<PendingCopy>,
    /// The input box's shown text slab and its first draft row, noted
    /// while the last frame drew it: the press and drag map their cells
    /// through it. `None` while the box draws nothing.
    input_note: std::cell::RefCell<Option<(Rect, usize)>>,
    /// Every other text surface's noted rows, topmost first, from the
    /// frame that drew them. The press claims the topmost surface whose
    /// rows hold its cell.
    notes: std::cell::RefCell<Vec<NotedSurface>>,
}

impl Selection {
    /// A left press at `point` claims `surface` for a new selection.
    fn press(&mut self, surface: SurfaceId, point: Point) {
        self.surface = Some(surface);
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

    /// The button came up: the selection's ends, when the left button's
    /// gesture is still held and it dragged; a press released without a
    /// drag selects nothing, and a release with no gesture held copies
    /// nothing while the shown selection stays.
    fn release(&mut self) -> Option<(Point, Point)> {
        // SGR does not say which button came up: only a release ending
        // the left button's gesture copies.
        let gesture = self.held;
        self.held = false;
        if !gesture {
            return None;
        }
        self.span()
    }

    /// Drops the drag; a pending copy stays.
    fn clear(&mut self) {
        self.surface = None;
        self.anchor = None;
        self.head = None;
        self.held = false;
    }

    /// The claimed surface, while a press anchors one.
    pub(in crate::app) fn surface(&self) -> Option<SurfaceId> {
        self.surface
    }

    /// Whether the press's button is still held: a press with no drag
    /// yet, which Esc clears before anything else.
    fn held(&self) -> bool {
        self.held
    }

    /// Notes the input box's shown text slab and its first draft row,
    /// from the frame that drew it.
    pub(crate) fn note_input(&self, area: Rect, top: usize) {
        *self.input_note.borrow_mut() = Some((area, top));
    }

    /// Drops the input box's note, before the next frame notes its own.
    pub(crate) fn clear_input_note(&self) {
        *self.input_note.borrow_mut() = None;
    }

    /// The input box's shown text slab and its first draft row.
    fn input_note(&self) -> Option<(Rect, usize)> {
        *self.input_note.borrow()
    }

    /// Notes `surface`'s drawn text rows. Empty rows note nothing, so a
    /// surface squeezed to nothing takes the old path. A surface may
    /// note twice in one frame for two disjoint regions, such as home's
    /// notice overlay under its question.
    pub(crate) fn note_surface(&self, noted: NotedSurface) {
        if noted.rows.is_empty() {
            return;
        }
        self.notes.borrow_mut().push(noted);
    }

    /// Drops every surface's note, before the next frame notes its own.
    pub(crate) fn clear_notes(&self) {
        self.notes.borrow_mut().clear();
    }

    /// The noted rows of `surface`, in reading order, every region the
    /// frame noted.
    fn noted_rows(&self, surface: SurfaceId) -> Vec<SpanRow> {
        self.notes
            .borrow()
            .iter()
            .filter(|slot| slot.surface == surface)
            .flat_map(|slot| slot.rows.clone())
            .collect()
    }

    /// The noted cell of `surface` under `col`, `row`: the row holding
    /// it, else the nearest row when the press lands inside the
    /// surface's box but on no text (a blank gap or an empty row), so a
    /// press on the surface's chrome cannot fall through to the surface
    /// beneath it. `None` outside the box.
    fn noted_press(&self, surface: SurfaceId, col: u16, row: u16) -> Option<Point> {
        if let Some(point) = self.noted_at(surface, col, row) {
            return Some(point);
        }
        let area = self.noted_box(surface)?;
        if area.is_empty()
            || col < area.x
            || col >= area.right()
            || row < area.y
            || row >= area.bottom()
        {
            return None;
        }
        self.clamp_noted(surface, col, row)
    }

    /// The noted row holding cell `col`, `row` of `surface`: its index
    /// with the cell's offset past its first text cell. `None` on no
    /// noted row.
    fn noted_at(&self, surface: SurfaceId, col: u16, row: u16) -> Option<Point> {
        let rows = self.noted_rows(surface);
        let (at, span) = rows
            .iter()
            .enumerate()
            .find(|(_, span)| row == span.y && col >= span.x && col < span.end.max(span.x))?;
        // Past the text, the press reads as the text's end: chrome
        // holds no text to start from.
        let wide = u16::try_from(crate::format::width(&span.text)).unwrap_or(u16::MAX);
        Some(Point {
            row: at,
            col: col.saturating_sub(span.x).min(wide),
        })
    }

    /// The noted rows' bounding box on screen: where a drag clamps.
    /// `None` while the surface noted nothing.
    fn noted_box(&self, surface: SurfaceId) -> Option<Rect> {
        let rows = self.noted_rows(surface);
        let top = rows.iter().map(|span| span.y).min()?;
        let bottom = rows.iter().map(|span| span.y).max()?.saturating_add(1);
        let left = rows.iter().map(|span| span.x).min().unwrap_or(0);
        let right = rows.iter().map(|span| span.end).max().unwrap_or(0);
        Some(Rect::new(
            left,
            top,
            right.saturating_sub(left),
            bottom.saturating_sub(top),
        ))
    }

    /// The nearest cell of a noted `surface` to `col`, `row`: a drag
    /// outside the surface clamps to its edge cell.
    fn clamp_noted(&self, surface: SurfaceId, col: u16, row: u16) -> Option<Point> {
        let area = self.noted_box(surface)?;
        if area.is_empty() {
            return None;
        }
        let clamped_col = col.clamp(area.x, area.right().saturating_sub(1).max(area.x));
        let rows = self.noted_rows(surface);
        let clamped_row = row.clamp(area.y, area.bottom().saturating_sub(1));
        // The nearest noted row to the clamped row: rows read top to
        // bottom, so the last row on or above it, else the first.
        let (at, span) = rows
            .iter()
            .enumerate()
            .rfind(|(_, span)| span.y <= clamped_row)
            .or_else(|| rows.iter().enumerate().next())?;
        // Past the text, the drag reads as the text's end, as the
        // conversation's clamp does.
        let wide = u16::try_from(crate::format::width(&span.text)).unwrap_or(u16::MAX);
        Some(Point {
            row: at,
            col: clamped_col.saturating_sub(span.x).min(wide),
        })
    }

    /// The text of the noted rows from `from` to `to`: each row's cells
    /// in the range, trailing spaces dropped; rows a wrap joins onto the
    /// next with no newline, other rows ending the line.
    fn noted_text(&self, surface: SurfaceId, from: Point, to: Point) -> String {
        let rows = self.noted_rows(surface);
        let (start, end) = (from.min(to), from.max(to));
        let mut parts: Vec<String> = Vec::new();
        let mut line = String::new();
        let mut first = true;
        for (at, span) in rows.iter().enumerate() {
            if at < start.row || at > end.row {
                continue;
            }
            if !first {
                match span.join {
                    crate::rows::Join::Break => parts.push(std::mem::take(&mut line)),
                    crate::rows::Join::Wrap => {}
                    crate::rows::Join::WrapSpace => line.push(' '),
                }
            }
            first = false;
            let from_col = if at == start.row { start.col } else { 0 };
            let to_col = if at == end.row {
                end.col.saturating_add(1)
            } else {
                u16::MAX
            };
            let mut fragment = slice_cells(&span.text, from_col, to_col);
            // Trailing spaces on each row are dropped: a wrap's joint
            // reads as no gap at all.
            while fragment.ends_with(' ') {
                fragment.pop();
            }
            line.push_str(&fragment);
        }
        parts.push(line);
        parts.join("\n")
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

/// What a char of a selected line is: under a selected cell, or not (outside
/// the selection, or drawn in no cell like a space a wrap dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Under {
    Selected,
    Unplaced,
}

impl App {
    /// A mouse report against the last frame's targets: a left press
    /// claims the surface under it for a selection, a drag extends the
    /// selection inside that surface, and the release copies it. Any left
    /// press clears the selection shown. A press with no drag selects
    /// nothing, so the click path still fires.
    pub(crate) fn on_select(&mut self, mouse: &Mouse, targets: &[mouse::Target]) -> Effect {
        match mouse.kind {
            MouseKind::Press(Button::Left) => {
                self.clear_selection();
                if let Some((surface, point)) = self.press_at(mouse.col, mouse.row, targets) {
                    self.select.press(surface, point);
                    if surface == SurfaceId::Input {
                        self.draft.select_at(point.row);
                    }
                    self.notices.hold();
                }
                Effect::None
            }
            MouseKind::Drag(Button::Left) => {
                let surface = self.select.surface();
                if let Some(point) =
                    surface.and_then(|surface| self.clamp_surface(surface, mouse.col, mouse.row))
                {
                    self.select.drag(point);
                    if surface == Some(SurfaceId::Input) {
                        self.draft.select_to(point.row);
                    }
                }
                Effect::None
            }
            MouseKind::Release => {
                self.notices.release();
                match (self.select.surface(), self.select.release()) {
                    (Some(SurfaceId::Input), Some(_)) => self.copy_input(),
                    (Some(surface), Some((from, to))) if surface != SurfaceId::Conversation => {
                        self.copy_noted(surface, from, to)
                    }
                    (_, Some((from, to))) => self.copy_selection(from, to),
                    (_, None) => Effect::None,
                }
            }
            MouseKind::Press(_)
            | MouseKind::Drag(_)
            | MouseKind::Motion
            | MouseKind::WheelUp
            | MouseKind::WheelDown => Effect::None,
        }
    }

    /// The surface a left press at `col`, `row` claims, with its local
    /// cell: the topmost noted surface holding the cell, else the input
    /// box, else the conversation's text. `None` where no selectable
    /// surface shows.
    fn press_at(
        &self,
        col: u16,
        row: u16,
        targets: &[mouse::Target],
    ) -> Option<(SurfaceId, Point)> {
        for surface in NOTE_ORDER {
            if let Some(point) = self.select.noted_press(surface, col, row) {
                return Some((surface, point));
            }
        }
        if let Some(point) = self.input_at(col, row) {
            return Some((SurfaceId::Input, point));
        }
        // A press on a link still starts a selection: the release
        // copies the text, and a click without a drag follows the
        // link (`docs/tui.md`, "Selection and copy", "Links").
        let target_ok = mouse::under(targets, col, row)
            .is_none_or(|target| matches!(target.id, TargetId::Line(_) | TargetId::Link { .. }));
        if let Some(point) = self.point_at(col, row)
            && self.session().is_some()
            && self.home_screen().is_none()
            && !self.conversation_covered()
            && target_ok
        {
            return Some((SurfaceId::Conversation, point));
        }
        None
    }

    /// The draft piece under cell `col`, `row`: inside the input box's
    /// noted slab, on a draft row the frame showed.
    fn input_at(&self, col: u16, row: u16) -> Option<Point> {
        let (area, top) = self.select.input_note()?;
        if !area.contains(ratatui::layout::Position::new(col, row)) {
            return None;
        }
        let at = usize::from(row.saturating_sub(area.y));
        let piece = self.draft.position_at(
            area.width,
            top.saturating_add(at),
            col.saturating_sub(area.x),
        )?;
        Some(Point { row: piece, col: 0 })
    }

    /// The nearest cell of `surface` to `col`, `row`: a drag outside the
    /// surface clamps to its edge cell and never spills into a neighbour.
    fn clamp_surface(&self, surface: SurfaceId, col: u16, row: u16) -> Option<Point> {
        match surface {
            SurfaceId::Conversation => self.clamped(col, row),
            SurfaceId::Input => {
                let (area, top) = self.select.input_note()?;
                let clamped_col = col.clamp(area.x, area.right().saturating_sub(1));
                let clamped_row = row.clamp(area.y, area.bottom().saturating_sub(1));
                let at = usize::from(clamped_row.saturating_sub(area.y));
                let piece = self.draft.position_at(
                    area.width,
                    top.saturating_add(at),
                    clamped_col.saturating_sub(area.x),
                )?;
                Some(Point { row: piece, col: 0 })
            }
            SurfaceId::Notices
            | SurfaceId::Panel
            | SurfaceId::Rail
            | SurfaceId::Request
            | SurfaceId::Swapped
            | SurfaceId::Overlay
            | SurfaceId::Home => self.select.clamp_noted(surface, col, row),
        }
    }

    /// A noted surface's copy: the text of its rows in the range.
    fn copy_noted(&mut self, surface: SurfaceId, from: Point, to: Point) -> Effect {
        let text = self.select.noted_text(surface, from, to);
        if text.is_empty() {
            return Effect::None;
        }
        self.copied = true;
        Effect::Copy(text)
    }

    /// The input box selection's copy: the visible text of the draft's
    /// selected pieces, pasted tokens and images as their labels.
    fn copy_input(&mut self) -> Effect {
        match self.draft.selected_text() {
            Some(text) => {
                self.copied = true;
                Effect::Copy(text)
            }
            None => Effect::None,
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

    /// The rect the conversation draws in: the column past the gutter
    /// while the layout applies, else the whole screen, cut to the
    /// conversation's rows from its top. It keeps the scroll bar's
    /// column, as now.
    pub(crate) fn conversation_area(&self) -> Rect {
        let screen = Rect::new(0, 0, self.screen.width(), self.screen.height());
        let raw = self.chrome.layout().map_or(screen, |layout| layout.column);
        let area = crate::layout::past_gutter(raw, self.chrome.gutter());
        let height = u16::try_from(self.conversation_height()).unwrap_or(u16::MAX);
        // The item view's header takes the conversation's top rows: a
        // selection starts below it.
        let top = if self.item_open() {
            u16::try_from(crate::view::item::ITEM_HEADER_ROWS).unwrap_or(u16::MAX)
        } else {
            0
        };
        let y = area.y.saturating_add(top).min(area.bottom());
        Rect {
            y,
            height: height.min(area.bottom().saturating_sub(y)),
            ..area
        }
    }

    /// Whether something covers the conversation: the key map, the notice
    /// overlay, the repository offer's swapped view or the search results.
    /// A press there starts no selection. An item view without a
    /// transcript covers it too, since no conversation row draws there.
    pub(crate) fn conversation_covered(&self) -> bool {
        self.keymap_top().is_some()
            || self.notice_overlay().is_some()
            || self.offer_open()
            || self.results_open()
            || self.item_view().is_some_and(|view| !view.has_transcript)
    }

    /// The cells the selection highlights on `surface` in `area`, one
    /// rect per row, in reading order. On the conversation: from its
    /// start cell to the row's end, whole rows between, the last row's
    /// start to its end cell, never the "↓ New messages below" row. On
    /// the input box: the draft's selected rects, `area` the text column
    /// with the first draft row on its first row. Empty for any other
    /// surface than the claimed one, so overlapping coordinates never
    /// alias.
    pub(crate) fn selection_cells(&self, surface: SurfaceId, area: Rect) -> Vec<Rect> {
        if self.select.surface() != Some(surface) {
            return Vec::new();
        }
        if surface == SurfaceId::Input {
            return self
                .draft
                .selected_rects(area.width)
                .into_iter()
                .map(|(row, start, end)| {
                    let y = area
                        .y
                        .saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
                    Rect::new(
                        area.x.saturating_add(start),
                        y,
                        end.saturating_sub(start),
                        1,
                    )
                })
                .collect();
        }
        if surface != SurfaceId::Conversation {
            // A noted surface highlights its rows' cells in the span's
            // range: whole rows between, clipped to the range's columns
            // on its first and last row.
            let Some((from, to)) = self.select.span() else {
                return Vec::new();
            };
            let (start, end) = (from.min(to), from.max(to));
            return self
                .select
                .noted_rows(surface)
                .into_iter()
                .enumerate()
                .filter_map(|(at, span)| {
                    if at < start.row || at > end.row {
                        return None;
                    }
                    let from_col = if at == start.row { start.col } else { 0 };
                    let to_col = if at == end.row {
                        end.col.saturating_add(1)
                    } else {
                        u16::MAX
                    };
                    let (x, end) = cells_range(&span.text, span.x, from_col, to_col)?;
                    Some(Rect::new(x, span.y, end.saturating_sub(x), 1))
                })
                .collect();
        }
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

    /// Esc clears a selection shown, before anything it would interrupt.
    /// A press held with no drag yet clears the anchor the same way,
    /// balancing the hold the press took, and Esc still runs: a press
    /// alone never arms an interrupt guard
    /// (`docs/tui.md`, "Selection and copy").
    pub(in crate::app) fn select_key(&mut self, key: &Key) -> Option<Effect> {
        if *key != Key::Esc {
            return None;
        }
        if self.select.span().is_some() {
            self.clear_selection();
            return Some(Effect::None);
        }
        if self.select.held() {
            self.clear_selection();
        }
        None
    }

    /// Clears the selection and drops a copy waiting on dropped pages,
    /// unpinning them; notices a drag held show. The input box's editor
    /// selection goes with it.
    pub(in crate::app) fn clear_selection(&mut self) {
        self.select.clear();
        self.draft.clear_selection();
        self.notices.release();
        self.drop_pending_copy();
    }

    /// Drops a copy waiting on dropped pages with the notice that says it
    /// could not run: the pages failed to load or the link went down.
    pub(in crate::app) fn abandon_copy(&mut self) {
        if self.select.pending.is_some() {
            self.drop_pending_copy();
            self.notices.push("io_failed", COPY_FAILED.to_owned());
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
    pub(in crate::app) fn view_rows(&self, area: Rect) -> (usize, u16, usize) {
        let (top, total) = self.scroll();
        let height = usize::from(area.height);
        let shown = total.min(top.saturating_add(height)).saturating_sub(top);
        let blank = u16::try_from(height.saturating_sub(shown)).unwrap_or(u16::MAX);
        (top, area.y.saturating_add(blank), shown)
    }

    /// The last screen row a selection reaches in `area`: the last shown
    /// row, above "↓ New messages below" when it shows; `None` with no row.
    pub(in crate::app) fn last_row(&self, area: Rect, y0: u16, shown: usize) -> Option<u16> {
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
                        let grown = row.saturating_add(usize::from(cell.row));
                        // A grapheme is covered when the selection meets
                        // its cells [col, col + width): its far edge past
                        // the selection's start, its near edge before its
                        // end.
                        let past_start =
                            grown != from.row || cell.col.saturating_add(cell.width) > from.col;
                        let before_end = grown != to.row || cell.col <= to.col;
                        let covered =
                            grown >= from.row && grown <= to.row && past_start && before_end;
                        (cell.bytes, covered)
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
                        Under::Unplaced => {}
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
