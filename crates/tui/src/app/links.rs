//! Links on screen (`docs/tui.md`, "Links"): markdown links and bare
//! `http://`/`https://` URLs are handled on click. [`App::visible_links`]
//! is a pure function of the app's state and the area, so the id drawn in
//! a frame finds the same URL on click; a frame made stale by a scroll
//! earlier in the same read opens nothing.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use ratatui::layout::Rect;

use super::{App, Effect};
use crate::mouse::TargetId;

/// Trailing punctuation no URL keeps.
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', '\'', '"'];

/// One link as drawn: what was clicked, the cells it covers, and where it
/// goes. One link wrapped over rows covers one rect per row under one id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisibleLink {
    pub(crate) id: TargetId,
    pub(crate) rects: Vec<Rect>,
    pub(crate) url: String,
}

/// The hash of the destination drawn, so a stale frame never opens
/// another URL (`docs/tui.md`, "Links").
pub(crate) fn link_hash(url: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    hasher.finish()
}

/// The bare `http://` and `https://` URLs `text` holds, as byte ranges:
/// each starts at its scheme and runs to the next whitespace or control,
/// with trailing punctuation and an unbalanced `)` or `]` cut off
/// (`docs/tui.md`, "Links").
pub(crate) fn urls(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(ch) = text.get(at..).and_then(|rest| rest.chars().next()) {
        // A scheme starts here, in any case; anything else moves one
        // char forward, and a multibyte char is never a scheme start.
        let prefix = if text
            .get(at..at.saturating_add("https://".len()))
            .is_some_and(|head| head.eq_ignore_ascii_case("https://"))
        {
            "https://".len()
        } else if text
            .get(at..at.saturating_add("http://".len()))
            .is_some_and(|head| head.eq_ignore_ascii_case("http://"))
        {
            "http://".len()
        } else {
            at = at.saturating_add(ch.len_utf8());
            continue;
        };
        let mut end = at.saturating_add(prefix);
        while let Some(ch) = text.get(end..).and_then(|rest| rest.chars().next()) {
            if ch.is_whitespace() || ch.is_control() {
                break;
            }
            end = end.saturating_add(ch.len_utf8());
        }
        let mut range = at..end;
        trim(&mut range, text);
        if range.end.saturating_sub(range.start) > prefix {
            out.push(range.clone());
            at = range.end;
        } else {
            // `http://` alone is no link; move past its scheme.
            at = at.saturating_add(prefix);
        }
    }
    out
}

/// Cuts trailing punctuation and an unbalanced `)` or `]` off `range`.
fn trim(range: &mut Range<usize>, text: &str) {
    loop {
        let Some(tail) = text
            .get(range.clone())
            .and_then(|url| url.chars().next_back())
        else {
            return;
        };
        if TRAILING.contains(&tail) {
            range.end = range.end.saturating_sub(tail.len_utf8());
            continue;
        }
        if tail == ')' || tail == ']' {
            let open = if tail == ')' { '(' } else { '[' };
            let url = text.get(range.clone()).unwrap_or_default();
            let opens = url.chars().filter(|ch| *ch == open).count();
            let closes = url.chars().filter(|ch| *ch == tail).count();
            if closes > opens {
                range.end = range.end.saturating_sub(1);
                continue;
            }
        }
        return;
    }
}

/// The conversation's layout with the focused link's hint row: where
/// its rows draw and which row the hint reserves, if any. Content
/// (`crate::view`) and targets ([`App::visible_links`]) share it, so the
/// hint never paints over a row whose target opens another URL
/// (`docs/tui.md`, "Links").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HintLayout {
    /// The first conversation row drawn.
    pub(crate) top: usize,
    /// The screen row it draws on.
    pub(crate) y0: u16,
    /// How many conversation rows draw.
    pub(crate) shown: usize,
    /// The reserved row under the focused link's line, if any.
    pub(crate) gap: Option<u16>,
    /// The focused link's entry under its layout cells, when the layout
    /// moved them.
    pub(crate) focus: Option<FocusedLink>,
}

