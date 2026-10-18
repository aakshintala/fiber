//! The sessions that continue a session (`docs/invocation.md`, "Deleting
//! and pruning"): a fork and a rewind each name the session they continue
//! in `forked_from` on their `session_started`, the first line of their log.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use contract::SessionId;

use crate::scan::{Started, started_sessions};

/// The fork graph over [`started_sessions`]: each started session's id to
/// the ids that continue it, sorted. Prune reads this instead of walking
/// the logs again.
pub struct Children(BTreeMap<String, BTreeSet<String>>);

impl Children {
    /// The fork graph over `started`: each parent id to its child ids.
    /// Two sessions with one id add one edge each.
    pub fn of(started: &[Started]) -> Self {
        let mut children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for session in started {
            if let Some(from) = session.forked_from.as_ref() {
                children
                    .entry(from.0.clone())
                    .or_default()
                    .insert(session.id.0.clone());
            }
        }
        Self(children)
    }

    /// Every session that continues `id`, directly or through another in
    /// the result: breadth-first from `id`, each step's sessions sorted by
    /// id, each once, never `id` itself. A cycle ends the walk.
    pub fn dependents(&self, id: &str) -> Vec<String> {
        let mut seen = BTreeSet::from([id.to_owned()]);
        let mut queue = VecDeque::from([id.to_owned()]);
        let mut found = Vec::new();
        while let Some(parent) = queue.pop_front() {
            for child in self.0.get(&parent).into_iter().flatten() {
                if seen.insert(child.clone()) {
                    found.push(child.clone());
                    queue.push_back(child.clone());
                }
            }
        }
        found
    }

    /// The ids that reach themselves in one or more steps, over every id
    /// in the map: the sessions on a cycle. A descendant of a cycle is
    /// not one, and neither is a parent no started session names back.
    pub fn cyclic(&self) -> BTreeSet<String> {
        self.0
            .keys()
            .filter(|id| reaches(&self.0, id, id))
            .cloned()
            .collect()
    }
}

/// Whether the walk from `from` reaches `target` again in one or more steps.
fn reaches(children: &BTreeMap<String, BTreeSet<String>>, from: &str, target: &str) -> bool {
    let mut seen = BTreeSet::from([from.to_owned()]);
    let mut queue = VecDeque::from([from.to_owned()]);
    while let Some(parent) = queue.pop_front() {
        for child in children.get(&parent).into_iter().flatten() {
            if child == target {
                return true;
            }
            if seen.insert(child.clone()) {
                queue.push_back(child.clone());
            }
        }
    }
    false
}

/// Every session under `home/projects/*/sessions/` that continues
/// `session`, directly or through another in the result: [`Children`]
/// over [`started_sessions`], mapped back to ids.
pub fn dependents(home: &Path, session: &SessionId) -> Vec<SessionId> {
    Children::of(&started_sessions(home))
        .dependents(&session.0)
        .into_iter()
        .map(SessionId)
        .collect()
}

#[cfg(test)]
#[path = "dependents_tests.rs"]
mod tests;
