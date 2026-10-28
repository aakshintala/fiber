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
//! When the old log's last line is `moved` naming this session, the same
//! flag starts the moved child instead (`docs/invocation.md`, "Change
//! workspace"): the hub starts the process in the old session's workspace,
//! and it moves to the `moved` target workspace itself before running
//! through `run_new` with no worktree and no rewind note or summary.
//!
//! `fiber session --forked-from <parent> --forked-seq <n>`: starts the
//! session a `delegate_fork` call named (`docs/delegates.md`, "Forks").
//! The point is named outright, so no last line is checked; the fold must
//! hold a build before it, or the child cannot start.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::events::{Moved, Parent};
use contract::shapes::{Failure, Point};
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
    let sessions = log::sessions_dir(&home, &worktree::project(&workspace));
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
    inherited: Vec<PathBuf>,
    adopted: Option<contract::shapes::Worktree>,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let context = match context() {
        Ok(context) => context,
        Err(code) => return code,
    };
    let sessions = context.sessions;
    let from_dir = log::SessionDir::new(&sessions, &from.session_id);
    // A missing session is `session_not_found`; any other failure to
    // read its directory (permissions, a broken link) is `io_failed`
    // and names the path (`docs/errors.md`).
    let missing = match from_dir.require_dir() {
        Ok(()) => false,
        Err(log::Error::NotFound(_)) => true,
        Err(log::Error::Io { path, source }) => {
            return ask_failed(failed(
                ErrorCode::IoFailed,
                format!(
                    "Cannot read session directory `{}`: {source}",
                    path.display()
                ),
            ));
        }
        Err(error) => {
            return ask_failed(failed(error.code(), error));
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
    let folded = match r#loop::forked(from_dir.path(), from.seq) {
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
            Err(failure) => return crate::session_command::report(signals, Failure::from(failure)),
        },
        None => None,
    };
    // A delegate's stdin is its lifeline: the parent holds the write end
    // open and never writes to it (`docs/delegates.md`, "Lifetime").
    // Installed once the child will run, so a failing fork still
    // reports its error instead of dying on a closed stdin.
    signals.lifeline(Box::new(std::io::stdin()));
    let parent_dir = log::SessionDir::new(&sessions, &parent.session_id);
    let parent_workspace = match log::started_first(parent_dir.path()) {
        Ok(started) => match started.workspace {
            Some(workspace) => workspace,
            None => {
                return ask_failed(failed(
                    ErrorCode::LogCorrupt,
                    "The parent has no workspace.",
                ));
            }
        },
        Err(_) => {
            return ask_failed(failed(
                ErrorCode::LogCorrupt,
                format!(
                    "Cannot read the parent workspace from {}.",
                    parent_dir.path().display()
                ),
            ));
        }
    };
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(error) => return ask_failed(failed(ErrorCode::IoFailed, error)),
    };
    let notice = if workspace == Path::new(&parent_workspace) {
        String::new()
    } else {
        r#loop::fork_workspace(&workspace.to_string_lossy(), &parent_workspace)
    };
    let task = r#loop::fork_task(&parent.session_id, depth, &prompt, &notice);
    match super::session_command::run_new(
        new,
        Vec::new(),
        Some(task),
        true,
        adopted.or_else(|| worktree.clone()),
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
        inherited,
    ) {
        Ok(code) => code,
        Err(failure) => super::session_command::report(signals, failure),
    }
}
/// The directory holding `point`'s session: the history segment when the
/// old log's chain names it (a rewind to a point in another project, after
/// a workspace move), else the session beside the old one, as before.
fn point_dir(old: &Path, sessions: &Path, point: &Point) -> PathBuf {
    if let Ok(chain) = log::history(old)
        && let Some(segment) = chain
            .into_iter()
            .find(|segment| segment.session_id == point.session_id)
    {
        return segment.dir;
    }
    log::SessionDir::new(sessions, &point.session_id)
        .path()
        .to_path_buf()
}

/// The project key of the session in `dir`: the key its project-qualified
/// directory names (`home/projects/<key>/sessions/<id>`), never the
/// recorded workspace, which may have moved or vanished since the session
/// ran. A directory outside that layout is `log_corrupt`, before any
/// session line is written, as a fork's missing parent workspace is.
fn ancestor_key(dir: &Path) -> Result<String, i32> {
    let invalid = || {
        ask_failed(failed(
            ErrorCode::LogCorrupt,
            format!("Cannot read the point's project from {}.", dir.display()),
        ))
    };
    let sessions = dir.parent().ok_or_else(invalid)?;
    if sessions.file_name().and_then(|name| name.to_str()) != Some("sessions") {
        return Err(invalid());
    }
    let key_dir = sessions.parent().ok_or_else(invalid)?;
    let key = key_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(invalid)?;
    if key_dir
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        != Some("projects")
    {
        return Err(invalid());
    }
    Ok(key.to_owned())
}

