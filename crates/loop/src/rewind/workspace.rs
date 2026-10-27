//! The `change_workspace` driver command (`docs/events.md`, "Change
//! workspace"): its refusals, the resolved target workspace, and `moved`
//! as the last line.

use std::path::{Path, PathBuf};

use contract::commands::ChangeWorkspaceArgs;
use contract::events::{CommandResult, Event, Moved};
use contract::inbox::{Ack, Rejection};
use contract::{CommandId, ErrorCode, Seq, SessionId};

use crate::inbox::{TurnInput, reject};
use crate::rewind::command::refuse_after_rewound;
use crate::{Error, Loop};

/// `change_workspace` refused while a turn runs or one is about to start:
/// move once it ends.
pub(crate) const TURN_RUNNING: &str = "A turn is running; change workspace once it ends.";

/// A competing `rewind` or `change_workspace` refused while a move stops
/// jobs, before `moved` is durable: the first transition wins.
pub(crate) const MOVE_RUNNING: &str =
    "A workspace move is in progress; send the command again once it ends.";

/// `change_workspace` on a delegate: it moves with its parent instead.
const DELEGATE: &str = "A delegate cannot change workspace.";

/// `change_workspace` to the session's own workspace, compared after
/// resolving: nothing is written and no session starts.
const SAME_FOLDER: &str = "already in that folder.";

/// A late `change_workspace` refused once `moved` is durable: the session
/// takes no more commands.
pub(crate) const MOVED_CLOSING: &str = "The session was moved and takes no more commands.";

impl Loop {
    /// Takes `change_workspace` while idle (`docs/events.md`, "Change
    /// workspace"): the refusals in order, then the stop, then `moved` as
    /// the last line, naming the minted new session, the last durable line
    /// before `moved`, and the resolved target workspace, and the new
    /// session's id as the answer. Every refusal before the stop leaves
    /// the log byte for byte as it was, mints nothing, starts nothing,
    /// and stops no job. On success the loop stops late-cost lookups and
    /// writes what settled, closes admission, stops every running job and
    /// writes each end that arrives, mints the new session, appends
    /// `moved`, sets the continuation guard, drains what came after, and
    /// only then accepts, so the acknowledgement is sent after `moved` is
    /// written. The child continues from that point with no summary.
    pub(crate) fn take_change_workspace(
        &mut self,
        _id: CommandId,
        args: ChangeWorkspaceArgs,
        ack: Ack,
        input: &TurnInput,
    ) -> Result<(), Error> {
        if self.closing {
            reject(ack, ErrorCode::Closing, crate::inbox::CLOSING);
            return Ok(());
        }
        // A turn about to start: the drain already admitted a prompt or a
        // steer.
        if !input.pieces.is_empty() {
            reject(ack, ErrorCode::Busy, TURN_RUNNING);
            return Ok(());
        }
        // The pre-write check: a malformed line anywhere in the own log
        // refuses the move `log_corrupt` before any write below.
        let own_lines = log::read(self.log.dir())?;
        let Some(first) = own_lines.first() else {
            return Err(Error::NoSessionStarted);
        };
        if first.kind != "session_started" {
            return Err(Error::NoSessionStarted);
        }
        let Some(Event::SessionStarted(started)) =
            Event::from_envelope(first).map_err(Error::Unreadable)?
        else {
            return Err(Error::NoSessionStarted);
        };
        if started.parent.is_some() {
            reject(ack, ErrorCode::DelegateSession, DELEGATE);
            return Ok(());
        }
        let target = match resolve(
            &args.path,
            &self.workspace,
            self.prompt.agents_home.as_deref(),
        ) {
            Ok(target) => target,
            Err(rejection) => {
                (ack.0)(Err(rejection));
                return Ok(());
            }
        };
        // Nothing is written and no session starts, so there is no empty
        // continuation and no cache miss.
        if is_same_folder(&target, &self.workspace) {
            reject(ack, ErrorCode::InvalidArguments, SAME_FOLDER);
            return Ok(());
        }
        self.write_last_settled()?;
        // No session message enters after this: one admitted before it and
        // still behind the `change_workspace` is dropped with the drain
        // below.
        self.close_admission();
        self.close_extension_starts();
        self.stop_jobs_for_move()?;
        let new_session_id = SessionId(crate::mint("s_"));
        // The point is the last durable line before `moved`: the count
        // read fresh after settlement, never a cached pre-stop count.
        let point = self
            .log
            .count()
            .checked_sub(1)
            .ok_or(Error::NoSessionStarted)?;
        let moved = Event::Moved(Moved {
            new_session_id: new_session_id.clone(),
            seq: Seq(point),
            workspace: target.display().to_string(),
        });
        self.log.append(&moved, None, None)?;
        self.summary.observe(&moved, None);
        self.rewound = true;
        for delivery in self.deferred.drain(..) {
            refuse_after_rewound(delivery);
        }
        (ack.0)(Ok(Some(CommandResult::Rewind { new_session_id })));
        Ok(())
    }
}

/// The target workspace `path` names: a bare `~` or leading `~/`
/// expands against the hub home, a relative path resolves at admission
/// against the admitting session's workspace, and an absolute path stands
/// as written. The resolved path must exist and be a directory. An empty
/// path, an unusable home, or a missing or non-directory path is refused
/// `invalid_arguments` before anything starts, naming the path as typed.
pub(crate) fn resolve(
    typed: &str,
    workspace: &Path,
    home: Option<&Path>,
) -> Result<PathBuf, Rejection> {
    let invalid = || Rejection {
        code: ErrorCode::InvalidArguments,
        message: format!("\"{typed}\" is missing or not a directory."),
    };
    if typed.is_empty() {
        return Err(invalid());
    }
    let candidate = if typed == "~" || typed == "~/" {
        home.ok_or_else(invalid)?.to_path_buf()
    } else if let Some(rest) = typed.strip_prefix("~/") {
        home.ok_or_else(invalid)?.join(rest)
    } else {
        let path = PathBuf::from(typed);
        if path.is_absolute() {
            path
        } else {
            workspace.join(typed)
        }
    };
    let resolved = std::fs::canonicalize(&candidate).map_err(|_| invalid())?;
    if !resolved.is_dir() {
        return Err(invalid());
    }
    Ok(resolved)
}

/// Whether `target` names the session's own workspace: compared after
/// resolving, so a symlink or a trailing `/` to the same folder counts.
pub(crate) fn is_same_folder(target: &Path, workspace: &Path) -> bool {
    let current = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    target == current
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
