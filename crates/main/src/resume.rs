//! `fiber ask --resume <id>`: sends the prompt to an existing session
//! (`docs/invocation.md`, "Lifecycle"); the internal session command's
//! `--resume` serves one to clients. The log's lock is held before the
//! lines it builds from are read, so no writer adds a line between the read
//! and the first write. A session another process holds is attached to
//! instead (`docs/invocation.md`, "Processes").

use std::io;
use std::path::Path;
use std::sync::Arc;

use contract::ErrorCode;
use contract::SessionId;
use contract::events::Event;
use contract::shapes::{Failure, Worktree};
use log::Log;

use crate::{ask_failed, failed};

/// Whether the session in `dir` continues its chain as a delegate's fork:
/// its first line carries `parent` (`docs/delegates.md`, "Forks"). A
/// missing or still-empty log reads as missing, exactly as a history walk
/// treats it; any other unreadable first line reads as corrupt, failing
/// the seed closed instead of silently folding a fork's full log.
fn is_delegate_fork(dir: &log::SessionDir) -> Result<bool, log::Error> {
    match log::started_first(dir.path()) {
        Ok(started) => Ok(started.delegate),
        Err(log::FirstLineError::Incomplete) => Err(log::Error::NotFound(dir.path().to_path_buf())),
        Err(log::FirstLineError::Corrupt) => Err(log::Error::Pointer {
            path: dir.events(),
            reason: "the delegate's first line is not a session_started with a workspace"
                .to_owned(),
        }),
    }
}

/// Folds one chain segment over `carry`: a fork boundary reads `Latest` as
/// of the point, any other boundary reads it at the parent's end. A fork
/// passes its bounded lines as both the point and the full log, so later
/// writes are excluded; a rewind passes the full log, as before.
fn fold_segment(
    carry: log::Ruled,
    segment: &log::Segment,
    fork: bool,
) -> Result<log::Ruled, log::Error> {
    let lines = segment.lines(0)?;
    match segment.to {
        Some(point) => {
            if fork {
                Ok(log::fold_boundary(carry, &lines, &lines, point.0))
            } else {
                let full = log::Segment {
                    session_id: segment.session_id.clone(),
                    dir: segment.dir.clone(),
                    to: None,
                }
                .lines(0)?;
                Ok(log::fold_boundary(carry, &lines, &full, point.0))
            }
        }
        None => Ok(log::fold_from(carry, &lines, log::Fold::Resume)),
    }
}

/// Folds the chain for a fresh terminal of the named kind: interior and
/// resume-path boundaries classify by the following child's header, while
/// a bounded terminal with no follower on disk folds as the caller names.
fn seed_from(
    sessions: &Path,
    segments: &[log::Segment],
    terminal_fork: bool,
) -> Result<extensions::StateSeed, log::Error> {
    let mut carry = log::Ruled::new();
    for (index, segment) in segments.iter().enumerate() {
        let fork = match segments.get(index + 1) {
            Some(next) => is_delegate_fork(&log::SessionDir::new(sessions, &next.session_id))?,
            None => terminal_fork,
        };
        carry = fold_segment(carry, segment, fork)?;
    }
    Ok(log::seed(carry))
}

/// The extension seed for a resumed session: each `to: Some` boundary folds
/// through the point the following child continues from, `Latest` as of the
/// point when that child carries `parent` (a fork) and at the parent's end
/// otherwise (a rewind, and any bounded segment with no follower).
pub(crate) fn chain_seed(
    sessions: &Path,
    segments: &[log::Segment],
) -> Result<extensions::StateSeed, log::Error> {
    seed_from(sessions, segments, false)
}

/// The extension seed for a fresh fork's first run over `history_to`: the
/// terminal is bounded by the fork's point with no child on disk yet, so it
/// folds as a fork; interior boundaries classify as [`chain_seed`] does.
pub(crate) fn fork_seed(
    sessions: &Path,
    segments: &[log::Segment],
) -> Result<extensions::StateSeed, log::Error> {
    seed_from(sessions, segments, true)
}

