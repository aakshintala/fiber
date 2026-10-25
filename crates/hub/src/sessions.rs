//! `sessions` (`docs/invocation.md`, "The hub"): one answer holding every
//! live top-level session's latest `session_status` and every exited
//! session's `recent.jsonl` row, scoped as `recent` is. A hub before the
//! end of its first scan of `run/` answers after it.

use std::collections::BTreeSet;
use std::path::Path;

use contract::SessionId;
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
    let (project, active) = session_args(args)?;
    feed.settled();
    Ok(Some(list_with_active(feed.live(), home, project, active)))
}

/// `project` (a string keeping only its sessions) and `active` (a boolean
/// adding each entry's activity): both optional, absent reads as off.
/// Any other key, a wrong type, or an explicit `null` is rejected, so the
/// default listing never opens a session log.
fn session_args(args: &Map<String, Value>) -> Result<(Option<&str>, bool), Refused> {
    let mut project = None;
    let mut active = false;
    for (key, value) in args {
        match (key.as_str(), value) {
            ("project", Value::String(kept)) => project = Some(kept.as_str()),
            ("active", Value::Bool(wanted)) => active = *wanted,
            _ => return Err(invalid()),
        }
    }
    Ok((project, active))
}

/// The answer from a captured `live` snapshot and `home`'s `recent.jsonl`
/// as it reads now, without opening any session log: the default listing.
/// Test-only: the wire path goes through [`answer`], and the unit tests pin
/// this default.
#[cfg(test)]
pub(crate) fn list(
    live: Vec<(String, SessionStatus)>,
    home: &Path,
    project: Option<&str>,
) -> Value {
    list_with_active(live, home, project, false)
}

/// The same answer, but with `active` every entry also carries its
/// activity, read from that entry's session log.
fn list_with_active(
    live: Vec<(String, SessionStatus)>,
    home: &Path,
    project: Option<&str>,
    active: bool,
) -> Value {
    let running: BTreeSet<String> = live.iter().map(|(id, _)| id.clone()).collect();
    if !active {
        // The default listing opens no session log: typed rows straight
        // into the answer, as before opt-in activity, so the listing
        // keeps the `sessions_list_ms` budget.
        let live: Vec<Value> = live
            .into_iter()
            .filter(|(_, status)| project.is_none_or(|key| status.project == key))
            .map(|(id, status)| json!({"session_id": id, "status": status}))
            .collect();
        let exited: Vec<recent::RecentRow> = recent::listed(home, project)
            .into_iter()
            .filter(|row| !running.contains(&row.session_id.0))
            .collect();
        return json!({"live": live, "exited": exited});
    }
    let live: Vec<Value> = live
        .into_iter()
        .filter(|(_, status)| project.is_none_or(|key| status.project == key))
        .map(|(id, status)| {
            let dir = log::session_dir(home, &status.project, &SessionId(id.clone()));
            json!({"session_id": id, "status": status, "active": active_of(&dir)})
        })
        .collect();
    let exited: Vec<Value> = recent::listed(home, project)
        .into_iter()
        .filter(|row| !running.contains(&row.session_id.0))
        .map(|row| {
            let mut entry = serde_json::to_value(&row).unwrap_or(Value::Null);
            let dir = log::session_dir(home, &row.project, &row.session_id);
            if let Value::Object(ref mut map) = entry {
                map.insert("active".to_owned(), active_of(&dir));
            }
            entry
        })
        .collect();
    json!({"live": live, "exited": exited})
}

/// An entry's activity: its log's last complete line's `ts`, else its
/// opening line's `ts`, else JSON null when neither can be read. Neither
/// the row's `ts` nor `status.started` is ever used.
fn active_of(dir: &Path) -> Value {
    match log::last_ts(dir).or_else(|| log::opening_ts(dir)) {
        Some(ts) => Value::from(ts),
        None => Value::Null,
    }
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
