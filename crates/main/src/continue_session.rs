//! `fiber session --continues <old>`: starts the session where the named
//! session's recorded worktree is (`docs/invocation.md`, "`start`
//! `continues`"). The old log's first line must be `session_started`
//! recording a worktree that identifies the current directory, the
//! workspace the hub applied; the new session runs through `run_new`
//! with that worktree, the `per_run` layer the dispatch passes, and no
//! `Isolation`, so its process never removes the worktree. The
//! continuing process holds the shared lock for its life. A failure
//! before `fiber_started` deletes its directory,
//! so the hub can start it again from the refusal.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::shapes::{Failure, Worktree};
use contract::{ErrorCode, SessionId};
use serde_json::Value;

use crate::{ask_failed, failed};

/// The internal session command with `--continues`: starts `new` where
/// `old` ran. A session continuing itself is `invalid_arguments`. A
/// missing old log, or a first line still being written, is
/// `session_not_found`, as a rewind names a missing session; an
/// unreadable, unparseable, not-`session_started`, or workspace-less
/// first line, and a worktree record the session cannot read, is
/// `log_corrupt`; no recorded worktree, or one that is not the current
/// directory, is `invalid_arguments`; a worktree removed after the
/// shared hold is taken names the old session `session_not_found`.
/// Every refusal answers before any session line is written.
pub(crate) fn session_continues(
    new: SessionId,
    old: SessionId,
    overrides: Vec<String>,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    fiber: Result<PathBuf, String>,
) -> i32 {
    // One reads for the process, cancelled when this returns whatever the
    // result; there is no worktree isolation to outlive here.
    let reads = Arc::new(crate::switch::Reads::default());
    let _cancel = crate::switch::CancelOnDrop(Arc::clone(&reads));
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(e) => {
            return ask_failed(failed(
                ErrorCode::IoFailed,
                format!("the current directory: {e}"),
            ));
        }
    };
    let sessions = log::sessions_dir(&home, &doors::project(&workspace));
    let recorded = match resolve(&new, &old, &sessions.join(&old.0), &workspace) {
        Ok(recorded) => recorded,
        Err(e) => return ask_failed(e),
    };
    // The continuing process runs in the recorded worktree it did not
    // create: it shares the worktree's lock file for its life, so a
    // recreation or a removal never takes the worktree under it.
    let _shared = match shared(&recorded, &old) {
        Ok(shared) => shared,
        // The mapped refusal answers before any session line, as the
        // recheck does; a share failure with the worktree standing
        // propagates through the run's reporting, as today.
        Err(failure) if failure.code == ErrorCode::SessionNotFound => return ask_failed(failure),
        Err(failure) => return crate::session_command::report(signals, failure),
    };
    // The old session may have ended and cleaned up between validation
    // and the shared hold: recheck the recorded path under the hold,
    // which lives through `run_new`, so no exclusive removal
    // interleaves after the recheck. A removed worktree names the old
    // session missing, before any session line is written.
    if let Err(e) = rechecked(&_shared, &recorded, &old) {
        return ask_failed(e);
    }
    match super::session_command::run_new(
        new,
        overrides,
        None,
        false,
        Some(recorded),
        clock,
        signals,
        fiber,
        None,
        None,
        None,
        None,
        reads,
    ) {
        Ok(code) => code,
        Err(failure) => super::session_command::report(signals, failure),
    }
}