/// `fiber ask --resume <selector>`: resolves the selector, opens the
/// session's log, folds what the resume needs, and runs one turn on it. A
/// held lock attaches instead of opening a second writer; every failure
/// before `fiber_started` is written leaves the log byte for byte as it was.
/// `credential` is `--credential`'s label, switching the session to it.
pub(crate) struct Resuming {
    /// The session to resume, as typed.
    pub(crate) selector: String,
    /// The credential label to switch the session to, if any.
    pub(crate) credential: Option<String>,
}

pub(crate) fn ask_resume(
    resuming: Resuming,
    overrides: Vec<String>,
    prompt: String,
    signals: &Arc<doors::Signals>,
    process: &crate::startup::Process,
) -> i32 {
    let clock = Arc::clone(&process.clock);
    let (home, workspace) = match crate::startup::startup() {
        Ok(directories) => directories,
        Err(e) => return ask_failed(e),
    };
    let project = worktree::project(&workspace);
    let sessions = log::sessions_dir(&home, &project);
    // One memo serves the lookup and the launch-project check below, so
    // every session in one workspace runs the identity once.
    let in_project = crate::open::in_project(&project, &worktree::project);
    let id = match log::resolve(&sessions, &resuming.selector, &in_project) {
        Ok(id) => id,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    // The recorded worktree may be gone: the lookup above reads the
    // recorded repository, but a resume runs in the recorded workspace, so
    // the launch project is re-applied to it here, unless the missing
    // worktree is recreated from its repository first. A session outside
    // this project is refused exactly as an unknown id: the log is
    // untouched, no lock is taken and nothing attaches.
    let dir = log::SessionDir::new(&sessions, &id);
    let recorded = recorded_worktree(dir.path());
    if resume_refused(recorded.as_ref(), dir.path(), &in_project) {
        let e = log::Error::NotFound(sessions.clone());
        return ask_failed(failed(e.code(), e));
    }
    // A resume that attaches to a live session names the crash file by the
    // session it attaches to, as the TUI does.
    log::crash::attach(&id);
    // `Log::open` takes the lock first; the log is folded under it.
    // A held lock means a live session: attach to it instead of opening a
    // second writer (`docs/invocation.md`, "Processes"). Its failure
    // already names the holder, which attach takes as its refusal.
    let log = match Log::open(&sessions, id.clone(), Arc::clone(&clock)) {
        Ok(log) => Arc::new(log),
        Err(e @ log::Error::Held { .. }) => {
            return match doors::attach(
                &home,
                &id,
                resuming.credential.as_deref(),
                prompt,
                &mut io::stdout(),
                failed(e.code(), e),
            ) {
                Ok(code) => code,
                Err(e) => ask_failed(Failure::from(e)),
            };
        }
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let dir = log::SessionDir::new(&sessions, &id);
    resume_session(
        log,
        &sessions,
        &dir,
        id,
        overrides,
        resuming.credential,
        Some(prompt),
        true,
        false,
        clock,
        signals,
        process,
    )
}

/// Whether the session in `dir` runs in the launch project: its recorded
/// workspace passes the predicate the lookup applied, so both share one
/// memo. A missing log, or a first line that fails to read or parse as
/// `session_started`, is refused the same way; the lookup just accepted it,
/// so that is unreachable short of a concurrent delete.
fn in_launch_project(dir: &Path, in_project: &dyn Fn(&str) -> bool) -> bool {
    let Ok(lines) = log::lines(dir) else {
        return false;
    };
    let mut lines = lines;
    let Some(Ok(line)) = lines.next() else {
        return false;
    };
    let Ok(Some(Event::SessionStarted(started))) = Event::from_envelope(&line) else {
        return false;
    };
    in_project(&started.workspace)
}

/// Whether `ask --resume` refuses the session as unknown: a missing worktree
/// with a recorded repository is recreated first, so it resumes even outside
/// the launch project; anything else must pass the launch-project predicate.
/// The recreatable check runs first, so `in_project` is not called for a
/// recreatable worktree.
fn resume_refused(
    recorded: Option<&Worktree>,
    dir: &Path,
    in_project: &dyn Fn(&str) -> bool,
) -> bool {
    !recreatable(recorded) && !in_launch_project(dir, in_project)
}

/// The internal session command with `--resume`: resumes the session `id`
/// in the current directory's project to serve clients
/// (`docs/invocation.md`, "The hub"). The hub attaches to a live session by
/// its socket, so a held lock fails instead of attaching. With a prompt,
/// which only a delegate's resume carries, the prompt runs as one
/// unattended turn, then the session exits, as a new delegate does.
pub(crate) fn session_resume(
    id: SessionId,
    overrides: Vec<String>,
    credential: Option<String>,
    prompt: Option<String>,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    process: &crate::startup::Process,
) -> i32 {
    let (home, workspace) = match crate::startup::startup() {
        Ok(directories) => directories,
        Err(e) => return ask_failed(e),
    };
    let sessions = log::sessions_dir(&home, &worktree::project(&workspace));
    log::crash::attach(&id);
    let log = match Log::open(&sessions, id.clone(), Arc::clone(&clock)) {
        Ok(log) => Arc::new(log),
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let dir = log::SessionDir::new(&sessions, &id);
    let one_turn = prompt.is_some();
    resume_session(
        log, &sessions, &dir, id, overrides, credential, prompt, one_turn, true, clock, signals,
        process,
    )
}

/// One function runs every resumed session: it enters the recorded
/// worktree, making it again when it was removed, runs the session, ends
/// a recreated worktree by the same rules, and only then reports a
/// startup failure. `log` holds the session lock; it is released before
/// `end`, whose scan holds the resumed session's own directory, and a
/// signal recorded during `end` reaches `report`.
#[allow(
    clippy::too_many_arguments,
    reason = "one resumed session needs its log, directory, id, mode, clock, signals and recorded executable"
)]
fn resume_session(
    log: Arc<Log>,
    sessions: &Path,
    dir: &log::SessionDir,
    id: SessionId,
    overrides: Vec<String>,
    credential: Option<String>,
    prompt: Option<String>,
    one_turn: bool,
    enter_present: bool,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    process: &crate::startup::Process,
) -> i32 {
    // One reads for the process, cancelled when this returns whatever the
    // result: the guard drops after `end`, so a signal during the end is
    // still answered.
    let reads = Arc::new(crate::switch::Reads::default());
    let _cancel = crate::switch::CancelOnDrop(Arc::clone(&reads));
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let mut held = match enter_worktree(
        recorded_worktree(dir.path()),
        &home,
        &id,
        &clock,
        signals,
        &reads,
        enter_present,
    ) {
        Ok(held) => held,
        Err(error) => {
            drop(log);
            let isolation = error.isolation;
            return finish(
                move || {
                    if let Some(isolation) = isolation {
                        isolation.end();
                    }
                },
                Err(Failure::from(error.failure)),
                |failure| crate::session_command::report(signals, failure),
            );
        }
    };
    // A recreated worktree is the session's workspace: configuration
    // reads from there, as a new session's does. A kept worktree keeps
    // the launch directory, as today.
    // The create line's opener is taken before the hold is rebuilt, so
    // the later `resumed_session` opens the same slot `end` reads.
    let mut opened: Option<doors::WorktreeOpener> = None;
    if let Held::Made(isolation) = held {
        opened = Some(isolation.opener());
        let path = isolation.path().to_path_buf();
        if let Err(source) = std::env::set_current_dir(&path) {
            drop(log);
            return finish(
                move || isolation.end(),
                Err(failed(
                    ErrorCode::IoFailed,
                    format!("{}: {}.", path.display(), source.kind()),
                )),
                |failure| crate::session_command::report(signals, failure),
            );
        }
        held = Held::Made(isolation);
    }
    // Every resume but a recreation keeps the session lock till the
    // process dies, as a resumed session always did: the hub sequences a
    // rewind's next start on the exiting process releasing it, so freeing
    // it at `close` lets the next start overlap this process dying and the
    // hub drops the next session. A recreated worktree takes no hold:
    // `end` must see the session free to remove it.
    if !matches!(held, Held::Made(_)) {
        std::mem::forget(Arc::clone(&log));
    }
    let result = resumed_session(
        log,
        sessions,
        dir,
        id,
        overrides,
        credential,
        prompt,
        one_turn,
        clock,
        signals,
        process,
        Arc::clone(&reads),
        opened,
    );
    finish(
        move || {
            if let Held::Made(isolation) = held {
                isolation.end();
            }
        },
        result,
        |failure| crate::session_command::report(signals, failure),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "one resumed session needs its log, directory, id, mode, clock, signals and recorded executable"
)]
fn resumed_session(
    log: Arc<Log>,
    sessions: &Path,
    dir: &log::SessionDir,
    id: SessionId,
    overrides: Vec<String>,
    credential: Option<String>,
    prompt: Option<String>,
    one_turn: bool,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    process: &crate::startup::Process,
    reads: Arc<crate::switch::Reads>,
    opened: Option<doors::WorktreeOpener>,
) -> Result<i32, Failure> {
    let folded = match r#loop::resumed(dir.path()) {
        Ok(folded) => folded,
        Err(e) => return Err(failed(e.code(), e)),
    };
    // The per-run layer first (`--model`, then `-c model=`), then the
    // recorded model, then configuration's default. A
    // model that no longer resolves fails here, before any session line is
    // written. Configuration and the project key still come from the launch
    // directory; the workspace is the first `session_started`'s, wherever
    // the resume runs. The recorded thinking level is read before `folded`
    // moves into `Loop::resume`.
    let recorded_thinking = folded
        .thinking
        .as_deref()
        .and_then(|level| level.parse::<contract::ThinkingLevel>().ok());
    crate::shutdown::arm(signals, &reads);
    let fiber_path = process.exe.clone()?;
    // The diagnostic process: a one-turn resume is an ask; a resumed
    // session keeps its session identity.
    let diag_process = if one_turn {
        log::diag::Process::Ask
    } else {
        log::diag::Process::Session
    };
    let seed = chain_seed(
        sessions,
        &log::history(dir.path()).map_err(|error| failed(error.code(), error))?,
    )
    .map_err(|error| failed(error.code(), error))?;
    let parts = crate::parts_with_session(
        overrides,
        folded.model.as_deref(),
        crate::credential::Labels::new(credential.as_deref(), folded.credential.as_deref()),
        recorded_thinking,
        Arc::clone(&clock),
        None,
        Arc::clone(&reads),
        diag_process,
        seed,
        crate::session_extensions::RepositoryScope {
            workspace: Some(std::path::PathBuf::from(&folded.workspace)),
            skipped: folded.skipped(),
        },
        crate::parts::SessionLaunch {
            id: id.clone(),
            reason: contract::events::ExtensionReason::Resume,
            event_cursor: log.count(),
        },
        fiber_path.clone(),
    )?;
    let resume = crate::session_command::ResumeStart::new(
        log::SessionDir::new(sessions, &id),
        Arc::clone(&log),
        folded,
        opened,
    )?;
    let inputs = crate::session_command::SessionInputs {
        prompt,
        one_turn,
        start: crate::session_command::Start::Resume(resume),
        fiber_path: fiber_path.clone(),
        reads: Arc::clone(&reads),
    };
    crate::session_command::assemble_session(parts, inputs, signals, process)
}

