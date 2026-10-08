//! What a match is in a page's text (`docs/tui.md`, "Search"): match
//! identity, the display snippets the results view shows, and the scanners
//! that find them. Pure functions of page text: nothing here reads `App`
//! or `Find`.

use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use crate::app::Target;
use crate::logical::{self, Logical};
use crate::rows::RowText;
use crate::turn::Row;
use crate::window::Pages;

/// A match's identity: its page, the sections around it, and its whole
/// logical line's hash and length with its char range in it. The row is
/// never compared: a rescan finds the match again wherever it moved
/// (`docs/tui.md`, "History and paging": row counts are exact, but rows
/// move).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Anchor {
    /// The page holding the match.
    pub(crate) page: usize,
    /// The sections around its line, outermost first.
    pub(crate) scopes: Vec<Target>,
    /// Its whole logical line's hash.
    pub(crate) line_hash: u64,
    /// Its whole logical line's char length.
    pub(crate) line_len: usize,
    /// Its char range in that whole line.
    pub(crate) at: Range<usize>,
}

impl Anchor {
    /// A match's identity from its page, its whole logical line and its
    /// char range in it: the row is never compared, so a new width never
    /// invalidates it (`docs/tui.md`, "Search").
    pub(crate) fn new(page: usize, line: &Logical, at: Range<usize>) -> Anchor {
        Anchor {
            page,
            scopes: line.scopes.clone(),
            line_hash: line_hash(&line.text),
            line_len: line.text.chars().count(),
            at,
        }
    }
}

/// How many characters of each snippet line the results view keeps:
/// the view shows every match with the lines around it and needs no
/// page (`docs/tui.md`, "Search").
const SNIPPET: usize = 400;

/// One match's display lines for the results view: its logical line cut
/// to [`SNIPPET`] characters around the match, with its char range in the
/// cut line, and one logical line each side, cut to [`SNIPPET`]
/// characters. Display only, never compared: the anchor holds the whole
/// line's hash and length (`docs/tui.md`, "Search").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Snippet {
    /// The logical line before the match's, or none at a page's edge.
    pub(crate) before: String,
    /// The match's logical line, cut around the match.
    pub(crate) line: String,
    /// The match's char range in the cut line.
    pub(crate) at: Range<usize>,
    /// The logical line after the match's, or none at a page's edge.
    pub(crate) after: String,
}

/// One logical line's hash: what identifies it across rescans.
pub(super) fn line_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// The chars a match covers: each one's page row and its byte offset in
/// that row's drawn text. A char the draw dropped, like the space a soft
/// wrap took, covers nothing.
pub(super) fn hit_chars(line: &Logical, hit: &Range<usize>) -> Vec<(usize, usize)> {
    line.text
        .chars()
        .zip(&line.from)
        .enumerate()
        .filter(|(at, _)| hit.contains(at))
        .filter_map(|(_, (_, from))| *from)
        .collect()
}

/// A shown line's identity to its conversation rows, in order, and each
/// drawn section target to its own line's row.
type Placed = (
    HashMap<(u64, usize, Vec<Target>), VecDeque<usize>>,
    HashMap<Target, usize>,
);

/// Page `at`'s shown lines' row map with its first conversation row:
/// each shown logical line's identity to its rows in order, and each
/// drawn section target to its own line's row. `None` when the page
/// is dropped (`docs/tui.md`, "Search": marks follow whatever is
/// drawn).
fn placed_for(pages: &Pages, at: usize) -> Option<(Placed, usize)> {
    let width = pages.wrap_width();
    let (rows, texts) = pages.page_text(at)?;
    let start = pages.index().start(at);
    let shown_offsets = row_offsets(&rows, width);
    let mut placed: Placed = (HashMap::new(), HashMap::new());
    for line in logical::logical(&rows, &texts) {
        placed
            .0
            .entry((
                line_hash(&line.text),
                line.text.chars().count(),
                line.scopes.clone(),
            ))
            .or_default()
            .push_back(start.saturating_add(shown_offsets.get(line.row).copied().unwrap_or(0)));
    }
    for (idx, (_, target)) in rows.iter().enumerate() {
        if let Some(target) = target {
            placed
                .1
                .entry(*target)
                .or_insert(start.saturating_add(shown_offsets.get(idx).copied().unwrap_or(0)));
        }
    }
    Some((placed, start))
}

/// Every match of `query` on resident page `at`, each with its
/// conversation row and whether a closed section hides it. `None` when
/// the page has no open text or is dropped: the caller records nothing,
/// and the page scans again when its revision moves.
pub(super) fn resident(
    pages: &Pages,
    at: usize,
    query: &str,
) -> Option<Vec<(Anchor, usize, bool, Snippet)>> {
    let (open_rows, open_texts) = pages.page_text_open(at)?;
    let (mut placed, start) = placed_for(pages, at)?;
    Some(search_shown(
        at,
        &logical::logical(&open_rows, &open_texts),
        query,
        &mut placed,
        start,
    ))
}

