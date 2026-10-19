//! The per-page cache of a renderer's lines (`docs/tui.md`, "How a TUI
//! extension runs").
//!
//! A renderer runs again only when its input or the width changes: a hit
//! returns the stored lines without calling it, and an error or a wrong
//! shape falls back to the built-in for that item without calling it
//! again until the input changes. The failure is cached against the
//! item's input, so a width change calls nothing for a failed input.
//! Entries live with the paging window and are dropped with their pages.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::spans::{self, FittedLine};
use super::{Failure, Input, RawSpan};

/// One cached item: its page, slot and input hash. The entry adds the
/// width the renderer was given; the failure does not.
type EntryKey = (usize, String, u64, u16);
/// One failed item: its page, slot and input hash.
type FailKey = (usize, String, u64);

/// A renderer's cached lines, one entry per page, slot, input and width.
/// Failures are cached per page, slot and input, and listed once for the
/// fallback notice.
#[derive(Debug, Default)]
pub(crate) struct Cache {
    /// Successful results, by page, slot, input hash and width.
    entries: RefCell<HashMap<EntryKey, Rc<Vec<FittedLine>>>>,
    /// Failed inputs, by page, slot and input hash.
    failed: RefCell<HashSet<FailKey>>,
    /// Failures not yet reported, each with its page and slot.
    pending: RefCell<Vec<(usize, String, Failure)>>,
}

impl Cache {
    /// The renderer's lines for `input` at `width`: a hit returns the
    /// stored lines; a miss calls `call` once and validates what it
    /// returns before storing or drawing any of it. An error or a wrong
    /// shape stores the failure and returns none, so the item falls back
    /// to its built-in row. No borrow crosses the renderer call, so a
    /// renderer that calls back into the cache never panics here.
    pub(crate) fn lines(
        &self,
        page: usize,
        slot: &str,
        input: &Input<'_>,
        width: u16,
        call: impl FnOnce() -> Result<Vec<Vec<RawSpan>>, Failure>,
    ) -> Option<Rc<Vec<FittedLine>>> {
        let hash = match input {
            Input::LedgerRow(row) => row.key(slot),
        };
        if self
            .failed
            .borrow()
            .contains(&(page, slot.to_owned(), hash))
        {
            return None;
        }
        let entry = (page, slot.to_owned(), hash, width);
        if let Some(hit) = self.entries.borrow().get(&entry).cloned() {
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
                    let fitted = Rc::new(fitted);
                    self.entries.borrow_mut().insert(entry, Rc::clone(&fitted));
                    Some(fitted)
                }
                Err(shape) => {
                    self.fail(
                        page,
                        slot,
                        hash,
                        Failure {
                            extension: String::new(),
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
        self.failed
            .borrow_mut()
            .insert((page, slot.to_owned(), hash));
        self.pending
            .borrow_mut()
            .push((page, slot.to_owned(), failure));
    }

    /// Drops the page's entries and failures.
    pub(crate) fn drop_page(&self, page: usize) {
        self.entries.borrow_mut().retain(|key, _| key.0 != page);
        self.failed.borrow_mut().retain(|key| key.0 != page);
        self.pending.borrow_mut().retain(|(at, _, _)| *at != page);
    }

    /// Drops every successful entry at every width and keeps the
    /// failures: a changed page width changes every derived width.
    pub(crate) fn drop_entries(&self) {
        self.entries.borrow_mut().clear();
    }

    /// Drops every entry, failure and listed failure.
    pub(crate) fn clear(&self) {
        self.entries.borrow_mut().clear();
        self.failed.borrow_mut().clear();
        self.pending.borrow_mut().clear();
    }

    /// The failures not yet reported, each with its slot, emptied.
    pub(crate) fn take_failures(&self) -> Vec<(String, Failure)> {
        self.pending
            .borrow_mut()
            .drain(..)
            .map(|(_, slot, failure)| (slot, failure))
            .collect()
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