/// The first line's `session_started` worktree, when it records one. Any
/// read or parse failure is none: the lookup already matched this session
/// into the launch project, so that is unreachable short of a concurrent
/// delete.
fn recorded_worktree(dir: &Path) -> Option<Worktree> {
    let lines = log::lines(dir).ok()?;
    let mut lines = lines;
    let line = lines.next()?;
    let line = line.ok()?;
    match Event::from_envelope(&line) {
        Ok(Some(Event::SessionStarted(started))) => started.worktree,
        Ok(_) | Err(_) => None,
    }
}

/// Whether the recorded worktree is recreated: its repository is recorded
/// and its path is gone. Anything else, a present path, another lookup
/// error or no recorded repository, resumes where the worktree is, with
/// no recreation. A dangling symlink at the path exists.
fn recreatable(recorded: Option<&Worktree>) -> bool {
    match recorded {
        Some(worktree) => {
            worktree.repository.is_some()
                && std::fs::symlink_metadata(Path::new(&worktree.path))
                    .is_err_and(|source| source.kind() == std::io::ErrorKind::NotFound)
        }
        None => false,
    }
}

/// What a resumed process holds its worktree with.
enum Held {
    /// No recorded worktree, or one that cannot be recreated: the process
    /// runs without a hold.
    Nothing,
    /// A share of a worktree the process did not make, held for its life:
    /// the hold is the ownership itself, dropped when the process ends.
    Shared { _shared: doors::Shared },
    /// A worktree the process made again, ended by the same rules.
    Made(doors::Isolation),
}