/// Every match of `query` in the lines folded for dropped page `at`,
/// each with its row in the scratch draw and never hidden: the reveal
/// expands and scrolls once the page loads.
pub(super) fn scratch(
    pages: &Pages,
    at: usize,
    rows: &[Row],
    texts: &[RowText],
    query: &str,
) -> Vec<(Anchor, usize, bool, Snippet)> {
    let offsets = row_offsets(rows, pages.wrap_width());
    let start = pages.index().start(at);
    search_scratch(at, &logical::logical(rows, texts), &offsets, query, start)
}

/// Cuts `text` to [`SNIPPET`] characters around `hit`, its char range:
/// the cut text with `hit` relative to it. A short line stays whole, so
/// the results view needs no page (`docs/tui.md`, "Search").
fn cut_around(text: &str, hit: &Range<usize>) -> (String, Range<usize>) {
    let len = text.chars().count();
    if len <= SNIPPET {
        return (text.to_owned(), hit.clone());
    }
    let span = hit.end.saturating_sub(hit.start);
    let start = hit
        .start
        .saturating_sub(SNIPPET.saturating_sub(span) / 2)
        .min(len.saturating_sub(SNIPPET));
    let cut: String = text.chars().skip(start).take(SNIPPET).collect();
    (
        cut,
        hit.start.saturating_sub(start)..hit.end.saturating_sub(start),
    )
}

/// Cuts `text` to [`SNIPPET`] characters from its start: a neighbour
/// line has no match to centre on (`docs/tui.md`, "Search").
fn cut_head(text: &str) -> String {
    text.chars().take(SNIPPET).collect()
}

/// The display snippet for the match `hit` on `line`: its line cut
/// around the match, one logical line each side cut from its start. A
/// neighbour at a page's edge is none: the scan renders one page at a
/// time and fetches nothing extra for a snippet (`docs/tui.md`,
/// "Search").
fn snippet_for(
    line: &Logical,
    hit: &Range<usize>,
    before: Option<&str>,
    after: Option<&str>,
) -> Snippet {
    let (cut, rel) = cut_around(&line.text, hit);
    Snippet {
        before: before.map(cut_head).unwrap_or_default(),
        line: cut,
        at: rel,
        after: after.map(cut_head).unwrap_or_default(),
    }
}

/// The neighbouring lines' text around `lines[at]`, or none at a page's
/// edge: the scan renders one page at a time and fetches nothing extra
/// for a snippet (`docs/tui.md`, "Search").
fn sides(lines: &[Logical], at: usize) -> (Option<&str>, Option<&str>) {
    (
        at.checked_sub(1)
            .and_then(|prev| lines.get(prev))
            .map(|line| line.text.as_str()),
        lines
            .get(at.saturating_add(1))
            .map(|line| line.text.as_str()),
    )
}

/// Every match of `query` in the all-open `lines` of a resident page, each
/// with its conversation row: a shown line maps to its row, and a line a
/// closed section hides counts from its outermost closed scope's own line,
/// the innermost drawn section around it.
fn search_shown(
    page: usize,
    lines: &[Logical],
    query: &str,
    placed: &mut Placed,
    start: usize,
) -> Vec<(Anchor, usize, bool, Snippet)> {
    let mut out = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        let (before, after) = sides(lines, at);
        let hash = line_hash(&line.text);
        let len = line.text.chars().count();
        for hit in logical::matches(&line.text, query) {
            let snippet = snippet_for(line, &hit, before, after);
            let anchor = Anchor::new(page, line, hit);
            let (row, hidden) = match placed
                .0
                .get_mut(&(hash, len, line.scopes.clone()))
                .and_then(VecDeque::pop_front)
            {
                Some(row) => (row, false),
                None => (
                    line.scopes
                        .iter()
                        .rev()
                        .find_map(|scope| placed.1.get(scope))
                        .copied()
                        .unwrap_or(start),
                    true,
                ),
            };
            out.push((anchor, row, hidden, snippet));
        }
    }
    out
}

/// Every match of `query` in the all-open `lines` folded for a dropped
/// page, each with its row in the scratch draw and never hidden: the
/// reveal expands and scrolls once the page loads.
fn search_scratch(
    page: usize,
    lines: &[Logical],
    offsets: &[usize],
    query: &str,
    start: usize,
) -> Vec<(Anchor, usize, bool, Snippet)> {
    let mut out = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        let (before, after) = sides(lines, at);
        for hit in logical::matches(&line.text, query) {
            let snippet = snippet_for(line, &hit, before, after);
            out.push((
                Anchor::new(page, line, hit),
                start.saturating_add(offsets.get(line.row).copied().unwrap_or(0)),
                false,
                snippet,
            ));
        }
    }
    out
}

/// A page's lines' conversation-row offsets: each line's rows from the
/// page's first row, wrapping at `width`.
pub(super) fn row_offsets(rows: &[Row], width: u16) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(rows.len());
    let mut row = 0usize;
    for (line, _) in rows {
        offsets.push(row);
        row = row.saturating_add(crate::view::rows(line.clone(), width));
    }
    offsets
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
