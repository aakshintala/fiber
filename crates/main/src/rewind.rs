//! `fiber session --rewound-from <old>`: starts the session a rewind named
//! (`docs/invocation.md`, "`rewind` starts a new session process"). The old
//! log's last line must be `rewound` naming this session; the point is the
//! session its `seq` counts in, the old session or the `from_session_id` an
//! ancestor point names. The new session runs through `run_new` with the
//! old session's worktree and no `Isolation`, so its process never removes
//! the worktree. A failure before `fiber_started` deletes its directory,
//! so the hub can start it again from `rewound`.

use std::path::PathBuf;
use std::sync::Arc;

use contract::shapes::{Failure, Point};
use contract::{Envelope, ErrorCode, SessionId};

use crate::{ask_failed, failed};

/// The internal session command with `--rewound-from`: starts `new` as the
/// continuation of `old`. A missing old log is `session_not_found`; a last
/// line that is not `rewound` naming `new` is `invalid_arguments`, before
/// any session line is written.
pub(crate) fn session_rewound(
    new: SessionId,
    old: SessionId,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    fiber: Result<PathBuf, String>,
) -> i32 {
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
    let old_dir = sessions.join(&old.0);
    if !old_dir.is_dir() {
        return ask_failed(failed(
            ErrorCode::SessionNotFound,
            format!("No session `{}`.", old.0),
        ));
    }
    let point = match point_of(log::last_line(&old_dir), &new, &old) {
        Ok(point) => point,
        Err(failure) => return ask_failed(failure),
    };
    let dir = sessions.join(&point.session_id.0);
    let forked = match r#loop::forked(&dir, point.seq) {
        Ok(forked) => forked,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let note = match r#loop::rewind_note(&dir, point.seq) {
        Ok(note) => note,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let worktree = forked.worktree.clone();
    match super::session_command::run_new(
        new,
        Vec::new(),
        None,
        false,
        worktree.clone(),
        clock,
        signals,
        fiber,
        None,
        None,
        None,
        Some(r#loop::Rewound {
            from: point,
            note,
            worktree,
        }),
    ) {
        Ok(code) => code,
        Err(failure) => super::session_command::report(signals, failure),
    }
}

/// The point `new` continues from, from the old log's last line: `seq`
/// counts in the old session's log, or in `from_session_id`'s when an
/// ancestor point names one. Anything else is `invalid_arguments`.
fn point_of(last: Option<Envelope>, new: &SessionId, old: &SessionId) -> Result<Point, Failure> {
    let Some(last) = last else {
        return Err(invalid(old, new));
    };
    if last.kind != "rewound" {
        return Err(invalid(old, new));
    }
    let rewound: contract::events::Rewound =
        serde_json::from_value(serde_json::Value::Object(last.payload))
            .map_err(|_| invalid(old, new))?;
    if rewound.new_session_id != *new {
        return Err(invalid(old, new));
    }
    Ok(Point {
        session_id: rewound.from_session_id.unwrap_or_else(|| old.clone()),
        seq: rewound.seq,
    })
}

fn invalid(old: &SessionId, new: &SessionId) -> Failure {
    failed(
        ErrorCode::InvalidArguments,
        format!(
            "Session `{}` was not rewound for session `{}`.",
            old.0, new.0
        ),
    )
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
