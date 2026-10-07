//! `sessions` (`docs/invocation.md`, "The hub"): one answer holding every
//! live top-level session's latest `session_status` and every exited
//! session's `recent.jsonl` row, scoped as `recent` is. A hub before the
//! end of its first scan of `run/` answers after it.

use std::collections::BTreeSet;
use std::path::Path;

use contract::events::SessionStatus;
use serde_json::{Map, Value, json};

use crate::feed::{Feed, Refusal, invalid};
use crate::recent;

/// Answers `sessions`: the live snapshot is copied first, then
/// `recent.jsonl` is read, and `exited` leaves out exactly the ids in that
/// snapshot, so every session is listed once.
// debt: time this as `sessions.list` with the operation timer
// (docs/code-quality.md, once the timer is documented); trigger: the
// timer's function exists.
pub(crate) fn answer(
    feed: &Feed,
    home: &Path,
    args: &Map<String, Value>,
) -> Result<Option<Value>, Refusal> {
    let project = project_arg(args)?;
    feed.settled();
    Ok(Some(list(feed.live(), home, project)))
}

/// `project`, the only key: absent, or a string.
fn project_arg(args: &Map<String, Value>) -> Result<Option<&str>, Refusal> {
    match (args.len(), args.get("project")) {
        (0, _) => Ok(None),
        (1, Some(Value::String(project))) => Ok(Some(project)),
        _ => Err(invalid()),
    }
}

/// The answer from a captured `live` snapshot and `home`'s `recent.jsonl`
/// as it reads now.
pub(crate) fn list(
    live: Vec<(String, SessionStatus)>,
    home: &Path,
    project: Option<&str>,
) -> Value {
    let running: BTreeSet<String> = live.iter().map(|(id, _)| id.clone()).collect();
    let live: Vec<Value> = live
        .into_iter()
        .filter(|(_, status)| project.is_none_or(|key| status.project == key))
        .map(|(id, status)| json!({"session_id": id, "status": status}))
        .collect();
    let exited: Vec<recent::RecentRow> = recent::listed(home, project)
        .into_iter()
        .filter(|row| !running.contains(&row.session_id.0))
        .collect();
    json!({"live": live, "exited": exited})
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