/// Enters the recorded worktree: a present one is shared for the
/// process's life, and a missing one with a recorded repository is made
/// again at the same path and branch before anything else runs. The
/// recreation arms the shutdown first, so a signal during the
/// hook-running `git worktree add` kills it and rolls back. A share or
/// recreation failure carries no isolation: nothing was made. A
/// downgrade failure after the worktree was made reaches the caller, so
/// its isolation is ended after the session lock is released.
// Only a non-NotFound error reaches the other arm; there both arms fail
// opening the lock beside the path with the same IoFailed sentence, so no
// assertion tells them apart.
#[cfg_attr(false, mutants::skip)]
#[allow(
    clippy::result_large_err,
    reason = "the caller matches the isolation out of the error by value"
)]
fn enter_worktree(
    recorded: Option<Worktree>,
    home: &Path,
    id: &SessionId,
    clock: &Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    reads: &Arc<crate::switch::Reads>,
    enter_present: bool,
) -> Result<Held, doors::RecreateError> {
    let Some(worktree) = recorded else {
        return Ok(Held::Nothing);
    };
    let path = Path::new(&worktree.path);
    match std::fs::symlink_metadata(path) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => match worktree.repository {
            Some(repository) => {
                crate::shutdown::arm(signals, reads);
                match doors::recreate(
                    Path::new(&repository),
                    path,
                    &worktree.branch,
                    home,
                    id,
                    Arc::clone(clock),
                    &|command| reads.command(command),
                ) {
                    Ok(doors::Recreated::Made(isolation)) => Ok(Held::Made(isolation)),
                    Ok(doors::Recreated::Present(shared)) => enter(path, shared, enter_present),
                    Err(error) => Err(error),
                }
            }
            None => Ok(Held::Nothing),
        },
        Ok(_) | Err(_) => match doors::share(path) {
            Ok(shared) => enter(path, shared, enter_present),
            Err(failure) => Err(doors::RecreateError {
                failure,
                isolation: None,
            }),
        },
    }
}

