//! A tool call's swapped view (`docs/tui.md`, "Swapped views"): the whole
//! call as drawn inline with no 20-line cap, its full output paged with
//! `artifact_page`, the model's-view markers from `kept`, `y` copying the
//! whole call, and search scoped to the view.
//!
//! The view never holds the whole file for drawing: it keeps at most
//! three pages around the visible page (the visible page plus one on
//! each side) and refetches a dropped page by its byte `offset` when
//! scrolled back to. The `y` copy pins the pages it needs independently
//! of the draw window and releases them after. A failed page leaves the
//! view open with the held pages and a notice; scrolling to it asks
//! again. Markers sit at the first line boundary at or past each `kept`
//! edge, never splitting a line.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::json;

use super::{App, Effect, Target, mint, session_command};
use crate::diff::gutter_prefix;
use crate::keys::Key;
use crate::mouse::TargetId;
use crate::swapped::{Frame, Ink, List, Spot};
use crate::theme::Role;
use crate::turn::opened::{self, CallSnapshot};
use contract::SessionId;
use contract::events::{CommandAccepted, CommandRejected, CommandResult};

/// The marker where the model's view stops.
const END_MARK: &str = "── end of the model's view ──";
/// The marker where the model's view resumes.
const RESUME_MARK: &str = "── the model's view resumes ──";

/// One held artifact page: its text, whole lines from its offset.
#[derive(Debug)]
struct Page {
    text: String,
}

/// What a fetched page's extent stays as after its text pages out: the
/// byte offset after it, absent at the end of the file; each line's
/// absolute starting byte, so markers place without the text; and
/// whether its last byte is a newline, which decides whether the next
/// page's first start is a line boundary or a continued fragment.
#[derive(Debug)]
struct Seg {
    lines: Vec<u64>,
    next: Option<u64>,
    ends_newline: bool,
}

/// A `y` copy waiting on its pages: the full text needs every offset
/// the segments chain, in file order. Pages stay pinned until the copy
/// lands whole or aborts, then unpin.
#[derive(Debug)]
struct CopyReq;

/// Where one drawn body row comes from: a prefix or closing row by its
/// index, an output line by its page and line, or a marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Prefix(usize),
    Page {
        offset: u64,
        index: usize,
    },
    Closing(usize),
    Marker {
        offset: u64,
        index: usize,
        resume: bool,
    },
}

/// One drawn body row: its cells and where it comes from.
struct Row {
    cells: Vec<(String, Ink)>,
    source: Source,
}

/// A call's swapped view: the call's identity and snapshot, its held
/// pages, and its scroll, copy and search state.
#[derive(Debug)]
pub(crate) struct CallView {
    /// The call's id: Esc returns with its row still focused.
    call: usize,
    /// The capitalised kind word, drawn bold on the top row.
    title: String,
    /// The call's target, drawn dim on the top row.
    command: String,
    /// The headline and shell command rows, drawn before the output.
    prefix: Vec<opened::Body>,
    /// The shell or search closing row, drawn after the output.
    closing: Vec<opened::Body>,
    /// The first output line's number, for numbered output.
    numbering: Option<u64>,
    /// The whole call's uncapped rows, drawn when no artifact pages it.
    plain: Vec<opened::Body>,
    /// Whether the result carries an artifact to page.
    artifact: Option<String>,
    /// The model's-view edges, when the result carries them.
    kept: Option<Vec<[u64; 2]>>,
    /// The session the view opened on: answers from another go nowhere.
    session: SessionId,
    /// Held pages by byte offset.
    pages: BTreeMap<u64, Page>,
    /// Every fetched offset's extent, kept after its text pages out.
    segs: BTreeMap<u64, Seg>,
    /// The next offset the forward fill asks for, from 0 following
    /// `next`; `None` once a page answers at the end of the file.
    fill_next: Option<u64>,
    /// The request on the wire, by its id and offset: one at a time.
    request: Option<(String, u64)>,
    /// Offsets to ask for next, in order, without duplicates.
    queued: VecDeque<u64>,
    /// Offsets whose fetch failed: scrolling one into the window asks
    /// again.
    missing: BTreeSet<u64>,
    /// The failure notice, naming the error code.
    notice: Option<String>,
    /// The largest file size any page reported.
    size: u64,
    /// The scrolled-to row and the first row shown.
    list: List,
    /// Whether new pages pull the view to the bottom, as the
    /// conversation follows new output until the person scrolls up.
    follow: bool,
    /// A `y` copy waiting on its pages, if one runs.
    copy: Option<CopyReq>,
    /// Offsets the running copy pins against the draw window's
    /// eviction.
    pinned: BTreeSet<u64>,
    /// The query the matches below were built for.
    query: String,
    /// The body rows holding the query, in order.
    matches: Vec<usize>,
    /// The current match's index into `matches`.
    current: Option<usize>,
}

