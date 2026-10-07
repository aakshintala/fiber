//! `delete` (`docs/invocation.md`, "Deleting and pruning"): removes an
//! exited session's directory, its log and artifacts together, and drops
//! it from the feed. A session another process holds is refused, and so is
//! one that forks or rewinds continue, unless `cascade` deletes them too.
//! With `cascade`, `expect` names the sessions the person confirmed; when
//! the sessions the hub would remove differ, the delete is refused and
//! nothing is deleted.
//!
//! The hub takes the lock of every session it deletes before removing any,
//! so a session process opening one meanwhile is refused as held. Each
//! directory loses its log first: a process that takes a lock left behind
//! once removal began finds no log to open, so nothing writes into a
//! directory being removed.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::PathBuf;
use std::sync::PoisonError;

use contract::{ErrorCode, SessionId};
use serde_json::{Map, Value};

use crate::connection::Hub;
use crate::feed::{Refusal, invalid};
use crate::relay::valid_session_id;
use crate::resume::{find_log, not_found};

/// `delete`: removes `session`, and with `cascade` every session that
/// continues it, dependents first. With `cascade`, `expect` holds the set
/// the person confirmed; without `cascade` it is checked but not compared.
/// Accepted with no `result`.
pub(crate) fn delete(hub: &Hub, args: &Map<String, Value>) -> Result<Option<Value>, Refusal> {
    let parsed = parse(args).ok_or_else(invalid)?;
    let session = parsed.session;
    let cascade = parsed.cascade;
    let expect = parsed.expect;
    // No resume starts a process for a session while it is deleted.
    let _gate = hub
        .resume_gate
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let Some(dir) = session_dir(hub, &session) else {
        let refused = not_found(&session);
        return Err((refused.code, refused.message));
    };
    let dependents = log::dependents(&hub.home, &session);
    if !cascade && !dependents.is_empty() {
        let mut names: Vec<String> = dependents.iter().map(|id| format!("`{}`", id.0)).collect();
        names.sort();
        return Err((
            ErrorCode::SessionHasDependents,
            format!(
                "Session `{}` has sessions that continue it: {}. `--cascade` deletes them too.",
                session.0,
                names.join(", ")
            ),
        ));
    }
    let mut set = vec![(session, dir)];
    // A dependent whose directory went since the walk is not deleted.
    set.extend(
        dependents
            .into_iter()
            .filter_map(|id| session_dir(hub, &id).map(|dir| (id, dir))),
    );
    if cascade && let Some(expect) = &expect {
        let set_ids: BTreeSet<&SessionId> = set.iter().map(|(id, _)| id).collect();
        let wanted: BTreeSet<&SessionId> = expect.iter().collect();
        confirm(&set_ids, &wanted)?;
    }
    // Held until every removal ends.
    let mut locks = Vec::with_capacity(set.len());
    for (id, dir) in &set {
        match take_lock(dir) {
            Ok(lock) => locks.push(lock),
            Err(TryLockError::WouldBlock) => {
                return Err((
                    ErrorCode::SessionHeld,
                    format!("Session `{}` is held by another process.", id.0),
                ));
            }
            Err(TryLockError::Error(error)) => return Err(io_failed(id, &error)),
        }
    }
    // Discovery order puts each session after the one it continues, so in
    // reverse no survivor points at a removed session.
    for (id, dir) in set.iter().rev() {
        remove(dir).map_err(|error| io_failed(id, &error))?;
        hub.feed.forget(&id.0);
    }
    drop(locks);
    Ok(None)
}

/// `session` (a minted id), `cascade` (default false) and `expect` (an
/// optional array of minted ids); `None` for any other shape. The ids are
/// only compared, never joined into a path except `session`, whose shape is
/// checked here as it is joined into a path; every `expect` element is
/// checked the same way.
struct DeleteArgs {
    session: SessionId,
    cascade: bool,
    expect: Option<BTreeSet<SessionId>>,
}

fn parse(args: &Map<String, Value>) -> Option<DeleteArgs> {
    if args
        .keys()
        .any(|key| key != "session" && key != "cascade" && key != "expect")
    {
        return None;
    }
    let session = args.get("session")?.as_str()?;
    if !valid_session_id(session) {
        return None;
    }
    let cascade = match args.get("cascade") {
        None => false,
        Some(Value::Bool(cascade)) => *cascade,
        Some(_) => return None,
    };
    let expect = match args.get("expect") {
        None => None,
        Some(Value::Array(elements)) => {
            let mut set = BTreeSet::new();
            for element in elements {
                let text = element.as_str()?;
                if !valid_session_id(text) {
                    return None;
                }
                set.insert(SessionId(text.to_owned()));
            }
            Some(set)
        }
        Some(_) => return None,
    };
    Some(DeleteArgs {
        session: SessionId(session.to_owned()),
        cascade,
        expect,
    })
}

/// The set the hub would remove matches the confirmed set, or the delete
/// is refused naming the set it would remove now. Takes no lock and
/// removes nothing.
fn confirm(set_ids: &BTreeSet<&SessionId>, expect: &BTreeSet<&SessionId>) -> Result<(), Refusal> {
    if set_ids == expect {
        return Ok(());
    }
    let mut names: Vec<String> = set_ids.iter().map(|id| format!("`{}`", id.0)).collect();
    names.sort();
    Err((
        ErrorCode::StaleRequest,
        format!(
            "The sessions this delete would remove are now {}. Nothing was deleted.",
            names.join(", ")
        ),
    ))
}

/// `session`'s directory: a real directory, never a link, holding its log.
fn session_dir(hub: &Hub, session: &SessionId) -> Option<PathBuf> {
    let dir = find_log(&hub.home, session)?.parent()?.to_path_buf();
    fs::symlink_metadata(&dir)
        .is_ok_and(|meta| meta.is_dir())
        .then_some(dir)
}

/// The session's lock, created if absent and taken without waiting.
fn take_lock(dir: &std::path::Path) -> Result<File, TryLockError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("session.lock"))
        .map_err(TryLockError::Error)?;
    file.try_lock()?;
    Ok(file)
}

/// Removes the log, then the rest of the directory. `remove_dir_all`
/// removes a link inside it without following it.
fn remove(dir: &std::path::Path) -> io::Result<()> {
    fs::remove_file(dir.join("events.jsonl"))?;
    fs::remove_dir_all(dir)
}

fn io_failed(session: &SessionId, error: &io::Error) -> Refusal {
    (
        ErrorCode::IoFailed,
        format!("Session `{}` could not be deleted: {error}.", session.0),
    )
}

#[cfg(test)]
#[path = "delete_tests.rs"]
mod tests;