/// The internal session command with `--rewound-from`: starts `new` as the
/// continuation of `old`. A missing old log is `session_not_found`; a last
/// line that is not `rewound` naming `new` is `invalid_arguments`, before
/// any session line is written. A last line that is `moved` naming `new`
/// starts the moved child in its target workspace instead.
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
    let sessions = &context.sessions;
    let old_dir = log::SessionDir::new(sessions, &old);
    if !log::dir_exists(old_dir.path()) {
        return ask_failed(failed(
            ErrorCode::SessionNotFound,
            format!("No session `{}`.", old.0),
        ));
    }
    let last = log::last_line(old_dir.path());
    if last.as_ref().is_some_and(|line| line.kind == "moved") {
        return session_moved(new, old, last, context, clock, signals, fiber);
    }
    let (mut point, summary) = match r#loop::rewound_from(last, &new, &old) {
        Ok(point) => point,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let dir = point_dir(old_dir.path(), sessions, &point);
    // A rewind to a point in another project records the ancestor's
    // project, read off the point's project-qualified directory so a
    // recorded workspace that has since moved or vanished cannot select
    // the wrong project. A point in the child's own project records
    // nothing, as before. The child runs where this process started: the
    // hub spawned it in the old session's recorded workspace.
    let child_workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(error) => return ask_failed(failed(ErrorCode::IoFailed, error)),
    };
    let child_key = log::project_key(&worktree::project(&child_workspace));
    match ancestor_key(&dir) {
        Ok(ancestor) if ancestor != child_key => {
            point.project = Some(ancestor);
        }
        Ok(_) => {}
        Err(code) => return code,
    }
    let forked = match r#loop::forked(&dir, point.seq) {
        Ok(forked) => forked,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let note = match r#loop::rewind_note_with_jobs(old_dir.path(), &dir, point.seq) {
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
            Err(failure) => return crate::session_command::report(signals, Failure::from(failure)),
        },
        None => None,
    };
    // The name is the rewound session's, not the point's session's.
    let name = r#loop::latest_name(old_dir.path());
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
        Vec::new(),
    ) {
        Ok(code) => code,
        Err(failure) => super::session_command::report(signals, failure),
    }
}

/// The `--rewound-from` entry when the old log ends `moved` naming `new`:
/// starts `new` as the continuation of `old` in the moved target workspace
/// (`docs/invocation.md`, "Change workspace"). The hub starts this process
/// in the old session's workspace; the target comes from the durable
/// `moved` line alone. A `moved` naming another session, or one that does
/// not parse, is `invalid_arguments`; a missing target is `io_failed`
/// naming the path, and the old session stays ended `moved`. The child
/// records no worktree and no rewind note or summary: it continues the old
/// session at the last durable line before `moved`, with the old session's
/// latest name. Its `forked_from.project` names the parent's project only
/// when it differs from the child's. Target configuration, instructions,
/// MCP, extensions and repository approval inputs load through the
/// new-session path, in the target workspace, since the process moves
/// there before `run_new` reads them.
#[allow(
    clippy::too_many_arguments,
    reason = "one internal session entry: the move record, the source context and the process inputs"
)]
fn session_moved(
    new: SessionId,
    old: SessionId,
    last: Option<contract::Envelope>,
    context: Context,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let not_moved = || {
        ask_failed(failed(
            ErrorCode::InvalidArguments,
            format!("Session `{}` was not moved for session `{}`.", old.0, new.0),
        ))
    };
    let Some(last) = last else {
        return not_moved();
    };
    let moved: Moved = match serde_json::from_value(serde_json::Value::Object(last.payload)) {
        Ok(moved) => moved,
        Err(_) => return not_moved(),
    };
    if moved.new_session_id != new {
        return not_moved();
    }
    // The hub started this process in the old session's workspace, so the
    // parent project below is the old session's.
    let source = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(error) => return ask_failed(failed(ErrorCode::IoFailed, error)),
    };
    if !Path::new(&moved.workspace).is_dir() {
        return ask_failed(failed(
            ErrorCode::IoFailed,
            format!("`{}` no longer exists.", moved.workspace),
        ));
    }
    if let Err(error) = std::env::set_current_dir(&moved.workspace) {
        return ask_failed(failed(
            ErrorCode::IoFailed,
            format!("{}: {error}", moved.workspace),
        ));
    }
    let parent_key = log::project_key(&worktree::project(&source));
    let child_key = log::project_key(&worktree::project(Path::new(&moved.workspace)));
    let point = Point {
        session_id: old.clone(),
        seq: moved.seq,
        project: (parent_key != child_key).then_some(parent_key),
    };
    // The fold reads the old session where it lives: a corrupt old log
    // fails here, before any session line is written.
    let old_dir = log::SessionDir::new(&context.sessions, &old);
    if let Err(e) = r#loop::forked(old_dir.path(), moved.seq) {
        return ask_failed(failed(e.code(), e));
    }
    // The name is the moved session's, as a rewind keeps the rewound one.
    let name = r#loop::latest_name(old_dir.path());
    match super::session_command::run_new(
        new,
        Vec::new(),
        None,
        false,
        None,
        clock,
        signals,
        fiber,
        None,
        None,
        None,
        Some(r#loop::Rewound {
            factory: Some(crate::builtin::tool_search_factory()),
            from: point,
            worktree: None,
            how: r#loop::Continuation::Rewind {
                note: String::new(),
                summary: None,
                name,
            },
        }),
        context.reads,
        false,
        Vec::new(),
    ) {
        Ok(code) => code,
        Err(failure) => super::session_command::report(signals, failure),
    }
}