/// The focused link as drawn: its entry keeps the id focus names at its
/// layout cells, so focus sticks across the reserved row's move and
/// Enter, `y` and a click keep its URL (`docs/tui.md`, "Links").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FocusedLink {
    /// The id keyboard focus names.
    pub(crate) id: TargetId,
    /// The destination it opens.
    pub(crate) url: String,
    /// Its cells in the layout, for the entry they rename.
    pub(crate) rects: Vec<Rect>,
    /// The hint row's cells, opening the focused link, when the layout
    /// scrolled them into view.
    pub(crate) extra: Option<Rect>,
}

/// The top row shown, the screen row it draws on and how many rows
/// show, in `area`: like [`App::view_rows`], but clamping the top to
/// the area's own height, so content and targets share the rows the
/// area draws. `view_rows` anchors the top to the screen's
/// conversation height instead, which differs while a frame draws at
/// another size.
fn area_rows(app: &App, area: Rect) -> (usize, u16, usize) {
    let total = app.scroll().1;
    let height = usize::from(area.height);
    let bottom = total.saturating_sub(height);
    let top = app.top().map_or(bottom, |top| top.min(bottom));
    let shown = total.min(top.saturating_add(height)).saturating_sub(top);
    let blank = u16::try_from(height.saturating_sub(shown)).unwrap_or(u16::MAX);
    (top, area.y.saturating_add(blank), shown)
}

/// `rects` moved below `gap` by the hint's reserved row, less the rows
/// it pushes past `bottom`: the conversation draws its rows the same
/// way, so targets sit on their own cells (`docs/tui.md`, "Links").
fn shifted(rects: &[Rect], gap: u16, bottom: u16) -> Vec<Rect> {
    let mut out = rects.to_vec();
    for rect in &mut out {
        if rect.y >= gap {
            rect.y = rect.y.saturating_add(1);
        }
    }
    out.retain(|rect| rect.y < bottom);
    out
}

/// The raw entry matching focus after a scroll freed its hint row:
/// the nearest row with the same destination. The first visible piece
/// may have a different column after a wrapped line scrolls. Same-frame
/// re-resolution only: callers matched the id exactly before scrolling.
fn find_moved(links: &[VisibleLink], fcol: u16, fhash: u64, frow: usize) -> Option<&VisibleLink> {
    links
        .iter()
        .filter(|link| link_hash(&link.url) == fhash)
        .min_by_key(|link| {
            link.rects.first().map_or((usize::MAX, u16::MAX), |first| {
                (usize::from(first.y).abs_diff(frow), first.x.abs_diff(fcol))
            })
        })
}

impl App {
    /// The links drawn in `area`, in reading order: each markdown link and
    /// each bare URL on a shown resident row, one entry per link with one
    /// rect per row it covers (`docs/tui.md`, "Links"). Rows on and below
    /// the focused link's hint row draw one lower, with the conversation's
    /// rows, so hint text and click targets agree.
    pub(crate) fn visible_links(&self, area: Rect) -> Vec<VisibleLink> {
        if area.width == 0 {
            return Vec::new();
        }
        let layout = self.hint_layout(area);
        let Some(last) = self.last_row(area, layout.y0, layout.shown) else {
            return Vec::new();
        };
        let mut out = self.raw_links(area, layout.top, layout.y0, layout.shown, last);
        if let Some(gap) = layout.gap {
            for link in &mut out {
                link.rects = shifted(&link.rects, gap, area.bottom());
            }
            out.retain(|link| !link.rects.is_empty());
        }
        if let Some(focus) = layout.focus {
            // The focused link keeps the id focus names even when the
            // reserved row moved its cells: keyboard focus sticks across
            // the move, and Enter, `y` and a click keep opening the same
            // URL (`docs/tui.md`, "Links").
            for link in &mut out {
                if link.rects == focus.rects {
                    link.id = focus.id;
                }
            }
            if let Some((extra, link)) = focus
                .extra
                .zip(out.iter_mut().find(|link| link.id == focus.id))
            {
                link.rects.push(extra);
            }
        }
        out
    }