impl CallView {
    /// Snapshots `call` for the view: its kind, target, prefix, closing
    /// and numbering from the per-kind renderer, the uncapped rows for
    /// a result with no artifact, and the paging inputs from its
    /// result. A call with no result yet shows its empty rows.
    fn open(call: CallSnapshot, session: SessionId) -> Self {
        let fill_next = if call.artifact.is_some() {
            Some(0)
        } else {
            None
        };
        Self {
            call: call.id,
            title: call.title,
            command: call.command,
            prefix: call.prefix,
            closing: call.closing,
            numbering: call.numbering,
            plain: call.plain,
            artifact: call.artifact,
            kept: call.kept,
            session,
            pages: BTreeMap::new(),
            segs: BTreeMap::new(),
            fill_next,
            request: None,
            queued: VecDeque::new(),
            missing: BTreeSet::new(),
            notice: None,
            size: 0,
            list: List::default(),
            follow: true,
            copy: None,
            pinned: BTreeSet::new(),
            query: String::new(),
            matches: Vec::new(),
            current: None,
        }
    }

    /// Whether the result pages an artifact.
    fn paged(&self) -> bool {
        self.artifact.is_some()
    }
}

/// One text line of a held page: `text` split on newlines holds no
/// trailing empty, so each line pairs with one `lines` entry.
fn page_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

impl CallView {
    /// The view's body rows: prefix rows, held output lines with marker
    /// rows at their anchors, then closing rows. Rows from a result
    /// with no artifact are its uncapped rows whole.
    fn rows(&self) -> Vec<Row> {
        if !self.paged() {
            return self.plain_rows();
        }
        let mut out = Vec::new();
        for (index, body) in self.prefix.iter().enumerate() {
            out.push(Row {
                cells: body_cells(body, 0),
                source: Source::Prefix(index),
            });
        }
        let mut anchors = self.markers();
        anchors.sort_by_key(|(at, _)| *at);
        for offset in self.pages.keys().copied().collect::<Vec<_>>() {
            let Some(page) = self.pages.get(&offset) else {
                continue;
            };
            let lines = page_lines(&page.text);
            for (index, text) in lines.iter().enumerate() {
                while anchors
                    .first()
                    .is_some_and(|(at, _)| *at == (offset, index))
                {
                    let (at, marker) = anchors.remove(0);
                    out.push(marker_row(at.0, at.1, marker));
                }
                let number = self.numbering.map(|start| {
                    start
                        .saturating_add(self.line_before(offset))
                        .saturating_add(index as u64)
                });
                out.push(Row {
                    cells: output_cells(text, number),
                    source: Source::Page { offset, index },
                });
            }
            let extent = self.segs.get(&offset).map_or(0, |seg| seg.lines.len());
            while anchors
                .first()
                .is_some_and(|(at, _)| *at == (offset, extent))
            {
                let (at, resume) = anchors.remove(0);
                out.push(marker_row(at.0, at.1, resume));
            }
        }
        for (index, body) in self.closing.iter().enumerate() {
            out.push(Row {
                cells: body_cells(body, self.gutter()),
                source: Source::Closing(index),
            });
        }
        out
    }

    /// The uncapped rows for a result with no artifact, with their
    /// gutter fitted to the largest number drawn.
    fn plain_rows(&self) -> Vec<Row> {
        let gutter = self
            .plain
            .iter()
            .fold(0usize, |gutter, body| gutter.max(gutter_width(body)));
        self.plain
            .iter()
            .enumerate()
            .map(|(index, body)| Row {
                cells: body_cells(body, gutter),
                source: Source::Prefix(index),
            })
            .collect()
    }

    /// The gutter width the output numbers need: digits of the largest
    /// drawn number, or of the start alone while nothing is held.
    fn gutter(&self) -> usize {
        let Some(start) = self.numbering else {
            return 0;
        };
        let mut width = digits(start);
        for offset in self.pages.keys().copied().collect::<Vec<_>>() {
            let lines = self.segs.get(&offset).map_or(0, |seg| seg.lines.len());
            let last = start
                .saturating_add(self.line_before(offset))
                .saturating_add(lines.saturating_sub(1) as u64);
            width = width.max(digits(last));
        }
        width
    }

