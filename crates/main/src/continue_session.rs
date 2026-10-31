//! `fiber session --continues <old>`: starts the session where the named
//! session's recorded worktree is (`docs/invocation.md`, "`start`
//! `continues`"). The old log's first line must be `session_started`
//! recording a worktree that identifies `workspace`, the original
//! `args.workspace` spelling the dispatch passes; the new session runs
//! through `run_new` with that worktree, the `per_run` layer the
//! dispatch passes, and no
//! `Isolation`, so its process never removes the worktree. The
//! continuing process holds the shared lock for its life. A failure
//! before `fiber_started` deletes its directory,
//! so the hub can start it again from the refusal.

use std::path::Path;
use std::sync::Arc;

use contract::shapes::{Failure, Worktree};
use contract::{ErrorCode, SessionId};

use crate::{ask_failed, failed};

/// The internal session command with `--continues`: starts `new` where
/// `old` ran. A session continuing itself is `invalid_arguments`. A
/// missing old log, or a first line still being written, is
/// `session_not_found`, as a rewind names a missing session; an
/// unreadable first line is `log_corrupt`; no recorded worktree, or one
/// that is not `workspace`, is `invalid_arguments`, while a present but
/// malformed worktree record is `log_corrupt`; a worktree removed after
/// the shared hold is taken names the old session `session_not_found`.
/// Every refusal answers before any session line is written.
#[allow(
    clippy::too_many_arguments,
    reason = "the continues call takes each launch option once"
)]
pub(crate) fn session_continues(
    new: SessionId,
    old: SessionId,
    overrides: Vec<String>,
    workspace: &Path,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    process: &crate::startup::Process,
    supervised: bool,
) -> i32 {
    // One reads for the process, cancelled when this returns whatever the
    // result; there is no worktree isolation to outlive here.
    let reads = Arc::new(crate::switch::Reads::default());
    let _cancel = crate::switch::CancelOnDrop(Arc::clone(&reads));
    let (home, current) = match crate::startup::startup() {
        Ok(directories) => directories,
        Err(e) => return ask_failed(e),
    };
    // The sessions dir comes from the entered directory, as every entry
    // does; the identity compare below uses the passed-in original
    // spelling, before `set_current_dir` normalized slashes and symlinks
    // away.
    let sessions = log::sessions_dir(&home, &worktree::project(&current));
    let recorded = match resolve(
        &new,
        &old,
        &log::SessionDir::new(&sessions, &old),
        workspace,
    ) {
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
        process,
        None,
        None,
        None,
        None,
        reads,
        supervised,
        Vec::new(),
    ) {
        Ok(code) => code,
        Err(failure) => super::session_command::report(signals, failure),
    }
}

/// Checks the `continues` session `old` names in `old_dir` for the new
/// session `new`: naming itself is `invalid_arguments`, before any log
/// is read. Past that the first line goes through the shared scan
/// (`log::started_first`, as the hub reads it): still being written
/// reads as missing, anything else unreadable as corrupt, and the
/// workspace is guaranteed. Past the first line the session needs the
/// whole worktree record, not the scan's path, so the record goes
/// through the scan's tri-state: absent is no worktree to continue, a
/// present but malformed record (including one without an absolute
/// `repository`) is corrupt; a record that is not `workspace` refuses,
/// aliases refused.
fn resolve(
    new: &SessionId,
    old: &SessionId,
    old_dir: &log::SessionDir,
    workspace: &Path,
) -> Result<Worktree, Failure> {
    if new == old {
        return Err(failed(
            ErrorCode::InvalidArguments,
            format!("Session `{}` cannot continue itself.", new.0),
        ));
    }
    // The first line through the shared scan, exactly as the hub reads
    // it: the workspace is guaranteed by `started_first`.
    let recorded = match log::started_first(old_dir.path()) {
        Ok(recorded) => recorded,
        Err(log::FirstLineError::Incomplete) => return Err(not_found(old)),
        Err(log::FirstLineError::Corrupt) => return Err(corrupt(old)),
    };
    let recorded = match recorded.worktree_record() {
        Ok(Some(recorded)) => recorded,
        Ok(None) => {
            return Err(failed(
                ErrorCode::InvalidArguments,
                format!("Session `{}` runs in no worktree to continue.", old.0),
            ));
        }
        Err(_) => return Err(corrupt(old)),
    };
    if recorded.path != workspace.to_string_lossy() {
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
        Err(failure) if Path::new(&recorded.path).exists() => Err(Failure::from(failure)),
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
