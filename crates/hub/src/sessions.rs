//! `sessions` (`docs/invocation.md`, "The hub"): one answer holding every
//! live top-level session's latest `session_status` and every exited
//! session's `recent.jsonl` row, scoped as `recent` is. A hub before the
//! end of its first scan of `run/` answers after it.

use std::collections::BTreeSet;
use std::path::Path;

use contract::clock::Clock;
use contract::diag::{Op, Timing};
use contract::events::SessionStatus;
use serde_json::{Map, Value, json};

use crate::connection::Refused;
use crate::feed::{Feed, invalid};
use crate::recent;

/// Answers `sessions`: the live snapshot is copied first, then
/// `recent.jsonl` is read, and `exited` leaves out exactly the ids in that
/// snapshot, so every session is listed once. The timer covers the whole
/// attempt, from entry through the snapshot and row reads: an invalid
/// argument is an entered attempt, not an unlisted one.
pub(crate) fn answer(
    feed: &Feed,
    home: &Path,
    args: &Map<String, Value>,
    clock: &dyn Clock,
    timing: Option<&Timing>,
) -> Result<Option<Value>, Refused> {
    // The start stamp comes from the caller's clock, so the helper takes
    // no start read of its own; without a callable no clock is read.
    let started = timing.map(|_| clock.now());
    let _timer = timing.and_then(|timing| timing(Op::SessionsList, None, started));
    let project = project_arg(args)?;
    feed.settled();
    Ok(Some(list(feed.live(), home, project)))
}

/// `project`, the only key: absent, or a string.
fn project_arg(args: &Map<String, Value>) -> Result<Option<&str>, Refused> {
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