    /// How many output lines the fetched pages before `offset` hold:
    /// every segment's line count, which is its starts' count.
    fn line_before(&self, offset: u64) -> u64 {
        self.segs
            .range(..offset)
            .map(|(_, seg)| seg.lines.len() as u64)
            .sum()
    }

    /// The marker anchors the held pages can draw: for each `kept` edge
    /// the first absolute logical line start at or past it, as a page
    /// offset and line index, or the end of the final held fragment at
    /// the end of the file, each with whether the model's view resumes
    /// there. A page's first start equal to its offset is no boundary
    /// when the previous page ends without a newline, so it is skipped;
    /// an edge with no later start stays pending until the pages around
    /// it, or the end of the file, arrive. No markers when the result
    /// carries no `kept`.
    fn markers(&self) -> Vec<((u64, usize), bool)> {
        let Some(kept) = self.kept.as_ref() else {
            return Vec::new();
        };
        let mut edges: Vec<(u64, bool)> = Vec::new();
        for range in kept {
            let [start, end] = *range;
            if start > 0 {
                edges.push((start, true));
            }
            if end < self.size {
                edges.push((end, false));
            }
        }
        edges.sort();
        edges
            .into_iter()
            .filter_map(|(edge, resume)| self.marker_for(edge).map(|at| (at, resume)))
            .collect()
    }

    /// The anchor for the `kept` edge: the first logical line start at
    /// or past it that a held page draws, or the end of the final held
    /// fragment when the file ends with no later start. `None` while
    /// the edge's pages are still missing or paged out.
    fn marker_for(&self, edge: u64) -> Option<(u64, usize)> {
        let mut first = true;
        let mut ends_newline = true;
        for (offset, seg) in &self.segs {
            for (index, start) in seg.lines.iter().enumerate() {
                let continuation = index == 0 && !first && !ends_newline;
                if !continuation && *start >= edge && self.pages.contains_key(offset) {
                    return Some((*offset, index));
                }
                first = false;
            }
            ends_newline = seg.ends_newline;
        }
        if self.complete() {
            let (offset, seg) = self.segs.last_key_value()?;
            if self.pages.contains_key(offset) {
                return Some((*offset, seg.lines.len()));
            }
        }
        None
    }

    /// Whether the segments chain from 0 to the end of the file with no
    /// gap: the forward fill reached a page with no `next`.
    fn complete(&self) -> bool {
        let mut at = 0u64;
        loop {
            let Some(seg) = self.segs.get(&at) else {
                return false;
            };
            match seg.next {
                Some(next) => at = next,
                None => return true,
            }
        }
    }

    /// The view's frame: the capitalised kind bold and its target dim
    /// on the top row, the body rows, and "y copies the whole call" dim
    /// on the last row, with the failure notice above it when a page
    /// failed to load.
    pub(in crate::app) fn frame(&self) -> Frame {
        let mut rows = self.rows();
        self.highlight(&mut rows);
        Frame {
            title: self.title.clone(),
            command: self.command.clone(),
            rows: rows
                .into_iter()
                .map(|row| {
                    row.cells
                        .into_iter()
                        .map(|(text, ink)| (text, None, ink))
                        .collect()
                })
                .collect(),
            list: self.list,
            below: self.notice.iter().cloned().collect(),
            field: None,
            footer: "y copies the whole call".to_owned(),
        }
    }

    /// Restyles matched body rows with the query's hits: every hit on
    /// the match tint, the current match brighter. Only body text ever
    /// matches: markers draw from chrome and never match, as the top
    /// row and footer never do.
    fn highlight(&self, rows: &mut [Row]) {
        if self.query.is_empty() {
            return;
        }
        let current_row = self.current.and_then(|at| self.matches.get(at).copied());
        for (index, row) in rows.iter_mut().enumerate() {
            if !matches!(
                row.source,
                Source::Prefix(_) | Source::Page { .. } | Source::Closing(_)
            ) {
                continue;
            }
            let text: String = row.cells.iter().map(|(text, _)| text.as_str()).collect();
            let hits = crate::logical::matches(&text, &self.query);
            if hits.is_empty() {
                continue;
            }
            let current = Some(index) == current_row;
            row.cells = highlight_cells(&row.cells, &text, &hits, current);
        }
    }
}

/// A body row's cells with the gutter its numbers need.
fn body_cells(body: &opened::Body, gutter: usize) -> Vec<(String, Ink)> {
    let mut cells = body.cells();
    if body.gutter
        && let Some(prefix) = gutter_prefix(body.number, gutter)
    {
        cells.insert(0, (prefix, Ink::Muted));
    }
    cells
}

