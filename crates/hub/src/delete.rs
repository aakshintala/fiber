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

use contract::{ErrorCode, SessionId};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::connection::{Hub, Refused};
use crate::feed::invalid;
use crate::relay::valid_session_id;
use crate::resume::not_found;

/// `delete`: removes `session`, and with `cascade` every session that
/// continues it, dependents first. With `cascade`, `expect` holds the set
/// the person confirmed; without `cascade` it is checked but not compared.
/// With `dry_run`, answers `sessions`, the ids the delete would remove,
/// and `held`, those another process holds, and deletes nothing. Accepted
/// with no `result`, except the dry run's answer.
pub(crate) fn delete(hub: &Hub, args: &Map<String, Value>) -> Result<Option<Value>, DeleteRefused> {
    let parsed = parse(args).ok_or_else(invalid)?;
    let session = parsed.session;
    let cascade = parsed.cascade;
    let expect = parsed.expect;
    // No resume starts a process for a session while it is deleted.
    let _gate = support::lock(&hub.resume_gate);
    let set = resolve(hub, &session, cascade, expect.as_ref())?;
    // Held until every removal ends. A dry run probes existing locks
    // without creating missing ones and reports busy ids without refusing;
    // guards it takes stay held until the answer. A real delete probes
    // every candidate before refusing, so one busy session never hides a
    // later lock failure, and refuses before removing any part of the set.
    let mut locks = Vec::with_capacity(set.len());
    let mut busy: Vec<String> = Vec::new();
    for (id, dir) in &set {
        let lock = if parsed.dry_run {
            log::try_hold_existing(dir).map_err(|error| io_failed(id, &error))?
        } else {
            Some(log::try_hold(dir).map_err(|error| io_failed(id, &error))?)
        };
        match lock {
            None => {}
            Some(log::Hold::Held(lock)) => locks.push(lock),
            Some(log::Hold::Busy) => busy.push(id.0.clone()),
        }
    }
    if parsed.dry_run {
        let ids: Vec<&str> = set.iter().map(|(id, _)| id.0.as_str()).collect();
        return Ok(Some(serde_json::json!({"sessions": ids, "held": busy})));
    }
    if let Some(first) = busy.first().cloned() {
        return Err(held_refused(&first, busy));
    }
    // Discovery order puts each session after the one it continues, so in
    // reverse no survivor points at a removed session.
    for ((id, dir), lock) in set.iter().zip(locks.iter()).rev() {
        log::delete_session(dir, lock).map_err(|error| io_failed(id, &error))?;
        hub.feed.forget(&id.0);
    }
    drop(locks);
    Ok(None)
}

/// The sessions a delete would remove: `session` first, then every session
/// that continues it, each with its directory. Refuses a missing session,
/// dependents without `cascade`, and a confirmed set that differs, exactly
/// as the removing path does. Takes no lock and removes nothing.
fn resolve(
    hub: &Hub,
    session: &SessionId,
    cascade: bool,
    expect: Option<&BTreeSet<SessionId>>,
) -> Result<Vec<(SessionId, PathBuf)>, Refused> {
    let Some(dir) = session_dir(hub, session) else {
        return Err(not_found(session));
    };
    let dependents = log::dependents(&hub.home, session);
    if !cascade && !dependents.is_empty() {
        let mut names: Vec<String> = dependents.iter().map(|id| format!("`{}`", id.0)).collect();
        names.sort();
        return Err(Refused {
            code: ErrorCode::SessionHasDependents,
            message: format!(
                "Session `{}` has sessions that continue it: {}. `--cascade` deletes them too.",
                session.0,
                names.join(", ")
            ),
        });
    }
    let mut set = vec![(session.clone(), dir)];
    // A dependent whose directory went since the walk is not deleted.
    set.extend(
        dependents
            .into_iter()
            .filter_map(|id| session_dir(hub, &id).map(|dir| (id, dir))),
    );
    if cascade && let Some(expect) = expect {
        let set_ids: BTreeSet<&SessionId> = set.iter().map(|(id, _)| id).collect();
        let wanted: BTreeSet<&SessionId> = expect.iter().collect();
        confirm(&set_ids, &wanted)?;
    }
    Ok(set)
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
    dry_run: bool,
}

fn parse(args: &Map<String, Value>) -> Option<DeleteArgs> {
    if args
        .keys()
        .any(|key| key != "session" && key != "cascade" && key != "expect" && key != "dry_run")
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
    let dry_run = match args.get("dry_run") {
        None => false,
        Some(Value::Bool(dry_run)) => *dry_run,
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
        dry_run,
    })
}

/// The set the hub would remove matches the confirmed set, or the delete
/// is refused naming the set it would remove now. Takes no lock and
/// removes nothing.
fn confirm(set_ids: &BTreeSet<&SessionId>, expect: &BTreeSet<&SessionId>) -> Result<(), Refused> {
    if set_ids == expect {
        return Ok(());
    }
    let names: Vec<String> = set_ids.iter().map(|id| format!("`{}`", id.0)).collect();
    Err(Refused {
        code: ErrorCode::StaleRequest,
        message: format!(
            "The sessions this delete would remove are now {}. Nothing was deleted.",
            names.join(", ")
        ),
    })
}

/// `session`'s directory: a real directory, never a link, holding its log.
/// The link check precedes the lock, which would create `session.lock`
/// inside whatever `dir` names, so a linked directory stays untouched.
fn session_dir(hub: &Hub, session: &SessionId) -> Option<PathBuf> {
    let (_, dir) = crate::recent::lookup_logged(&hub.home, session)?;
    log::is_plain_dir(&dir).then_some(dir)
}

/// A delete refusal: only a real `session_held` refusal carries the busy
/// ids, in the order the hub checked them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DeleteRefused {
    /// The rejection code.
    pub(crate) code: ErrorCode,
    /// The rejection message.
    pub(crate) message: String,
    /// Busy ids in check order, only for a real `session_held` refusal.
    pub(crate) held: Option<Vec<String>>,
}

impl From<Refused> for DeleteRefused {
    fn from(refused: Refused) -> Self {
        Self {
            code: refused.code,
            message: refused.message,
            held: None,
        }
    }
}

/// A real delete blocked by busy sessions. Keep the single-session
/// wording even when `held` names several ids. A later lock-probe failure
/// still refuses `io_failed` instead, without deleting.
fn held_refused(first: &str, held: Vec<String>) -> DeleteRefused {
    DeleteRefused {
        code: ErrorCode::SessionHeld,
        message: format!("Session `{first}` is held by another process."),
        held: Some(held),
    }
}

fn io_failed(session: &SessionId, error: &impl std::fmt::Display) -> Refused {
    Refused {
        code: ErrorCode::IoFailed,
        message: format!("Session `{}` could not be deleted: {error}.", session.0),
    }
}

#[cfg(test)]
#[path = "delete_tests.rs"]
mod tests;
