//! `fiber session --rewound-from <old>`: starts the session a rewind named
//! (`docs/invocation.md`, "`rewind` starts a new session process"). The old
//! log's last line must be `rewound` naming this session; the point is the
//! session its `seq` counts in, the old session or the `from_session_id` an
//! ancestor point names. The new session runs through `run_new` with the
//! old session's worktree and no `Isolation`, so its process never removes
//! the worktree. The continuing process holds the shared lock for its life.
//! A failure before `fiber_started` deletes its directory,
//! so the hub can start it again from `rewound`.
//!
//! `fiber session --forked-from <parent> --forked-seq <n>`: starts the
//! session a `delegate_fork` call named (`docs/delegates.md`, "Forks").
//! The point is named outright, so no last line is checked; the fold must
//! hold a build before it, or the child cannot start.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::events::Parent;
use contract::shapes::Point;
use contract::{ErrorCode, SessionId};

use crate::{ask_failed, failed};

/// The process context both session starts share: one reads for the
/// process with its cancel guard, and the sessions dir. Home and current
/// directory failures report before any session line is written, exactly
/// as each function's inlined prologue did. The caller holds the context
/// to the end of its run, so the reads stay open until the function
/// returns whatever the result; there is no worktree isolation to outlive
/// here.
struct Context {
    /// The sessions dir: `<home>/<project>/sessions`.
    sessions: PathBuf,
    /// One reads for the process.
    reads: Arc<crate::switch::Reads>,
    /// Cancels the reads when the caller returns.
    _cancel: crate::switch::CancelOnDrop,
}

fn context() -> Result<Context, i32> {
    // One reads for the process, cancelled when the caller returns
    // whatever the result.
    let reads = Arc::new(crate::switch::Reads::default());
    let cancel = crate::switch::CancelOnDrop(Arc::clone(&reads));
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(e) => return Err(ask_failed(failed(e.code(), e))),
    };
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(e) => {
            return Err(ask_failed(failed(
                ErrorCode::IoFailed,
                format!("the current directory: {e}"),
            )));
        }
    };
    let sessions = log::sessions_dir(&home, &doors::project(&workspace));
    Ok(Context {
        sessions,
        reads,
        _cancel: cancel,
    })
}

/// The internal session command with `--forked-from`/`--forked-seq`:
/// starts `new` as the fork continuing `from`. A missing `from` log is
/// `session_not_found`; a fold failure carries its code; a point with no
/// `preamble_built` before it is `invalid_arguments`, since there is no
/// build to continue from. Each fails before any session line is written.
/// Otherwise the child holds `doors::share` on the inherited worktree
/// for its life and runs one turn with the `fork_task` prompt, so it
/// exits when its run finishes (`docs/delegates.md`, "Lifetime").
#[allow(
    clippy::too_many_arguments,
    reason = "one internal session entry: the fork's point, parent, depth and prompt with the process inputs"
)]
pub(crate) fn session_forked(
    new: SessionId,
    from: Point,
    parent: Parent,
    depth: u32,
    prompt: String,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let context = match context() {
        Ok(context) => context,
        Err(code) => return code,
    };
    let sessions = context.sessions;
    let from_dir = sessions.join(&from.session_id.0);
    // A missing session is `session_not_found`; any other failure to
    // read its directory (permissions, a broken link) is `io_failed`
    // and names the path (`docs/errors.md`). `is_dir` folds both into
    // `false`, so the metadata error is matched instead.
    let missing = match std::fs::metadata(&from_dir) {
        Ok(meta) => !meta.is_dir(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            return ask_failed(failed(
                ErrorCode::IoFailed,
                format!(
                    "Cannot read session directory `{}`: {e}",
                    from_dir.display()
                ),
            ));
        }
    };
    if missing {
        return ask_failed(failed(
            ErrorCode::SessionNotFound,
            format!("No session `{}`.", from.session_id.0),
        ));
    }
    // The fold reads the point's history: a corrupt log fails with its
    // code, before any session line is written.
    let folded = match r#loop::forked(&from_dir, from.seq) {
        Ok(folded) => folded,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    // A fork continues the point's build, sending its preamble verbatim
    // as its first request: with no `preamble_built` before the point
    // there is no build, so the child cannot start.
    if folded.model.is_none() {
        return ask_failed(failed(
            ErrorCode::InvalidArguments,
            format!(
                "Session `{}` has no `preamble_built` before seq {}, so there is no build to fork from.",
                from.session_id.0, from.seq.0,
            ),
        ));
    }
    let worktree = folded.worktree.clone();
    // The fork runs in the inherited worktree it did not create: it
    // shares the worktree's lock file for its life, so a recreation or
    // a removal never takes the worktree under it.
    let _shared = match worktree.as_ref() {
        Some(worktree) => match doors::share(Path::new(&worktree.path)) {
            Ok(shared) => Some(shared),
            Err(failure) => return crate::session_command::report(signals, failure),
        },
        None => None,
    };
    // A delegate's stdin is its lifeline: the parent holds the write end
    // open and never writes to it (`docs/delegates.md`, "Lifetime").
    // Installed once the child will run, so a failing fork still
    // reports its error instead of dying on a closed stdin.
    signals.lifeline(Box::new(std::io::stdin()));
    let task = r#loop::fork_task(&parent.session_id, depth, &prompt);
    match super::session_command::run_new(
        new,
        Vec::new(),
        Some(task),
        true,
        worktree.clone(),
        clock,
        signals,
        fiber,
        None,
        None,
        Some(parent.clone()),
        Some(r#loop::Rewound {
            factory: Some(crate::builtin::tool_search_factory()),
            from,
            worktree,
            how: r#loop::Continuation::Fork { parent },
        }),
        context.reads,
        false,
    ) {
        Ok(code) => code,
        Err(failure) => super::session_command::report(signals, failure),
    }
}
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
    let context = match context() {
        Ok(context) => context,
        Err(code) => return code,
    };
    let sessions = context.sessions;
    let old_dir = sessions.join(&old.0);
    if !old_dir.is_dir() {
        return ask_failed(failed(
            ErrorCode::SessionNotFound,
            format!("No session `{}`.", old.0),
        ));
    }
    let (point, summary) = match r#loop::rewound_from(log::last_line(&old_dir), &new, &old) {
        Ok(point) => point,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let dir = sessions.join(&point.session_id.0);
    let forked = match r#loop::forked(&dir, point.seq) {
        Ok(forked) => forked,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let note = match r#loop::rewind_note_with_jobs(&old_dir, &dir, point.seq) {
        Ok(note) => note,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let worktree = forked.worktree.clone();
    // The continuing process runs in the recorded worktree it did not
    // create: it shares the worktree's lock file for its life, so a
    // recreation or a removal never takes the worktree under it.
    let _shared = match forked.worktree.as_ref() {
        Some(worktree) => match doors::share(Path::new(&worktree.path)) {
            Ok(shared) => Some(shared),
            Err(failure) => return crate::session_command::report(signals, failure),
        },
        None => None,
    };
    // The name is the rewound session's, not the point's session's.
    let name = r#loop::latest_name(&old_dir);
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
            factory: Some(crate::builtin::tool_search_factory()),
            from: point,
            worktree,
            how: r#loop::Continuation::Rewind {
                note,
                summary,
                name,
            },
        }),
        context.reads,
        false,
    ) {
        Ok(code) => code,
        Err(failure) => super::session_command::report(signals, failure),
    }
}