/// An output line's cells: its number's dim gutter when numbered, else
/// its text plain.
fn output_cells(text: &str, number: Option<u64>) -> Vec<(String, Ink)> {
    match number {
        Some(number) => {
            let width = digits(number);
            let prefix = gutter_prefix(Some(number), width).unwrap_or_default();
            vec![(prefix, Ink::Muted), (text.to_owned(), Ink::Plain)]
        }
        None => vec![(text.to_owned(), Ink::Plain)],
    }
}

/// A marker's row: its text dim across the whole row.
fn marker_row(offset: u64, index: usize, resume: bool) -> Row {
    Row {
        cells: vec![(
            if resume {
                RESUME_MARK.to_owned()
            } else {
                END_MARK.to_owned()
            },
            Ink::Muted,
        )],
        source: Source::Marker {
            offset,
            index,
            resume,
        },
    }
}

/// The digits `number` draws in.
fn digits(number: u64) -> usize {
    number.to_string().len().max(1)
}

/// The gutter width one numbered body needs.
fn gutter_width(body: &opened::Body) -> usize {
    if body.gutter {
        digits(body.number.unwrap_or(0))
    } else {
        0
    }
}

/// The cells' text restyled around the query's char-range hits: hit
/// cells on the match tint, or the current tint for the current match,
/// every other cell keeping its ink. A hit spanning cells marks each
/// part; ranges that end at a cell's start belong to the next cell.
fn highlight_cells(
    cells: &[(String, Ink)],
    text: &str,
    hits: &[std::ops::Range<usize>],
    current: bool,
) -> Vec<(String, Ink)> {
    let chars: Vec<char> = text.chars().collect();
    let mut starts = Vec::with_capacity(chars.len() + 1);
    let mut byte = 0usize;
    for ch in &chars {
        starts.push(byte);
        byte += ch.len_utf8();
    }
    starts.push(byte);
    let byte_of = |char_index: usize| starts.get(char_index).copied().unwrap_or(text.len());
    let ranges: Vec<(usize, usize)> = hits
        .iter()
        .map(|hit| (byte_of(hit.start), byte_of(hit.end)))
        .collect();
    let mark = if current {
        Ink::Marked(Role::MatchCurrent)
    } else {
        Ink::Marked(Role::Match)
    };
    let mut out = Vec::new();
    let mut offset = 0usize;
    for cell in cells {
        let end = offset + cell.0.len();
        // Hit parts inside this cell, clipped to it, in order.
        let mut at = offset;
        for (from, to) in ranges.iter().copied() {
            if to <= offset || from >= end {
                continue;
            }
            let from = from.max(at);
            let to = to.min(end);
            if from < to {
                if at < from {
                    out.push((cell.0[at - offset..from - offset].to_owned(), cell.1));
                }
                out.push((cell.0[from - offset..to - offset].to_owned(), mark));
                at = to;
            }
        }
        if at < end {
            out.push((cell.0[at - offset..].to_owned(), cell.1));
        }
        offset = end;
    }
    out.into_iter()
        .filter(|(text, _)| !text.is_empty())
        .collect()
}

impl CallView {
    /// Asks for `offset` next, unless its page is held, already queued,
    /// or on the wire.
    fn queue(&mut self, offset: u64) {
        if self.pages.contains_key(&offset)
            || self.missing.contains(&offset)
            || self.queued.contains(&offset)
            || self.request.as_ref().is_some_and(|(_, at)| *at == offset)
        {
            return;
        }
        self.queued.push_back(offset);
    }

    /// Queues what the draw window wants: the visible pages, one page
    /// on each side, and the next fill page while following, then drops
    /// pages past the held window.
    fn queue_wanted(&mut self, height: usize) {
        if !self.paged() {
            return;
        }
        let rows = self.rows();
        let (top, shown) = window(&rows, &self.list, self.below_len(), height);
        let mut wanted = BTreeSet::new();
        for row in rows.iter().skip(top).take(shown) {
            if let Source::Page { offset, .. } = row.source {
                wanted.insert(offset);
            }
        }
        for offset in wanted.clone() {
            if let Some(previous) = self.segs.range(..offset).next_back().map(|(at, _)| *at) {
                wanted.insert(previous);
            }
            if let Some(next) = self.segs.get(&offset).and_then(|seg| seg.next) {
                wanted.insert(next);
            }
        }
        if self.follow
            && let Some(next) = self.fill_next
        {
            wanted.insert(next);
        }
        if self.copy.is_some() {
            wanted.extend(self.chain_prefix());
            if let Some(next) = self.fill_next {
                wanted.insert(next);
            }
        }
        let mut wanted: Vec<u64> = wanted.into_iter().collect();
        wanted.sort();
        for offset in wanted {
            self.queue(offset);
        }
        self.evict(&rows, top, shown);
    }

