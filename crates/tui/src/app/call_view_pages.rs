//! A call view's fetched artifact pages (`docs/tui.md`, "Swapped views"):
//! the page store and fetch queue behind the bounded draw window. The
//! store owns every fetched offset's text and extent; the window in
//! `call_view` decides what to ask for and what to drop, and the rows in
//! `call_view_rows` draw what the store holds.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// One held artifact page: its text, whole lines from its offset.
#[derive(Debug)]
pub(in crate::app) struct Page {
    pub(in crate::app) text: String,
}

/// What a fetched page's extent stays as after its text pages out: the
/// byte offset after it, absent at the end of the file; each line's
/// absolute starting byte, so markers place without the text; and
/// whether its last byte is a newline, which decides whether the next
/// page's first start is a line boundary or a continued fragment.
#[derive(Debug)]
pub(in crate::app) struct Seg {
    pub(in crate::app) lines: Vec<u64>,
    pub(in crate::app) next: Option<u64>,
    pub(in crate::app) ends_newline: bool,
}

/// The fetched pages and the fetch queue: held texts by byte offset,
/// every fetched offset's extent, the next fill offset, the request on
/// the wire, the queued offsets, the failed ones, and the largest file
/// size any page reported.
#[derive(Debug)]
pub(in crate::app) struct Pager {
    pub(in crate::app) pages: BTreeMap<u64, Page>,
    pub(in crate::app) segs: BTreeMap<u64, Seg>,
    pub(in crate::app) fill_next: Option<u64>,
    pub(in crate::app) request: Option<(String, u64)>,
    pub(in crate::app) queued: VecDeque<u64>,
    pub(in crate::app) missing: BTreeSet<u64>,
    pub(in crate::app) size: u64,
}

impl Pager {
    /// An empty store: the forward fill starts at 0 when the result
    /// carries an artifact to page, and stays still otherwise.
    pub(in crate::app) fn new(paged: bool) -> Self {
        Self {
            pages: BTreeMap::new(),
            segs: BTreeMap::new(),
            fill_next: paged.then_some(0),
            request: None,
            queued: VecDeque::new(),
            missing: BTreeSet::new(),
            size: 0,
        }
    }

    /// Asks for `offset` next, unless its page is held, already queued,
    /// or on the wire.
    pub(in crate::app) fn queue(&mut self, offset: u64) {
        if self.pages.contains_key(&offset)
            || self.missing.contains(&offset)
            || self.queued.contains(&offset)
            || self.request.as_ref().is_some_and(|(_, at)| *at == offset)
        {
            return;
        }
        self.queued.push_back(offset);
    }

    /// The fetched offsets from 0 while they chain with no gap: the
    /// pages the full text holds so far.
    pub(in crate::app) fn chain_prefix(&self) -> Vec<u64> {
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

    /// Whether the segments chain from 0 to the end of the file with no
    /// gap: the forward fill reached a page with no `next`.
    pub(in crate::app) fn complete(&self) -> bool {
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

    /// The segment offsets around `offset`: the fetched one before it
    /// and the one its extent names after it, each when present.
    pub(in crate::app) fn neighbors(&self, offset: u64) -> (Option<u64>, Option<u64>) {
        let previous = self.segs.range(..offset).next_back().map(|(at, _)| *at);
        let next = self.segs.get(&offset).and_then(|seg| seg.next);
        (previous, next)
    }
}
