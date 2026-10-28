//! `prune` (`docs/invocation.md`, "What the hub speaks"): prunes the
//! hub's own machine across every project through the machine seam. A dry
//! run answers the preview and deletes nothing; the confirmed run answers
//! the space freed and each item that failed.

use std::collections::BTreeSet;
use std::sync::Arc;

use contract::SessionId;
use contract::commands::hub::{PruneExpect, PruneSession};
use contract::shapes::Failure;
use serde_json::{Map, Value, json};

use crate::connection::{Hub, Refused};
use crate::feed::invalid;

/// `prune`: well-shaped args reach the machine with a session-delete
/// closure that calls the hub's own `delete` directly; a dry run answers
/// the preview, otherwise the run answers `freed`/`total`/`failures`.
/// Without a machine the dispatch keeps `unknown_command`, as
/// `configure` does.
pub(crate) fn prune(hub: &Hub, args: &Map<String, Value>) -> Result<Option<Value>, Refused> {
    let parsed = parse(args).ok_or_else(invalid)?;
    let machine = hub.machine.as_ref().ok_or_else(invalid)?;
    // The hub side runs the hub's own `delete::delete`, so `resume_gate`,
    // per-session locks, `stale_request`, `feed.forget` and refusal codes
    // are exactly the delete's own. The closure borrows the hub; the seam
    // borrows the closure mutably; every borrow ends when the seam call
    // returns.
    let mut delete_session =
        |session: &SessionId, cascade: bool, expect: Option<&BTreeSet<SessionId>>| {
            let mut delete_args = Map::new();
            delete_args.insert("session".to_owned(), Value::String(session.0.clone()));
            if cascade {
                delete_args.insert("cascade".to_owned(), Value::Bool(true));
            }
            if let Some(expect) = expect {
                delete_args.insert(
                    "expect".to_owned(),
                    Value::Array(
                        expect
                            .iter()
                            .map(|id| Value::String(id.0.clone()))
                            .collect(),
                    ),
                );
            }
            match crate::delete::delete(hub, &delete_args) {
                Ok(_) => Ok(()),
                Err(refused) => Err(Failure {
                    code: refused.code(),
                    message: refused.message().to_owned(),
                    provider: None,
                    retry_after_ms: None,
                }),
            }
        };
    let outcome = machine
        .prune(
            &hub.home,
            parsed.older_than,
            parsed.cascade,
            parsed.force,
            parsed.dry_run,
            parsed.expect,
            Arc::clone(&hub.clock),
            &mut delete_session,
        )
        .map_err(|failure| Refused {
            code: failure.code,
            message: failure.message,
        })?;
    if parsed.dry_run {
        Ok(Some(json!({
            "lines": outcome.lines,
            "would_free": outcome.freed,
            "items": outcome.items,
        })))
    } else {
        Ok(Some(json!({
            "freed": outcome.freed,
            "total": outcome.total,
            "failures": outcome.failures,
        })))
    }
}

/// `older_than` (an optional string), `cascade`/`force`/`dry_run`
/// (optional booleans) and `expect` (an optional previewed-items object);
/// `None` for any other shape. Missing reads as off and empty; unknown
/// keys, wrong types and explicit `null` are invalid.
struct PruneArgs {
    older_than: Option<String>,
    cascade: bool,
    force: bool,
    dry_run: bool,
    expect: Option<PruneExpect>,
}

fn parse(args: &Map<String, Value>) -> Option<PruneArgs> {
    if args.keys().any(|key| {
        key != "older_than"
            && key != "cascade"
            && key != "force"
            && key != "dry_run"
            && key != "expect"
    }) {
        return None;
    }
    let optional_string = |key: &str| match args.get(key) {
        None => Some(None),
        Some(Value::String(text)) => Some(Some(text.clone())),
        Some(_) => None,
    };
    let flag = |key: &str| match args.get(key) {
        None => Some(false),
        Some(Value::Bool(flag)) => Some(*flag),
        Some(_) => None,
    };
    let older_than = optional_string("older_than")?;
    let cascade = flag("cascade")?;
    let force = flag("force")?;
    let dry_run = flag("dry_run")?;
    // An object only: a derived parse would also read an array through
    // the sequence path, against the wire shape.
    let expect = match args.get("expect") {
        None => None,
        Some(expect @ Value::Object(_)) => Some(expect_of(expect)?),
        Some(_) => return None,
    };
    Some(PruneArgs {
        older_than,
        cascade,
        force,
        dry_run,
        expect,
    })
}

/// `expect`: the previewed items object, each list defaulting empty. Names
/// and shapes are checked key by key, so unknown keys, wrong types and
/// explicit `null` are invalid.
fn expect_of(value: &Value) -> Option<PruneExpect> {
    let map = value.as_object()?;
    if map.keys().any(|key| {
        key != "sessions" && key != "worktrees" && key != "pinned" && key != "diagnostics"
    }) {
        return None;
    }
    let strings = |key: &str| -> Option<Vec<String>> {
        match map.get(key) {
            None => Some(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect(),
            Some(_) => None,
        }
    };
    let sessions = match map.get("sessions") {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            let mut sessions = Vec::with_capacity(items.len());
            for item in items {
                sessions.push(session_of(item)?);
            }
            sessions
        }
        Some(_) => return None,
    };
    Some(PruneExpect {
        sessions,
        worktrees: strings("worktrees")?,
        pinned: strings("pinned")?,
        diagnostics: strings("diagnostics")?,
    })
}

/// One previewed session: its `id` with its confirmed `cascade` set.
fn session_of(item: &Value) -> Option<PruneSession> {
    let entry = item.as_object()?;
    if entry.keys().any(|key| key != "id" && key != "cascade") {
        return None;
    }
    let id = entry.get("id")?.as_str()?;
    let Some(Value::Array(members)) = entry.get("cascade") else {
        return None;
    };
    let mut cascade = Vec::with_capacity(members.len());
    for member in members {
        cascade.push(SessionId(member.as_str()?.to_owned()));
    }
    Some(PruneSession {
        id: SessionId(id.to_owned()),
        cascade,
    })
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod tests;