    /// How many body rows the notice leaves out of `height`.
    fn below_len(&self) -> usize {
        self.notice.iter().len()
    }

    /// Drops held pages past the window around the visible rows: the
    /// visible pages plus one on each side stay, pinned pages stay,
    /// everything else pages out and refetches by byte offset when
    /// scrolled back to. Segments stay, so markers and the fill chain
    /// never lose their extents.
    fn evict(&mut self, rows: &[Row], top: usize, shown: usize) {
        let top_source = rows.get(top).map(|row| row.source);
        let selected_source = rows.get(self.list.selected()).map(|row| row.source);
        let order: Vec<u64> = self.segs.keys().copied().collect();
        if order.is_empty() {
            return;
        }
        let mut visible: Vec<usize> = rows
            .iter()
            .skip(top)
            .take(shown)
            .filter_map(|row| match row.source {
                Source::Page { offset, .. } => order.iter().position(|at| *at == offset),
                Source::Prefix(_) | Source::Closing(_) | Source::Marker { .. } => None,
            })
            .collect();
        if visible.is_empty() {
            return;
        }
        visible.sort();
        let low = visible.first().copied().unwrap_or(0).saturating_sub(1);
        let high = visible.last().copied().unwrap_or(0).saturating_add(1);
        let keep: BTreeSet<u64> = order
            .into_iter()
            .enumerate()
            .filter(|(index, _)| *index >= low && *index <= high)
            .map(|(_, offset)| offset)
            .collect();
        self.pages
            .retain(|offset, _| keep.contains(offset) || self.pinned.contains(offset));
        let rows = self.rows();
        let position = |source: Option<Source>| {
            source.and_then(|source| rows.iter().position(|row| row.source == source))
        };
        self.list.rebase(
            position(top_source).unwrap_or(top.min(rows.len().saturating_sub(1))),
            position(selected_source)
                .unwrap_or_else(|| self.list.selected().min(rows.len().saturating_sub(1))),
            rows.len(),
            shown,
        );
    }

    /// Lands a fetched page: holds its text, records its extent, pulls
    /// a following view to the bottom, and drops pages past the window.
    /// A page the running copy needs stays pinned until the copy lands
    /// whole or aborts.
    fn land(
        &mut self,
        offset: u64,
        text: String,
        lines: Vec<u64>,
        next: Option<u64>,
        size: u64,
        height: usize,
    ) {
        let ends_newline = text.ends_with('\n');
        self.size = self.size.max(size);
        self.segs.insert(
            offset,
            Seg {
                lines,
                next,
                ends_newline,
            },
        );
        self.pages.insert(offset, Page { text });
        self.missing.remove(&offset);
        if offset == self.fill_next.unwrap_or(u64::MAX) {
            self.fill_next = next;
        }
        if self.copy.is_some() {
            self.pinned.insert(offset);
        }
        self.notice = None;
        if self.follow {
            let rows = self.rows();
            let len = rows.len();
            self.list.select(
                len.saturating_sub(1),
                len,
                shown_height(height, self.below_len()),
            );
        }
        let rows = self.rows();
        let (top, shown) = window(&rows, &self.list, self.below_len(), height);
        self.evict(&rows, top, shown);
    }

    /// Fails a fetch: the offset stays missing until scrolled to, the
    /// view keeps the pages it holds with a notice naming the error
    /// code, and a running copy aborts without writing partial text.
    fn fail(&mut self, offset: u64, code: &str) {
        self.missing.insert(offset);
        if self.copy.take().is_some() {
            self.pinned.clear();
        }
        self.notice = Some(self.failure_notice(offset, code));
    }

    /// The failure notice: the offset that failed and the error code.
    fn failure_notice(&self, offset: u64, code: &str) -> String {
        format!("The call's output page at byte {offset} could not load: {code}.")
    }

    /// The fetched offsets from 0 while they chain with no gap: the
    /// pages the full text holds so far.
    fn chain_prefix(&self) -> Vec<u64> {
        let mut out = Vec::new();
        let mut at = 0u64;
        while let Some(seg) = self.segs.get(&at) {
            out.push(at);
            let Some(next) = seg.next else {
                break;
            };
            at = next;
        }
        out
    }

