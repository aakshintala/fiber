//! `archive` (`docs/invocation.md`, "Archiving"): appends the `archived`
//! marker to an exited session's log and re-appends its `recent.jsonl`
//! row with `archived` true, so listings that filter archives leave it
//! out. A session another process holds, a running one included, is
//! refused `session_held`; resuming an archived session unarchives it,
//! since the marker only counts past the last `fiber_started`.

use std::path::Path;
use std::sync::Arc;

use contract::clock::wall_ms;
use contract::events::{Empty, Event};
use contract::{ErrorCode, SessionId};
use serde_json::{Map, Value};
use support::lock;

use crate::connection::{Hub, Refused};
use crate::feed::invalid;
use crate::recent::{self, Left, RecentRow};
use crate::relay::valid_session_id;
use crate::resume::not_found;

/// `archive`: appends `archived` to `session`'s log and re-appends its
/// derived row with `archived` true. Accepted with no result, once both
/// appends succeed; a failed index append refuses `io_failed` and keeps
/// the event. An already archived session is accepted: a retry appends
/// no new event, and the derived row only when its newest row is not
/// already archived, so a converged retry writes nothing. A missing or
/// non-plain session, or one with no log, is `session_not_found`; a
/// running session or a held lock is `session_held` without delete's
/// `held` field.
pub(crate) fn archive(hub: &Hub, args: &Map<String, Value>) -> Result<Option<Value>, Refused> {
    let session = parse(args).ok_or_else(invalid)?;
    // No resume starts a process for a session while it is archived.
    let _gate = lock(&hub.resume_gate);
    let Some((_, dir)) = recent::lookup_logged(&hub.home, &session) else {
        return Err(not_found(&session));
    };
    if !log::is_plain_dir(&dir) {
        return Err(not_found(&session));
    }
    // Serialized with a late `on_left`: the row `on_left` appends is the
    // source a retry copies, and `on_left` must not append a stale row or
    // reinsert a rail entry after this forgets the session.
    let _recent = lock(hub.feed.recent_mutation());
    // A running session's lock can be momentarily free, so the feed's
    // live set refuses it, not the lock probe alone.
    if hub.feed.live().iter().any(|(id, _)| id == &session.0) {
        return Err(held(&session));
    }
    #[cfg(test)]
    fire_before_commit();
    let sessions = dir.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
    // One opening: it validates complete lines, derives the next `seq`
    // and repairs only a torn tail. Never a second acquisition.
    let log = match log::Log::open(&sessions, session.clone(), Arc::clone(&hub.clock)) {
        Ok(log) => log,
        Err(log::Error::Held { .. }) => return Err(held(&session)),
        Err(log::Error::NotFound(_)) => return Err(not_found(&session)),
        Err(error) => return Err(io_failed(&session, &error)),
    };
    if log::is_archived(&dir) {
        // A retry: no new event. The derived row is appended only when
        // the newest row is not already archived.
        let newest = newest_row(&hub.home, &session);
        if newest.as_ref().is_none_or(|row| !row.archived) {
            let row = newest
                .map(marked)
                .unwrap_or_else(|| missing_row(hub, &dir, &session));
            recent::append(&hub.home, &row).map_err(|error| io_failed(&session, &error))?;
        }
        hub.feed.forget(&session.0);
        return Ok(None);
    }
    log.append(&Event::Archived(Empty {}), None, None)
        .map_err(|error| io_failed(&session, &error))?;
    let newest = newest_row(&hub.home, &session);
    let row = newest
        .map(marked)
        .unwrap_or_else(|| missing_row(hub, &dir, &session));
    // The index is derived state, not a second record of truth: a failed
    // append refuses without rolling back the log's event.
    recent::append(&hub.home, &row).map_err(|error| io_failed(&session, &error))?;
    hub.feed.forget(&session.0);
    Ok(None)
}

/// `session` (a minted id) and nothing else; `None` for any other shape.
fn parse(args: &Map<String, Value>) -> Option<SessionId> {
    if args.len() != 1 {
        return None;
    }
    let session = args.get("session")?.as_str()?;
    if !valid_session_id(session) {
        return None;
    }
    Some(SessionId(session.to_owned()))
}

/// The session's newest derived row, in file order.
fn newest_row(home: &Path, session: &SessionId) -> Option<RecentRow> {
    recent::read_all(home)
        .into_iter()
        .filter(|row| row.session_id == *session)
        .last()
}

/// The newest row marked archived.
fn marked(mut row: RecentRow) -> RecentRow {
    row.archived = true;
    row
}

/// The archived row for a logged session with no derived row: the same
/// conventional crashed shape [`Feed::append_crashed`] writes, from the
/// opening record's workspace and the available name and status, then
/// marked archived. Empty and absent defaults when no status survived.
fn missing_row(hub: &Hub, dir: &Path, session: &SessionId) -> RecentRow {
    let status = hub.feed.status_of(&session.0);
    let workspace = status
        .as_ref()
        .map(|status| status.workspace.clone())
        .or_else(|| opening_workspace(dir))
        .unwrap_or_default();
    RecentRow {
        session_id: session.clone(),
        ts: wall_ms(hub.clock.wall()),
        project: project_of(dir),
        workspace,
        name: status
            .as_ref()
            .map(|status| status.name.clone())
            .unwrap_or_default(),
        how: Left::Crashed,
        status,
        archived: true,
    }
}

/// The workspace `dir`'s `session_started` recorded, when it parses as
/// one.
fn opening_workspace(dir: &Path) -> Option<String> {
    let mut lines = log::lines(dir).ok()?;
    let first = lines.next()?;
    let line = first.ok()?;
    match Event::from_envelope(&line).ok()? {
        Some(Event::SessionStarted(started)) => Some(started.workspace),
        _ => None,
    }
}

/// The project key naming `dir`: `projects/<key>/sessions/<id>`.
fn project_of(dir: &Path) -> String {
    dir.parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map(|key| key.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A held or running session's refusal: `session_held`, carrying no
/// `held` field.
fn held(session: &SessionId) -> Refused {
    Refused {
        code: ErrorCode::SessionHeld,
        message: format!("Session `{}` is held by another process.", session.0),
    }
}

/// A log or index failure keeps the event and refuses `io_failed`.
fn io_failed(session: &SessionId, error: &impl std::fmt::Display) -> Refused {
    Refused {
        code: ErrorCode::IoFailed,
        message: format!(
            "Session `{session}` could not be archived: {error}.",
            session = session.0
        ),
    }
}

// Test seam: armed on the archiving thread by the race test; fires
// after the live check passes and before the exclusive open, while the
// recent-mutation mutex is held and no marker is committed yet.
// Thread-local, like `delete`'s dry-run pause, so parallel tests never
// observe each other's hooks. Compiles to nothing outside tests. No
// outside seam observes the admitted instant before the open: without
// the signal the test cannot stage an `on_left` past the gate but
// before the commit, and the weakened test would pass either way.
#[cfg(test)]
thread_local! {
    static BEFORE_COMMIT: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
}

/// Tests only: arms the race test's hook on this thread, firing after
/// the live check and before the exclusive open.
#[cfg(test)]
pub(crate) fn pause_before_commit(hook: impl FnOnce() + 'static) {
    BEFORE_COMMIT.with(|slot| slot.set(Some(Box::new(hook))));
}

#[cfg(test)]
fn fire_before_commit() {
    BEFORE_COMMIT.with(|slot| {
        if let Some(hook) = slot.take() {
            hook();
        }
    });
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
