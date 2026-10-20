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
/// `old` ran. A missing old log, or a first line still being written, is
/// `session_not_found`, as a rewind names a missing session; an
/// unreadable, unparseable, not-`session_started`, or workspace-less
/// first line, and a worktree record the session cannot read, is
/// `log_corrupt`; no recorded worktree, or one that is not the current
/// directory, is `invalid_arguments`. Every refusal answers before any
/// session line is written.
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
    let recorded = match resolve(&old, &sessions.join(&old.0), &workspace) {
        Ok(recorded) => recorded,
        Err(e) => return ask_failed(e),
    };
    // The continuing process runs in the recorded worktree it did not
    // create: it shares the worktree's lock file for its life, so a
    // recreation or a removal never takes the worktree under it.
    let _shared = match doors::share(Path::new(&recorded.path)) {
        Ok(shared) => shared,
        Err(failure) => return crate::session_command::report(signals, failure),
    };
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

/// Checks the `continues` session `old` names in `old_dir`: its log must
/// open on `session_started` with a worktree identifying `workspace`. The
/// mapping mirrors the hub's scan (`crates/hub/src/start.rs`): a missing
/// log or a first line still being written reads as missing, an
/// unreadable or unparseable first line as corrupt. Past the first line
/// the session needs the whole worktree record, not the scan's path, so
/// this reads the line once itself: a worktree that is absent, or whose
/// path is not a string, is no worktree to continue; a worktree record
/// the session cannot read is corrupt; a worktree that is not the
/// workspace refuses, aliases accepted.
fn resolve(old: &SessionId, old_dir: &Path, workspace: &Path) -> Result<Worktree, Failure> {
    if !old_dir.is_dir() {
        return Err(not_found(old));
    }
    let bytes = match std::fs::read(old_dir.join("events.jsonl")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found(old)),
        Err(_) => return Err(corrupt(old)),
    };
    // A first line still being written reads as missing, as the scan's
    // `Incomplete` does: an empty log ends here too.
    if !bytes.ends_with(b"\n") {
        return Err(not_found(old));
    }
    let line: Value = match serde_json::from_slice(&bytes) {
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
    if path != workspace.to_string_lossy() && !same_dir(path, workspace) {
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

/// Whether `recorded` names the same directory as `workspace`: both
/// sides are canonicalized and compared, so an alias spelling of the
/// same directory accepts. Any canonicalization failure is not a match.
/// Callers try exact string equality first, so matching spellings never
/// canonicalize.
fn same_dir(recorded: &str, workspace: &Path) -> bool {
    match (
        std::fs::canonicalize(recorded),
        std::fs::canonicalize(workspace),
    ) {
        (Ok(recorded), Ok(workspace)) => recorded == workspace,
        _ => false,
    }
}

#[cfg(test)]
#[path = "continue_session_tests.rs"]
mod tests;