    /// The whole call's text for a result with no artifact: its uncapped
    /// rows joined, with no gutter or line numbers.
    fn plain_text(&self) -> String {
        self.plain
            .iter()
            .map(|body| body.text())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The rows one frame draws in `height`: the top row, the notice rows
/// and the footer leave this many body rows.
fn shown_height(height: usize, below: usize) -> usize {
    height.saturating_sub(2 + below).max(1)
}

/// The visible body rows: the first row shown and how many show in
/// `height` less the top row, the notice rows and the footer.
fn window(rows: &[Row], list: &List, below: usize, height: usize) -> (usize, usize) {
    let top = list.top().min(rows.len().saturating_sub(1));
    (top, shown_height(height, below))
}

impl App {
    /// Whether the call's view is open.
    pub(in crate::app) fn call_view_open(&self) -> bool {
        self.call_view().is_some()
    }

    /// Opens the call's view: Enter on its opened row or its more row,
    /// or a click on the more row. The call stays open behind the view;
    /// Esc returns with its row still focused.
    pub(in crate::app) fn open_call_view(&mut self, id: usize) -> Effect {
        let Some(session) = self.session().cloned() else {
            return Effect::None;
        };
        let Some(call) = self.find_call(id) else {
            return Effect::None;
        };
        let mut view = CallView::open(call, session);
        if !self.connected() {
            view.notice = Some("The call's output could not load: not connected.".to_owned());
        }
        self.set_call_view(view);
        self.settle();
        self.call_view_pump_effect()
    }

    /// The call `id` names on the attached screen, snapshotted for its
    /// view, if it drew.
    fn find_call(&self, id: usize) -> Option<CallSnapshot> {
        let calls = self
            .screen
            .pages()
            .newest_first()
            .flatten()
            .flat_map(|turns| turns.iter())
            .flat_map(|turn| turn.groups())
            .flat_map(|group| group.calls());
        opened::snapshot(calls, id)
    }

    /// Whether the call `id` names is open inline.
    pub(in crate::app) fn call_open(&self, id: usize) -> bool {
        self.find_call(id).is_some_and(|call| call.open)
    }

    /// Handles a click on the open call view: its top row closes it,
    /// and a body row selects it.
    pub(in crate::app) fn call_view_click(&mut self, spot: &Spot) -> Effect {
        let height = self.conversation_height();
        let Some(view) = self.call_view_mut() else {
            return Effect::None;
        };
        if let Spot::Row(at) = spot {
            let len = view.rows().len();
            view.list.select(*at, len, height.max(1));
        }
        self.settle();
        Effect::None
    }

    /// Hands `key` to the open call view. `None` with no view open, and
    /// for Ctrl+C.
    pub(in crate::app) fn call_view_key(&mut self, key: &Key) -> Option<Effect> {
        if self.call_view_mut().is_none() || *key == Key::CtrlC {
            return None;
        }
        if *key == Key::Esc && self.find_open() {
            return self.find_key(key);
        }
        if self.find_open() && matches!(key, Key::Char(_) | Key::Backspace) {
            return self.find_key(key);
        }
        match key {
            Key::Esc => {
                self.close_call_view();
                Some(Effect::None)
            }
            // With the find bar open the arrows move between the view's
            // matches, wrapping at either end, as Enter does.
            Key::Enter | Key::Up | Key::Down if self.find_open() => {
                self.step_call_view_match(key);
                Some(Effect::None)
            }
            // Ctrl+F reaches the find bar, opening it or typing through
            // it; a second Ctrl+F stays swallowed there, so the results
            // view never leaks conversation matches over the view.
            Key::CtrlF => None,
            Key::Up => Some(self.scroll_call_view(true, false)),
            Key::Down => Some(self.scroll_call_view(false, false)),
            Key::PageUp => Some(self.scroll_call_view(true, true)),
            Key::PageDown => Some(self.scroll_call_view(false, true)),
            Key::Char('y') => Some(self.start_call_copy()),
            // Every other key is swallowed: the conversation behind the
            // view takes no input while it is open.
            Key::Char(_)
            | Key::Backspace
            | Key::Enter
            | Key::CtrlC
            | Key::CtrlO
            | Key::End
            | Key::AltA
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlG
            | Key::CtrlR
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_)
            | Key::CtrlV
            | Key::CtrlL => Some(Effect::None),
        }
    }

    /// Closes the call view, leaving the call open behind it, with the
    /// call's row still focused.
    pub(in crate::app) fn close_call_view(&mut self) {
        let call = self.call_view_mut().map(|view| view.call);
        self.close_session_view();
        if let Some(call) = call {
            self.focus = Some(TargetId::Line(Target::Call(call)));
        }
        self.settle();
    }

    /// Scrolls the open view by one row or one page, retrying visible
    /// missing pages and dropping pages past the held window, then
    /// pumps the fetch queue.
    fn scroll_call_view(&mut self, up: bool, page: bool) -> Effect {
        let height = self.conversation_height();
        let key = if page {
            if up { Key::PageUp } else { Key::PageDown }
        } else if up {
            Key::Up
        } else {
            Key::Down
        };
        {
            let Some(view) = self.call_view_mut() else {
                return Effect::None;
            };
            let rows = view.rows();
            let len = rows.len();
            let shown = shown_height(height, view.below_len());
            let (top, _) = window(&rows, &view.list, view.below_len(), height);
            let before = view.list.selected();
            view.list.key(&key, len, shown);
            if view.list.selected() != before {
                view.follow = !up && view.list.selected() + 1 >= len;
            } else if !up {
                view.follow = true;
            }
            if up {
                view.follow = false;
            }
            // Scrolling a missing page into the window asks again, as
            // does scrolling down to the end it stalls.
            for row in rows.iter().skip(top).take(shown) {
                if let Source::Page { offset, .. } = row.source
                    && view.missing.remove(&offset)
                {
                    view.queue(offset);
                }
            }
            let at_bottom = !rows.is_empty() && view.list.selected() + 1 >= rows.len();
            if !up
                && at_bottom
                && let Some(next) = view.fill_next
                && view.missing.remove(&next)
            {
                view.queue(next);
            }
        }
        self.settle();
        self.call_view_pump_effect()
    }

    /// Fetches the view's queued offsets, one request on the wire, and
    /// the next fill page while following.
    fn call_view_pump_effect(&mut self) -> Effect {
        match self.call_view_pump() {
            Some(line) => Effect::Send(vec![line]),
            None => Effect::None,
        }
    }

    /// Queues what the window wants and sends the next request, if any.
    /// `None` with nothing to send or no connection.
    fn call_view_pump(&mut self) -> Option<String> {
        if !self.connected() {
            return None;
        }
        let height = self.conversation_height();
        let session = self.session().cloned()?;
        let line = {
            let view = self.call_view_mut()?;
            if view.request.is_some() {
                return None;
            }
            view.queue_wanted(height);
            let offset = view.queued.pop_front()?;
            let path = view.artifact.clone()?;
            let id = mint();
            view.request = Some((id.clone(), offset));
            session_command(
                &id,
                "artifact_page",
                &session,
                Some(json!({"path": path, "offset": offset})),
            )
            .to_string()
        };
        Some(line)
    }

    /// A session answer that may answer the view's fetch: it folds into
    /// the view, never into the pages. The follow-up fetch lines go
    /// out; a finished `y` copy drains through `take_call_copy`.
    pub(in crate::app) fn call_view_answered(&mut self, accepted: &CommandAccepted) -> Vec<String> {
        let id = accepted.command_id.0.clone();
        let session = self.session().cloned();
        let height = self.conversation_height();
        let Some(view) = self.call_view_mut() else {
            return Vec::new();
        };
        let Some((request, offset)) = view.request.clone() else {
            return Vec::new();
        };
        if request != id || Some(&view.session) != session.as_ref() {
            return Vec::new();
        }
        view.request = None;
        let Some(CommandResult::ArtifactPage {
            text,
            lines,
            next,
            size,
        }) = accepted.result.clone()
        else {
            view.fail(offset, "unexpected answer");
            return Vec::new();
        };
        view.land(offset, text, lines, next, size, height);
        self.settle();
        self.refresh_call_view_matches();
        self.call_view_pump().into_iter().collect()
    }

    /// A session rejection that may refuse the view's fetch: the view
    /// stays open with the pages it holds and a notice naming the
    /// error code. A running copy aborts without writing partial text.
    pub(in crate::app) fn call_view_refused(&mut self, rejected: &CommandRejected) {
        let Some(id) = rejected.command_id.as_ref() else {
            return;
        };
        let code = serde_json::to_value(&rejected.code)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "failed".to_owned());
        let Some(view) = self.call_view_mut() else {
            return;
        };
        let Some((request, offset)) = view.request.clone() else {
            return;
        };
        if request != id.0 {
            return;
        }
        view.request = None;
        view.fail(offset, &code);
        self.settle();
    }

