//! Searching past sessions (`docs/tools.md`, "Searching past sessions"):
//! one raw pass per log with the ripgrep crates, no index, nothing written.

mod fields;
mod read;
mod session;
mod text;

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use contract::SessionId;
use contract::session_search::{Found, Hit, Label};

/// The most problems a search lists; the rest are counted.
const PROBLEMS: usize = 20;

/// What one search keeps while it runs: the best `limit` hits, the count of
/// every hit, and the first [`PROBLEMS`] problems.
pub(super) struct Collect {
    /// The most hits kept.
    limit: usize,
    /// The kept hits, the worst on top.
    heap: BinaryHeap<Ranked>,
    /// Every hit found.
    total: u64,
    /// The problems kept.
    problems: Vec<String>,
    /// The problems past [`PROBLEMS`].
    more_problems: u64,
}

impl Collect {
    /// Keeps at most `limit` hits.
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: BinaryHeap::new(),
            total: 0,
            problems: Vec::new(),
            more_problems: 0,
        }
    }

    /// Counts `hit`, and keeps it while it is among the best `limit`.
    pub(super) fn hit(&mut self, hit: Hit) {
        self.total += 1;
        self.heap.push(Ranked(hit));
        if self.heap.len() > self.limit {
            self.heap.pop();
        }
    }

    /// Lists `problem`, or counts it once [`PROBLEMS`] are listed.
    pub(super) fn problem(&mut self, problem: String) {
        if self.problems.len() < PROBLEMS {
            self.problems.push(problem);
        } else {
            self.more_problems += 1;
        }
    }

    /// Gives the kept hits of session `id` its name, known once its whole
    /// log was read.
    pub(super) fn name(&mut self, id: &SessionId, name: &str) {
        if !self.heap.iter().any(|kept| kept.0.session_id == *id) {
            return;
        }
        let mut kept = std::mem::take(&mut self.heap).into_vec();
        for ranked in kept.iter_mut().filter(|kept| kept.0.session_id == *id) {
            name.clone_into(&mut ranked.0.name);
        }
        self.heap = BinaryHeap::from(kept);
    }

    /// The kept hits, best first, with the counts.
    pub(super) fn found(self) -> Found {
        Found {
            hits: self
                .heap
                .into_sorted_vec()
                .into_iter()
                .map(|ranked| ranked.0)
                .collect(),
            total: self.total,
            problems: self.problems,
            more_problems: self.more_problems,
        }
    }
}

/// A hit ordered by rank: the smaller is the better. Messages and tool
/// inputs come before tool outputs, then newer before older; ties go to the
/// smaller session id, then the larger `seq`, then the label's order.
struct Ranked(Hit);

impl Ranked {
    /// The rank's key.
    fn key(&self) -> (bool, Reverse<u64>, &str, Reverse<u64>, Label) {
        let hit = &self.0;
        (
            hit.label == Label::ToolOutput,
            Reverse(hit.ts),
            hit.session_id.0.as_str(),
            Reverse(hit.seq.0),
            hit.label,
        )
    }
}

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for Ranked {}

impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}
