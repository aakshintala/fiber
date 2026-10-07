//! The sessions that continue a session (`docs/invocation.md`, "Deleting
//! and pruning"): a fork and a rewind each name the session they continue
//! in `forked_from` on their `session_started`, the first line of their log.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use contract::SessionId;
use serde_json::Value;

use crate::EVENTS;

/// Every session under `home/projects/*/sessions/` that continues
/// `session`, directly or through another in the result: breadth-first
/// from `session`, each step's sessions sorted by id, each once, never
/// `session` itself. Only first lines are read; a log whose first line is
/// not a readable `session_started` is skipped, and so is a session
/// directory that is a symbolic link. A cycle ends the walk.
pub fn dependents(home: &Path, session: &SessionId) -> Vec<SessionId> {
    let mut children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (id, from) in pointers(home) {
        children.entry(from).or_default().insert(id);
    }
    let mut seen = BTreeSet::from([session.0.clone()]);
    let mut queue = VecDeque::from([session.0.clone()]);
    let mut found = Vec::new();
    while let Some(parent) = queue.pop_front() {
        for child in children.get(&parent).into_iter().flatten() {
            if seen.insert(child.clone()) {
                found.push(SessionId(child.clone()));
                queue.push_back(child.clone());
            }
        }
    }
    found
}

/// Each session under `home` that names a session it continues: its id
/// and that session's id.
fn pointers(home: &Path) -> Vec<(String, String)> {
    let Ok(projects) = std::fs::read_dir(home.join("projects")) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for project in projects.flatten() {
        let Ok(sessions) = std::fs::read_dir(project.path().join("sessions")) else {
            continue;
        };
        for entry in sessions.flatten() {
            // `file_type` does not follow a link: a linked directory is
            // not a session.
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if let Some(from) = forked_from(&entry.path().join(EVENTS)) {
                found.push((entry.file_name().to_string_lossy().into_owned(), from));
            }
        }
    }
    found
}

/// The `forked_from.session_id` of the `session_started` on `log`'s
/// first line.
fn forked_from(log: &Path) -> Option<String> {
    let mut first = String::new();
    BufReader::new(File::open(log).ok()?)
        .read_line(&mut first)
        .ok()?;
    let line: Value = serde_json::from_str(&first).ok()?;
    if line.get("kind")?.as_str()? != "session_started" {
        return None;
    }
    let from = line.get("payload")?.get("forked_from")?.get("session_id")?;
    from.as_str().map(str::to_owned)
}

#[cfg(test)]
#[path = "dependents_tests.rs"]
mod tests;