    /// The link went down: the request on the wire is gone, so a
    /// scroll asks again, and a running copy ends with the pages it
    /// holds and no clipboard write.
    pub(in crate::app) fn call_view_lost(&mut self) {
        if let Some(view) = self.call_view_mut() {
            view.request = None;
            if view.copy.take().is_some() {
                view.pinned.clear();
            }
        }
    }

    /// Starts copying the whole call's output text: pages already held
    /// copy at once, and missing pages fetch one after another first.
    /// A needed page that failed to load aborts with the error-code
    /// notice instead of a partial write.
    fn start_call_copy(&mut self) -> Effect {
        {
            let Some(view) = self.call_view_mut() else {
                return Effect::None;
            };
            if view.copy.is_some() {
                return Effect::None;
            }
            if !view.paged() {
                let text = view.plain_text();
                self.copied = true;
                return Effect::Copy(text);
            }
            // A needed page that failed to load leaves the standing
            // error-code notice; scrolling it back in retries, and then
            // `y` copies whole.
            if !view.missing.is_empty() {
                return Effect::None;
            }
            for offset in view.chain_prefix() {
                if view.pages.contains_key(&offset) {
                    view.pinned.insert(offset);
                }
            }
            view.copy = Some(CopyReq);
        }
        self.settle();
        match self.take_call_copy() {
            Some(text) => Effect::Copy(text),
            None => self.call_view_pump_effect(),
        }
    }