/// Enters a present worktree: both arms the hub's launch directory can
/// meet, the recreation's Present result and a path present at the
/// metadata check. On the hub path the process runs in the worktree, as a
/// made one does; `ask --resume` keeps the launch directory. A path that
/// vanished after the claim fails with
/// the made arm's sentence and no isolation: the share drops with it.
#[allow(
    clippy::result_large_err,
    reason = "the caller matches the isolation out of the error by value"
)]
fn enter(
    path: &Path,
    shared: doors::Shared,
    enter_present: bool,
) -> Result<Held, doors::RecreateError> {
    if enter_present && let Err(source) = std::env::set_current_dir(path) {
        return Err(doors::RecreateError {
            failure: doors::SessionError::Forwarded(failed(
                ErrorCode::IoFailed,
                format!("{}: {}.", path.display(), source.kind()),
            )),
            isolation: None,
        });
    }
    Ok(Held::Shared { _shared: shared })
}

/// Ends the isolation, then answers the resume's result: a signal
/// recorded during `end` reaches `report`, which the recorded signal
/// answers before it prints, so a failure after recreation and before
/// `fiber_started` exits with the signal's code and silence.
fn finish(
    end: impl FnOnce(),
    result: Result<i32, Failure>,
    report: impl FnOnce(Failure) -> i32,
) -> i32 {
    end();
    match result {
        Ok(code) => code,
        Err(failure) => report(failure),
    }
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
