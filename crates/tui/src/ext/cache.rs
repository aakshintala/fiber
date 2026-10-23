//! The per-page cache of a renderer's lines (`docs/tui.md`, "How a TUI
//! extension runs").
//!
//! A renderer runs again only when its input or the width changes: a hit
//! returns the stored lines without calling it, and an error or a wrong
//! shape falls back to the built-in for that item without calling it
//! again until the input changes. The failure is cached against the
//! item's input, so a width change calls nothing for a failed input.
//! Entries live with the paging window and are dropped with their pages.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use super::spans::{self, FittedLine};
use super::{Failure, Input, RawSpan};

/// One cached item: its page, slot and input hash. The entry adds the
/// width the renderer was given; the failure does not.
type EntryKey = (usize, String, u64, u16);
/// One failed item: its page, slot and input hash.
type FailKey = (usize, String, u64);

/// A renderer's cached lines, one entry per page, slot, input and width.
/// Failures are cached per page, slot and input, and listed once for the
/// fallback notice. The cache crosses threads behind `Arc` (the paging
/// window shares it with the rows bound to each page), so it holds only
/// `Send` data: keys, spans and failures. No guard crosses a renderer
/// call, so a renderer that calls back into the cache never deadlocks
/// here the way a held `RefCell` borrow would panic.
#[derive(Debug, Default)]
pub(crate) struct Cache {
    inner: Mutex<Inner>,
}

/// What [`Cache`] locks: successful results by page, slot, input hash
/// and width, failed inputs by page, slot and input hash, and failures
/// not yet reported, each with its page and slot.
#[derive(Debug, Default)]
struct Inner {
    /// Successful results, by page, slot, input hash and width.
    entries: HashMap<EntryKey, Arc<Vec<FittedLine>>>,
    /// Failed inputs, by page, slot and input hash.
    failed: HashSet<FailKey>,
    /// Failures not yet reported, each with its page and slot.
    pending: Vec<(usize, String, Failure)>,
}

impl Cache {
    /// The lock, with a poisoned one taken as is: only a panicking
    /// thread poisons it, and a panic aborts the process, so no
    /// poisoned lock is ever observed here.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        support::lock(&self.inner)
    }

    /// The renderer's lines for `input` at `width`: a hit returns the
    /// stored lines; a miss calls `call` once and validates what it
    /// returns before storing or drawing any of it. An error or a wrong
    /// shape stores the failure under `extension` and returns none, so
    /// the item falls back to its built-in row. No lock crosses the
    /// renderer call, so a renderer that calls back into the cache never
    /// deadlocks.
    pub(crate) fn lines(
        &self,
        page: usize,
        slot: &str,
        extension: &str,
        input: &Input<'_>,
        width: u16,
        call: impl FnOnce() -> Result<Vec<Vec<RawSpan>>, Failure>,
    ) -> Option<Arc<Vec<FittedLine>>> {
        let hash = match input {
            Input::LedgerRow(row) => row.key(slot),
        };
        if self.lock().failed.contains(&(page, slot.to_owned(), hash)) {
            return None;
        }
        let entry = (page, slot.to_owned(), hash, width);
        if let Some(hit) = self.lock().entries.get(&entry).cloned() {
            return Some(hit);
        }
        crate::work::add(|work| work.renderer_calls += 1);
        let rendered = call();
        let flat = match input {
            Input::LedgerRow(row) => row.flat,
        };
        match rendered {
            Ok(lines) => match spans::fit(&lines, width, flat) {
                Ok(fitted) => {
                    let fitted = Arc::new(fitted);
                    self.lock().entries.insert(entry, Arc::clone(&fitted));
                    Some(fitted)
                }
                Err(shape) => {
                    self.fail(
                        page,
                        slot,
                        hash,
                        Failure {
                            extension: extension.to_owned(),
                            message: shape.to_string(),
                        },
                    );
                    None
                }
            },
            Err(failure) => {
                self.fail(page, slot, hash, failure);
                None
            }
        }
    }

    /// Records `failure` against the input and lists it for the notice.
    fn fail(&self, page: usize, slot: &str, hash: u64, failure: Failure) {
        let mut inner = self.lock();
        inner.failed.insert((page, slot.to_owned(), hash));
        inner.pending.push((page, slot.to_owned(), failure));
    }

    /// Drops the page's entries and failures.
    pub(crate) fn drop_page(&self, page: usize) {
        let mut inner = self.lock();
        inner.entries.retain(|key, _| key.0 != page);
        inner.failed.retain(|key| key.0 != page);
        inner.pending.retain(|(at, _, _)| *at != page);
    }

    /// Drops every successful entry at every width and keeps the
    /// failures: a changed page width changes every derived width.
    pub(crate) fn drop_entries(&self) {
        self.lock().entries.clear();
    }

    /// Drops every entry, failure and listed failure.
    pub(crate) fn clear(&self) {
        let mut inner = self.lock();
        inner.entries.clear();
        inner.failed.clear();
        inner.pending.clear();
    }

    /// The failures not yet reported, each with its slot, emptied.
    pub(crate) fn take_failures(&self) -> Vec<(String, Failure)> {
        std::mem::take(&mut self.lock().pending)
            .into_iter()
            .map(|(_, slot, failure)| (slot, failure))
            .collect()
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