    /// The links drawn in `area` with no hint row reserved: what drew
    /// before the hint existed, for views whose content does not move
    /// with it (`docs/tui.md`, "Links").
    pub(crate) fn plain_links(&self, area: Rect) -> Vec<VisibleLink> {
        if area.width == 0 {
            return Vec::new();
        }
        let (top, y0, shown) = area_rows(self, area);
        let Some(last) = self.last_row(area, y0, shown) else {
            return Vec::new();
        };
        self.raw_links(area, top, y0, shown, last)
    }

    /// The conversation's layout in `area` with the focused link's hint
    /// row reserved under its line: rows on and below the row draw one
    /// lower, so the hint never covers conversation cells. With no row
    /// below, the layout uses blank rows above or scrolls enough to free
    /// it, keeping the link on screen; with no room to scroll, only the
    /// focused look paints (`docs/tui.md`, "Links").
    pub(crate) fn hint_layout(&self, area: Rect) -> HintLayout {
        let (top, y0, shown) = area_rows(self, area);
        let plain = HintLayout {
            top,
            y0,
            shown,
            gap: None,
            focus: None,
        };
        let Some(id) = self.focused() else {
            return plain;
        };
        let TargetId::Link { .. } = id else {
            return plain;
        };
        let Some(last) = self.last_row(area, y0, shown) else {
            return plain;
        };
        let raw = self.raw_links(area, top, y0, shown, last);
        let Some(hit) = raw.iter().find(|link| link.id == id) else {
            return plain;
        };
        let Some(rect) = hit.rects.last() else {
            return plain;
        };
        let row = top.saturating_add(usize::from(rect.y.saturating_sub(y0)));
        let Some((_, end)) = self.line_span(row) else {
            return plain;
        };
        let gap = y0.saturating_add(u16::try_from(end.saturating_sub(top)).unwrap_or(u16::MAX));
        // The new-below pill owns the last row; the hint must end before
        // it, just as its click target does (`docs/tui.md`, "Turns").
        let limit = area.bottom().saturating_sub(u16::from(self.has_new()));
        if gap < limit {
            // No `focus` entry: the rows below only shift down one, so
            // the focused link keeps its id at its shifted cells without
            // a rename, and there is no hint-row target to add. Keeping
            // the layout plain here leaves no field no test can catch.
            return HintLayout {
                gap: Some(gap),
                ..plain
            };
        }
        // Reserve the final available row. Use blank rows above first,
        // then scroll the conversation enough to bring a wrapped line's
        // end and the hint into the area. If that loses the focused link,
        // `scrolled_focus` leaves the ordinary focused look in place.
        let shift = gap.saturating_sub(limit.saturating_sub(1));
        let blank = y0.saturating_sub(area.y).min(shift);
        let y0 = y0.saturating_sub(blank);
        let scroll = shift.saturating_sub(blank);
        if row < top.saturating_add(usize::from(scroll)) {
            return plain;
        }
        let top = top.saturating_add(usize::from(scroll));
        let total = self.scroll().1;
        let shown = total
            .min(top.saturating_add(usize::from(area.height)))
            .saturating_sub(top);
        let gap = gap.saturating_sub(shift);
        self.scrolled_focus(area, id, top, y0, shown, gap, &hit.url)
    }

