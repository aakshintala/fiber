//! Page-qualified renderer entries, shared failure memo and pending notices.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::spans::{self, FittedLine};
use super::{Decision, Failure, FailureKind, Input};

/// The drawn page, never a caller's current session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PageKey {
    pub(crate) session: contract::SessionId,
    pub(crate) index: usize,
}

impl PageKey {
    /// The home notice key: the empty session with an index no page
    /// takes, so it never equals a real page key.
    pub(crate) fn home() -> Self {
        Self {
            session: contract::SessionId(String::new()),
            index: usize::MAX,
        }
    }
}

type FailKey = (PageKey, String, u64, String, Option<String>);
type EntryKey = (FailKey, u16);

#[derive(Debug, Default)]
struct Pending {
    failures: Vec<(PageKey, String, Failure)>,
    notices: Vec<(String, String)>,
}

/// Successful entries are local; speculative draws share only failure state.
#[derive(Debug)]
pub(crate) struct Cache {
    entries: Mutex<HashMap<EntryKey, Arc<Vec<FittedLine>>>>,
    failed: Arc<Mutex<HashSet<FailKey>>>,
    pending: Arc<Mutex<Pending>>,
    owner: AtomicBool,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
            failed: Arc::default(),
            pending: Arc::default(),
            owner: AtomicBool::new(true),
        }
    }
}

impl Cache {
    /// A non-owning draw cache whose failures survive its disposal.
    pub(crate) fn scratch_sharing_pending(&self) -> Self {
        Self {
            entries: Mutex::default(),
            failed: Arc::clone(&self.failed),
            pending: Arc::clone(&self.pending),
            owner: AtomicBool::new(false),
        }
    }

    /// A landed copy gains authority to clear shared state on reload.
    pub(crate) fn promote(&self) {
        self.owner.store(true, Ordering::Relaxed);
    }

    /// Looks up only the selected winner and its output-relevant half identity.
    #[allow(
        clippy::too_many_arguments,
        reason = "the key and callback describe one bound row"
    )]
    pub(crate) fn lines(
        &self,
        page: PageKey,
        slot: &str,
        extension: &str,
        half: Option<String>,
        input: &Input<'_>,
        width: u16,
        call: impl FnOnce() -> Decision,
    ) -> Option<Arc<Vec<FittedLine>>> {
        let (input_key, flat) = match input {
            Input::LedgerRow(row) => (row.key(slot), row.flat),
            Input::TurnCard(card) => (card.key(slot), card.flat),
            Input::Prompt(prompt) => (prompt.key(slot), prompt.flat),
            Input::Reply(reply) => (reply.key(slot), reply.flat),
            Input::ThinkingLine(line) => (line.key(slot), line.flat),
            Input::GroupLine(line) => (line.key(slot), line.flat),
            Input::Steer(steer) => (steer.key(slot), steer.flat),
            Input::Answers(answers) => (answers.key(slot), answers.flat),
            Input::HandoffBand(band) => (band.key(slot), band.flat),
            Input::ErrorLine(line) => (line.key(slot), line.flat),
            Input::Notice(notice) => (notice.key(slot), notice.flat),
        };
        let key = (page, slot.to_owned(), input_key, extension.to_owned(), half);
        if support::lock(&self.failed).contains(&key) {
            return None;
        }
        let entry = (key.clone(), width);
        if let Some(hit) = support::lock(&self.entries).get(&entry).cloned() {
            return Some(hit);
        }
        crate::work::add(|work| work.renderer_calls += 1);
        match call() {
            Decision::Draw {
                extension: actual,
                lines,
            } => {
                if actual != extension {
                    return None;
                }
                match spans::fit(&lines, width, flat) {
                    Ok(lines) => {
                        let lines = Arc::new(lines);
                        support::lock(&self.entries).insert(entry, Arc::clone(&lines));
                        Some(lines)
                    }
                    Err(error) => {
                        self.fail(
                            key,
                            Failure {
                                kind: FailureKind::HookFailed,
                                extension: actual,
                                message: error.to_string(),
                            },
                        );
                        None
                    }
                }
            }
            Decision::Fail(failure) => {
                if failure.extension == extension {
                    self.fail(key, failure);
                }
                None
            }
            Decision::NotForSession { .. } => None,
        }
    }

    fn fail(&self, key: FailKey, failure: Failure) {
        if support::lock(&self.failed).insert(key.clone()) {
            support::lock(&self.pending)
                .failures
                .push((key.0, key.1, failure));
        }
    }

    /// Selection notices have no page lifetime and are already deduplicated.
    pub(crate) fn queue_notice(&self, notice: (String, String)) {
        support::lock(&self.pending).notices.push(notice);
    }

    /// Only displayed owners may prune shared failures for this exact page.
    pub(crate) fn drop_page(&self, page: PageKey) {
        support::lock(&self.entries).retain(|key, _| key.0.0 != page);
        if self.owner.load(Ordering::Relaxed) {
            support::lock(&self.failed).retain(|key| key.0 != page);
            support::lock(&self.pending)
                .failures
                .retain(|(at, _, _)| *at != page);
        }
    }

    pub(crate) fn drop_entries(&self) {
        support::lock(&self.entries).clear();
    }

    pub(crate) fn clear(&self) {
        self.drop_entries();
        if self.owner.load(Ordering::Relaxed) {
            support::lock(&self.failed).clear();
            *support::lock(&self.pending) = Pending::default();
        }
    }

    pub(crate) fn take_failures(&self) -> Vec<(String, Failure)> {
        std::mem::take(&mut support::lock(&self.pending).failures)
            .into_iter()
            .map(|(_, slot, failure)| (slot, failure))
            .collect()
    }

    pub(crate) fn take_notices(&self) -> Vec<(String, String)> {
        std::mem::take(&mut support::lock(&self.pending).notices)
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
