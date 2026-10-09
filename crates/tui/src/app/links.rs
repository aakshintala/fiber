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

impl App {
    /// The links drawn in `area`, in reading order: each markdown link and
    /// each bare URL on a shown resident row, one entry per link with one
    /// rect per row it covers (`docs/tui.md`, "Links").
    pub(crate) fn visible_links(&self, area: Rect) -> Vec<VisibleLink> {
        if area.width == 0 {
            return Vec::new();
        }
        let (top, y0, shown) = self.view_rows(area);
        let Some(last) = self.last_row(area, y0, shown) else {
            return Vec::new();
        };
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
                let count = crate::view::rows(line.clone(), area_width);
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
            at = at.saturating_add(crate::view::rows(line.clone(), area_width));
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