    /// The focused link's entry after a scroll freed its hint row:
    /// re-resolved in the scrolled layout, with the hint row's cells
    /// opening it. Plain when the scroll lost it (`docs/tui.md`,
    /// "Links").
    #[allow(
        clippy::too_many_arguments,
        reason = "a scrolled layout needs its rows and the focused link"
    )]
    fn scrolled_focus(
        &self,
        area: Rect,
        id: TargetId,
        top: usize,
        y0: u16,
        shown: usize,
        gap: u16,
        url: &str,
    ) -> HintLayout {
        let plain = HintLayout {
            top,
            y0,
            shown,
            gap: None,
            focus: None,
        };
        let Some(last) = self.last_row(area, y0, shown) else {
            return plain;
        };
        let raw = self.raw_links(area, top, y0, shown, last);
        let TargetId::Link {
            row: frow,
            col: fcol,
            url: fhash,
        } = id
        else {
            return plain;
        };
        let Some(hit) = find_moved(&raw, fcol, fhash, frow) else {
            return plain;
        };
        let x = hit.rects.first().map_or(area.x, |rect| rect.x);
        let room = area.right().saturating_sub(x);
        let width = u16::try_from(crate::format::width(crate::view::marks::HINT))
            .unwrap_or(u16::MAX)
            .min(room);
        HintLayout {
            gap: Some(gap),
            focus: Some(FocusedLink {
                id,
                url: url.to_owned(),
                rects: shifted(&hit.rects, gap, area.bottom()),
                extra: Some(Rect::new(x, gap, width, 1)),
            }),
            ..plain
        }
    }

    /// The drawn row holding conversation `row`: its first row and one
    /// past its last. `None` when no resident page holds `row`.
    fn line_span(&self, row: usize) -> Option<(usize, usize)> {
        let index = self.screen.pages().index();
        let (page, _) = index.locate(row)?;
        let (rows, _) = self.screen.pages().page_text(page)?;
        let width = self.screen.pages().wrap_width();
        let mut at = index.start(page);
        for (line, _) in &rows {
            let end = at.saturating_add(crate::view::rows(line, width));
            if row < end {
                return Some((at, end));
            }
            at = end;
        }
        None
    }

    /// The links drawn in `area` for the rows `top..top + shown` on
    /// screen rows from `y0`, down to `last`: each markdown link and each
    /// bare URL on a shown resident row, one entry per link with one rect
    /// per row it covers (`docs/tui.md`, "Links").
    #[allow(
        clippy::too_many_arguments,
        reason = "a link's cells need its rows and area"
    )]
    fn raw_links(
        &self,
        area: Rect,
        top: usize,
        y0: u16,
        shown: usize,
        last: u16,
    ) -> Vec<VisibleLink> {
        let end = top.saturating_add(shown);
        // One entry per drawn link occurrence, keyed by its anchor: its
        // first row, its first column there, and its destination's hash.
        // A markdown link wrapped over rows contributes one occurrence per
        // row; consecutive occurrences with the same destination merge into
        // the one link drawn over those rows.
        let mut found: Vec<(usize, u16, String, Rect)> = Vec::new();
        let index = self.screen.pages().index();
        // Links are placed and counted at the width the pages wrap at:
        // the rows draw one column narrower than the column, leaving
        // the last column to the scroll bar.
        let area_width = self.screen.pages().wrap_width();
        // Each resident page once, its rows with their texts as drawn now.
        let mut pages: BTreeMap<usize, (Vec<crate::turn::Row>, Vec<crate::rows::RowText>)> =
            BTreeMap::new();
        for row in top..end {
            let Some((page, _)) = index.locate(row) else {
                continue;
            };
            if !pages.contains_key(&page)
                && let Some(text) = self.screen.pages().page_text(page)
            {
                pages.insert(page, text);
            }
        }
        for (page, (rows, texts)) in &pages {
            let start = index.start(*page);
            let mut grow = start;
            for ((line, _), text) in rows.iter().zip(texts) {
                let count = crate::view::rows(line, area_width);
                let line_end = grow.saturating_add(count);
                let from = grow.max(top);
                let to = line_end.min(end);
                self.line_links(text, from, to, y0, top, last, area, area_width, &mut found);
                grow = line_end;
            }
            // Bare URLs come from the page's logical text, so a URL
            // markdown wrapped over rows stays one whole destination.
            self.bare_links(
                rows, texts, start, top, y0, last, area, area_width, &mut found,
            );
        }
        // Consecutive occurrences with the same destination are the one
        // link wrapped over those rows.
        let mut out: Vec<VisibleLink> = Vec::new();
        for (row, col, url, rect) in found {
            let hash = link_hash(&url);
            let follows = out.last().is_some_and(|last: &VisibleLink| {
                last.url == url
                    && last
                        .rects
                        .last()
                        .is_some_and(|prev| rect.y == prev.y.saturating_add(prev.height))
            });
            if follows && let Some(last) = out.last_mut() {
                last.rects.push(rect);
                continue;
            }
            out.push(VisibleLink {
                id: TargetId::Link {
                    row,
                    col,
                    url: hash,
                },
                rects: vec![rect],
                url,
            });
        }
        out
    }

    /// The links of one drawn `line`: each markdown link's cells on the
    /// visible sub-rows `from..to`.
    #[allow(
        clippy::too_many_arguments,
        reason = "a link's cells need its line, rows and area"
    )]
    fn line_links(
        &self,
        text: &crate::rows::RowText,
        from: usize,
        to: usize,
        y0: u16,
        top: usize,
        last: u16,
        area: Rect,
        area_width: u16,
        found: &mut Vec<(usize, u16, String, Rect)>,
    ) {
        // Markdown links: their columns in the line.
        for (range, url) in &text.links {
            let start = range.start.min(area_width);
            let end = range.end.min(area_width);
            if start >= end {
                continue;
            }
            // Markdown lines fit their width, so the link sits on the
            // line's single visible row. The id names the screen cell it
            // draws on, so a scroll that moves the frame leaves a stale
            // id behind (`docs/tui.md`, "Links").
            for row in from..to {
                let Some(y) = row
                    .checked_sub(top)
                    .and_then(|at| u16::try_from(at).ok())
                    .map(|at| y0.saturating_add(at))
                    .filter(|y| *y <= last)
                else {
                    continue;
                };
                let x = area.x.saturating_add(start);
                let w = end.saturating_sub(start);
                if w == 0 {
                    continue;
                }
                let w = w.min(area.right().saturating_sub(x));
                found.push((usize::from(y), x, url.clone(), Rect::new(x, y, w, 1)));
            }
        }
    }

    /// The bare URLs of one page's logical text: each whole URL, even one
    /// markdown wrapped over rows, mapped onto every drawn row it covers,
    /// less the cells a markdown link covers (`docs/tui.md`, "Links").
    #[allow(
        clippy::too_many_arguments,
        reason = "a link's cells need its rows and area"
    )]
    fn bare_links(
        &self,
        rows: &[crate::turn::Row],
        texts: &[crate::rows::RowText],
        start: usize,
        top: usize,
        y0: u16,
        last: u16,
        area: Rect,
        area_width: u16,
        found: &mut Vec<(usize, u16, String, Rect)>,
    ) {
        // The conversation row each page row's first drawn row holds.
        let mut first: Vec<usize> = Vec::with_capacity(rows.len());
        let mut at = start;
        for (line, _) in rows {
            first.push(at);
            at = at.saturating_add(crate::view::rows(line, area_width));
        }
        // The placed cells of each page row a URL touches, on demand.
        let mut placed: Vec<Option<Vec<crate::cells::Placed>>> = vec![None; rows.len()];
        for logical in crate::logical::logical(rows, texts) {
            // The byte each logical char starts at.
            let bytes: Vec<usize> = logical.text.char_indices().map(|(at, _)| at).collect();
            for range in urls(&logical.text) {
                // The URL's chars back on their drawn cells, by
                // conversation row, less the cells a markdown link
                // covers, so the markdown entry keeps them.
                let mut per_row: BTreeMap<usize, (u16, u16)> = BTreeMap::new();
                for (at, byte) in bytes.iter().enumerate() {
                    let char_end = bytes
                        .get(at.saturating_add(1))
                        .copied()
                        .unwrap_or(logical.text.len());
                    if *byte < range.start || char_end > range.end {
                        continue;
                    }
                    let Some((row_at, offset)) = logical.from.get(at).copied().flatten() else {
                        continue;
                    };
                    if !placed.get(row_at).is_some_and(|slot| slot.is_some())
                        && let Some((line, _)) = rows.get(row_at)
                    {
                        let cells = crate::cells::place(line, area_width);
                        if let Some(slot) = placed.get_mut(row_at) {
                            *slot = Some(cells);
                        }
                    }
                    let Some(Some(cells)) = placed.get(row_at) else {
                        continue;
                    };
                    let Some(cell) = cells
                        .iter()
                        .find(|cell| cell.bytes.start <= offset && offset < cell.bytes.end)
                    else {
                        continue;
                    };
                    if texts.get(row_at).is_some_and(|text| {
                        text.links.iter().any(|(link, _)| link.contains(&cell.col))
                    }) {
                        continue;
                    }
                    let Some(base) = first.get(row_at).copied() else {
                        continue;
                    };
                    let row = base.saturating_add(usize::from(cell.row));
                    let end_col = cell.col.saturating_add(cell.width);
                    per_row
                        .entry(row)
                        .and_modify(|(first_col, last_col)| {
                            *first_col = (*first_col).min(cell.col);
                            *last_col = (*last_col).max(end_col);
                        })
                        .or_insert((cell.col, end_col));
                }
                // One entry per row the URL's uncovered cells draw on.
                for (row, (first_col, last_col)) in per_row {
                    let Some(y) = row
                        .checked_sub(top)
                        .and_then(|at| u16::try_from(at).ok())
                        .map(|at| y0.saturating_add(at))
                        .filter(|y| *y <= last)
                    else {
                        continue;
                    };
                    if last_col <= first_col {
                        continue;
                    }
                    let x = area.x.saturating_add(first_col);
                    let w = last_col
                        .saturating_sub(first_col)
                        .min(area.right().saturating_sub(x));
                    if w == 0 {
                        continue;
                    }
                    let url = logical
                        .text
                        .get(range.clone())
                        .unwrap_or_default()
                        .to_owned();
                    if url.is_empty() {
                        continue;
                    }
                    // A bare URL is a link even when its destination would not
                    // open alone; `follow_link` still checks it. Markdown
                    // validity (`mailto` included) does not apply here.
                    found.push((usize::from(y), x, url, Rect::new(x, y, w, 1)));
                }
            }
        }
    }

    /// A click on `id`: opens its destination, or copies it with "Copied"
    /// when no opener is on `PATH`. A stale id, from a frame a scroll
    /// moved since, does nothing (`docs/tui.md`, "Links").
    pub(in crate::app) fn follow_link(&mut self, id: TargetId) -> Effect {
        let area = self.conversation_area();
        let found = self
            .visible_links(area)
            .into_iter()
            .find(|link| link.id == id);
        let Some(link) = found else {
            return Effect::None;
        };
        if !crate::opener::valid(&link.url) {
            return Effect::None;
        }
        if self.opener {
            Effect::OpenLink(link.url)
        } else {
            self.copied = true;
            Effect::Copy(link.url)
        }
    }

    /// A focused link's URL, for `y` (`docs/tui.md`, "Links").
    pub(in crate::app) fn link_text(&self, id: TargetId) -> Option<String> {
        self.visible_links(self.conversation_area())
            .into_iter()
            .find(|link| link.id == id)
            .map(|link| link.url)
    }

    /// Whether a link opener is on `PATH`.
    pub(crate) fn set_opener(&mut self, can: bool) {
        self.opener = can;
    }
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;