    /// The text of a `y` copy whose every page is held: the pages in
    /// file order, exactly as returned, with no gutter or line numbers.
    /// Unpins its pages either way. `None` while pages are missing or
    /// no copy runs.
    pub(crate) fn take_call_copy(&mut self) -> Option<String> {
        let text = {
            let view = self.call_view_mut()?;
            if view.copy.is_none() || !view.complete() {
                return None;
            }
            let chain = view.chain_prefix();
            if !chain.iter().all(|offset| view.pages.contains_key(offset)) {
                return None;
            }
            chain
                .iter()
                .filter_map(|offset| view.pages.get(offset))
                .map(|page| page.text.as_str())
                .collect::<String>()
        };
        if let Some(view) = self.call_view_mut() {
            view.copy = None;
            view.pinned.clear();
        }
        self.copied = true;
        Some(text)
    }

    /// Rebuilds the view's matches for the find bar's query: body rows
    /// only, never chrome rows and never the conversation. The current
    /// match stays on its row when it still matches, else moves to the
    /// first match.
    pub(in crate::app) fn refresh_call_view_matches(&mut self) {
        let query = self.find.query().to_owned();
        let Some(view) = self.call_view_mut() else {
            return;
        };
        let rows = view.rows();
        let current_row = view.current.and_then(|at| view.matches.get(at).copied());
        if query != view.query {
            view.query = query.clone();
            view.current = None;
        }
        if query.is_empty() {
            view.matches.clear();
            view.current = None;
            return;
        }
        view.matches = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                matches!(
                    row.source,
                    Source::Prefix(_) | Source::Page { .. } | Source::Closing(_)
                ) && !crate::logical::matches(&row_text(&row.cells), &query).is_empty()
            })
            .map(|(index, _)| index)
            .collect();
        view.current = current_row
            .and_then(|row| view.matches.iter().position(|at| *at == row))
            .or(if view.matches.is_empty() {
                None
            } else {
                Some(0)
            });
    }

    /// Moves the current match between view matches, wrapping at either
    /// end, and scrolls it into view: Enter and the arrows while the
    /// find bar is open.
    pub(in crate::app) fn step_call_view_match(&mut self, key: &Key) {
        let height = self.conversation_height();
        let Some(view) = self.call_view_mut() else {
            return;
        };
        if view.matches.is_empty() {
            return;
        }
        let down = !matches!(key, Key::Up);
        let next = match view.current {
            Some(at) if down => (at + 1) % view.matches.len(),
            Some(at) => (at + view.matches.len() - 1) % view.matches.len(),
            None => 0,
        };
        view.current = Some(next);
        let Some(&row) = view.matches.get(next) else {
            return;
        };
        let len = view.rows().len();
        view.list
            .select(row, len, shown_height(height, view.below_len()));
        self.settle();
    }

    /// The find bar's count while the view is open: the view's matches.
    pub(in crate::app) fn call_view_count(&self) -> Option<String> {
        match self.call_view() {
            Some(view) => {
                if view.query.is_empty() {
                    return None;
                }
                if view.matches.is_empty() {
                    return Some("no matches".to_owned());
                }
                let current = view.current.map(|at| at + 1).unwrap_or(0);
                Some(format!("{current} of {}", view.matches.len()))
            }
            _ => None,
        }
    }
}

/// One row's whole text, cells joined.
fn row_text(cells: &[(String, Ink)]) -> String {
    cells.iter().map(|(text, _)| text.as_str()).collect()
}

#[cfg(test)]
#[path = "call_view_tests.rs"]
mod tests;