/// Checks the `continues` session `old` names in `old_dir` for the new
/// session `new`: naming itself is `invalid_arguments`, before any log
/// is read. Past that its log must open on `session_started` with a worktree identifying `workspace`. The
/// mapping mirrors the hub's scan (`crates/hub/src/start.rs`): a missing
/// log or a first line still being written reads as missing, an
/// unreadable or unparseable first line as corrupt. Past the first line
/// the session needs the whole worktree record, not the scan's path, so
/// this reads the line once itself: a worktree that is absent, or whose
/// path is not a string, is no worktree to continue; a worktree record
/// the session cannot read is corrupt; a worktree that is not the
/// workspace refuses, aliases accepted.
fn resolve(
    new: &SessionId,
    old: &SessionId,
    old_dir: &Path,
    workspace: &Path,
) -> Result<Worktree, Failure> {
    if new == old {
        return Err(failed(
            ErrorCode::InvalidArguments,
            format!("Session `{}` cannot continue itself.", new.0),
        ));
    }
    if !old_dir.is_dir() {
        return Err(not_found(old));
    }
    let bytes = match std::fs::read(old_dir.join("events.jsonl")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found(old)),
        Err(_) => return Err(corrupt(old)),
    };
    // A first line still being written reads as missing, as the scan's
    // `Incomplete` does: an empty log ends here too. Only the first
    // line is read, as the scan does: later lines are the old session's
    // still-arriving writes, never this check's.
    let Some(end) = bytes.iter().position(|byte| *byte == b'\n') else {
        return Err(not_found(old));
    };
    let prefix = match bytes.get(..end) {
        Some(prefix) => prefix,
        // `end` indexes `bytes` by construction.
        None => return Err(corrupt(old)),
    };
    let line: Value = match serde_json::from_slice(prefix) {
        Ok(line) => line,
        Err(_) => return Err(corrupt(old)),
    };
    if line.get("kind").and_then(Value::as_str) != Some("session_started") {
        return Err(corrupt(old));
    }
    let payload = line.get("payload");
    if payload
        .and_then(|payload| payload.get("workspace"))
        .and_then(Value::as_str)
        .is_none()
    {
        return Err(corrupt(old));
    }
    let worktree = payload.and_then(|payload| payload.get("worktree"));
    let path = worktree
        .and_then(|worktree| worktree.get("path"))
        .and_then(Value::as_str);
    let Some(path) = path else {
        return Err(failed(
            ErrorCode::InvalidArguments,
            format!("Session `{}` runs in no worktree to continue.", old.0),
        ));
    };
    let recorded: Worktree = match worktree.cloned().map(serde_json::from_value) {
        Some(Ok(recorded)) => recorded,
        _ => {
            return Err(failed(
                ErrorCode::LogCorrupt,
                format!(
                    "The log of session `{}` has a worktree record the session cannot read.",
                    old.0
                ),
            ));
        }
    };
    // The schema requires the repository's absolute path: a record
    // without one cannot be continued (`docs/events.md`,
    // `session_started`), so it reads as corrupt, before any session
    // line is written.
    if !recorded
        .repository
        .as_deref()
        .is_some_and(|repository| Path::new(repository).is_absolute())
    {
        return Err(corrupt(old));
    }
    if path != workspace.to_string_lossy() {
        return Err(failed(
            ErrorCode::InvalidArguments,
            format!(
                "Session `{}` runs in a worktree that is not the workspace.",
                old.0
            ),
        ));
    }
    Ok(recorded)
}

/// Rulings 15: shares the recorded worktree's lock file. `share` opens
/// the sibling `.lock` file without creating parents, so a worktree
/// gone with its parents fails here, before the recheck: a failure
/// with the recorded path absent refuses `session_not_found` naming
/// the old session, and a failure with the path present propagates
/// unchanged.
fn shared(recorded: &Worktree, old: &SessionId) -> Result<doors::Shared, Failure> {
    match doors::share(Path::new(&recorded.path)) {
        Ok(shared) => Ok(shared),
        Err(failure) if Path::new(&recorded.path).exists() => Err(failure),
        Err(_) => Err(not_found(old)),
    }
}

/// Rulings 15: the recorded worktree path rechecked after the shared
/// hold is taken. A removed path means the old session ended and
/// cleaned up, so the check refuses `session_not_found` naming the old
/// session. Taking the hold as a parameter types the order: the recheck
/// runs only under the shared lock, and the guard lives through
/// `run_new`, so no exclusive removal interleaves after it.
fn rechecked(_shared: &doors::Shared, recorded: &Worktree, old: &SessionId) -> Result<(), Failure> {
    if Path::new(&recorded.path).is_dir() {
        Ok(())
    } else {
        Err(not_found(old))
    }
}

/// A session the hub does not know, or whose first line is still being
/// written: `session_not_found`, naming the id as a rewind does.
fn not_found(old: &SessionId) -> Failure {
    failed(
        ErrorCode::SessionNotFound,
        format!("No session `{}`.", old.0),
    )
}

/// An unreadable, unparseable, or not-`session_started` first line:
/// `log_corrupt`, in the hub's words for the same log.
fn corrupt(old: &SessionId) -> Failure {
    failed(
        ErrorCode::LogCorrupt,
        format!(
            "The log of session `{}` does not start with its workspace.",
            old.0
        ),
    )
}

#[cfg(test)]
#[path = "continue_session_tests.rs"]
mod tests;
