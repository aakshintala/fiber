//! `fiber ask --resume <id>`: sends the prompt to an existing session
//! (`docs/invocation.md`, "Lifecycle"); the internal session command's
//! `--resume` serves one to clients. The log's lock is held before the
//! lines it builds from are read, so no writer adds a line between the read
//! and the first write. A session another process holds is attached to
//! instead (`docs/invocation.md`, "Processes").

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::ErrorCode;
use contract::SessionId;
use contract::events::Event;
use contract::shapes::{Failure, Worktree};
use doors::Session;
use log::Log;
use r#loop::Loop;

use crate::session_command::close;
use crate::{ask_failed, failed, run_turn};

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

impl Resuming {
    /// What `fiber ask --resume` resumes: the session and the credential
    /// label to switch it to.
    pub(crate) fn new(selector: String, credential: Option<String>) -> Self {
        Self {
            selector,
            credential,
        }
    }
}

pub(crate) fn ask_resume(
    resuming: Resuming,
    overrides: Vec<String>,
    prompt: String,
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
                contract::ErrorCode::IoFailed,
                format!("the current directory: {e}"),
            ));
        }
    };
    let project = doors::project(&workspace);
    let sessions = log::sessions_dir(&home, &project);
    let id = match log::resolve(&sessions, &resuming.selector, &|started| {
        doors::project(Path::new(started)) == project
    }) {
        Ok(id) => id,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    // The recorded worktree may be gone: the lookup above reads the
    // recorded repository, but a resume runs in the recorded workspace, so
    // the launch project is re-applied to it here, unless the missing
    // worktree is recreated from its repository first. A session outside
    // this project is refused exactly as an unknown id: the log is
    // untouched, no lock is taken and nothing attaches.
    let dir = sessions.join(&id.0);
    let recorded = recorded_worktree(&dir);
    if !recreatable(recorded.as_ref()) && !in_launch_project(&dir, &project) {
        let e = log::Error::NotFound(sessions.clone());
        return ask_failed(failed(e.code(), e));
    }
    // A resume that attaches to a live session names the crash file by the
    // session it attaches to, as the TUI does.
    crate::crash::attach(&id);
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
                Err(e) => ask_failed(e),
            };
        }
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let dir = sessions.join(&id.0);
    resume_session(
        log,
        &dir,
        id,
        overrides,
        resuming.credential,
        Some(prompt),
        true,
        clock,
        signals,
        fiber,
    )
}

/// Whether the session in `dir` runs in the launch project: its recorded
/// workspace resolves to the same project, the same test the lookup
/// applied. A missing log, or a first line that fails to read or parse as
/// `session_started`, is refused the same way; the lookup just accepted it,
/// so that is unreachable short of a concurrent delete.
fn in_launch_project(dir: &Path, project: &Path) -> bool {
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
    doors::project(Path::new(&started.workspace)) == project
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
    prompt: Option<String>,
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
                contract::ErrorCode::IoFailed,
                format!("the current directory: {e}"),
            ));
        }
    };
    let sessions = log::sessions_dir(&home, &doors::project(&workspace));
    crate::crash::attach(&id);
    let log = match Log::open(&sessions, id.clone(), Arc::clone(&clock)) {
        Ok(log) => Arc::new(log),
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let dir = sessions.join(&id.0);
    let one_turn = prompt.is_some();
    resume_session(
        log, &dir, id, overrides, None, prompt, one_turn, clock, signals, fiber,
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
    dir: &Path,
    id: SessionId,
    overrides: Vec<String>,
    credential: Option<String>,
    prompt: Option<String>,
    one_turn: bool,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    fiber: Result<PathBuf, String>,
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
    let mut held = match enter_worktree(recorded_worktree(dir), &home, &id, &clock, signals, &reads)
    {
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
                Err(error.failure),
                |failure| crate::session_command::report(signals, failure),
            );
        }
    };
    // A recreated worktree is the session's workspace: configuration
    // reads from there, as a new session's does. A kept worktree keeps
    // the launch directory, as today.
    if let Held::Made(isolation) = held {
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
        dir,
        id,
        overrides,
        credential,
        prompt,
        one_turn,
        clock,
        signals,
        fiber,
        Arc::clone(&reads),
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

/// One function builds and runs every resumed session, as `new_session`
/// does every new one: `fiber ask --resume` runs `prompt` as one turn with
/// no client, and the session command serves clients that may answer
/// (`docs/permissions.md`, "Headless"). `log` holds the lock; every failure
/// before `fiber_started` is written leaves the log byte for byte as it
/// was. The result is returned, never reported: the caller ends the
/// isolation first, so a signal recorded during the end wins over the
/// failure.
#[allow(
    clippy::too_many_arguments,
    reason = "one resumed session needs its log, directory, id, mode, clock, signals and recorded executable"
)]
fn resumed_session(
    log: Arc<Log>,
    dir: &Path,
    id: SessionId,
    overrides: Vec<String>,
    credential: Option<String>,
    prompt: Option<String>,
    one_turn: bool,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    fiber: Result<PathBuf, String>,
    reads: Arc<crate::switch::Reads>,
) -> Result<i32, Failure> {
    let folded = match r#loop::resumed(dir) {
        Ok(folded) => folded,
        Err(e) => return Err(failed(e.code(), e)),
    };
    // The recorded model first, `--model` and configuration after it. A
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
    let mut parts = crate::parts_with(
        overrides,
        folded.model.as_deref(),
        crate::credential::Labels::new(credential.as_deref(), folded.credential.as_deref()),
        recorded_thinking,
        Arc::clone(&clock),
        None,
        Arc::clone(&reads),
    )?;
    // The session directory's log: the opening message's
    // environment names it.
    parts.prompt.session_log = dir.join("events.jsonl").display().to_string();
    let parts = parts;
    let crate::Parts {
        provider,
        model,
        prompt: prompt_inputs,
        reviewer,
        limits,
        reviewer_notes,
        budget,
        retry,
        handoff,
        idle,
        warm,
        caps,
        home,
        project,
        credential_files,
        extensions,
        locks,
        mut mcp,
        switching,
        switchable,
        resolve,
        web_search,
        ..
    } = parts;
    // The recorded workspace, not the launch directory. A failure here,
    // such as not finding the running binary, returns before `fiber_started`,
    // so the log stays as it was. Every configured stdio server starts too.
    // Jobs live and die with the process: a resumed session's registry
    // starts empty (`docs/tools.md`, "Background jobs").
    let jobs = jobs::Registry::new(
        dir.join("artifacts"),
        Arc::clone(&clock),
        Arc::new(log::WeakEmit::new(&log)) as _,
    );
    // A resumed session declares `delegate_spawn` too, parented to itself.
    let fiber_path = match fiber.clone() {
        Ok(fiber_path) => fiber_path,
        Err(message) => {
            return Err(failed(ErrorCode::IoFailed, message));
        }
    };
    let Some(sessions) = dir.parent() else {
        return Err(failed(
            ErrorCode::IoFailed,
            "the session directory has no parent",
        ));
    };
    let delegates = crate::delegates::Delegates::new(
        fiber_path,
        home.clone(),
        id,
        Path::new(&folded.workspace).to_path_buf(),
        sessions.to_path_buf(),
        Arc::clone(&jobs),
        Arc::clone(&clock),
        resolve,
    );
    crate::shutdown::arm(signals, &reads);
    let skill_set = r#loop::SkillSet::new(prompt_inputs.clone(), Path::new(&folded.workspace));
    let skills: Arc<dyn contract::skills::Skills> = Arc::new(skill_set.reader());
    let web_search =
        crate::builtin::web_search(web_search.as_deref(), extensions.search_backend().as_ref())?;
    let skipped = folded.skipped();
    let (tools, infos, driver, session_servers) = crate::mcp_servers::session_tools(
        fiber,
        &home,
        Path::new(&folded.workspace),
        &dir.join("artifacts"),
        &clock,
        &jobs,
        &locks,
        &project,
        mcp.specs,
        mcp.repository,
        &skipped,
        web_search,
        &delegates,
        skills,
        extensions.tools(),
    )?;
    let forget = Arc::clone(&session_servers.forget);
    let hosted_stands = crate::switch::hosted_stands(&tools);
    let workspace = std::path::PathBuf::from(&folded.workspace);
    let offer = Arc::new(extensions::SessionOffer::new(
        &home,
        &project,
        &workspace,
        Arc::clone(&clock),
    ));
    let permissions = crate::ask_permissions(
        &home,
        &project,
        folded.workspace.clone(),
        credential_files,
        &clock,
    );
    let session = match Session::resume(
        &home,
        dir,
        &log,
        Arc::clone(&clock),
        infos,
        Box::new(io::stdout()),
    ) {
        Ok(session) => session,
        Err(e) => {
            session_servers.servers.stop();
            return Err(e);
        }
    };
    // What the session loads once its offer step has run, before its first
    // preamble: the repository code approved since the process started.
    let load = session_servers
        .later
        .into_load(session.declarer(), caps.clone());
    session.shell(driver);
    session.jobs(jobs.clone());
    session.images(Arc::clone(&session_servers.images));
    session.hooks(Arc::clone(&extensions) as Arc<dyn contract::hook::Hooks>);
    session.extensions(Arc::clone(&extensions) as Arc<dyn contract::extension::ExtensionDoor>);
    extensions.emit_to(Arc::new(log::WeakEmit::new(&log)));
    extensions.drive_to(session.driver());
    extensions.answerable(!one_turn);
    session.skills(r#loop::skills(&prompt_inputs, Path::new(&folded.workspace)));
    // The session's MCP prompt rows, as a new session lists them: the
    // `commands` answer lists them beside skills, and `/name` runs them
    // through the fetch below (`docs/mcp.md`, "Prompts and resources").
    // Shadowed prompts join the startup notices.
    let prompt_rows = session_servers.prompts.commands();
    let listed = r#loop::commands(&prompt_inputs, Path::new(&folded.workspace), &prompt_rows);
    mcp.notices.extend(listed.notices);
    let mut all_commands = listed.rows;
    all_commands.extend(extensions.commands());
    session.commands(all_commands);
    let door = crate::switch::Door {
        declare: session.declarer(),
        hosted_stands,
        backend: extensions.search_backend(),
    };
    let cancel = Arc::new(r#loop::TurnCancel::default());
    // A signal while armed: the log stays as it was. The commit below
    // decides the window between `start` and the first write: a recorded
    // signal writes nothing and exits with its code.
    let reads = switching.reads();
    if let Some(code) = crate::shutdown::start(signals, &cancel, &session, jobs.clone(), reads) {
        session_servers.servers.stop();
        close(session, log, &home, dir, &workspace, &*clock);
        return Ok(code);
    }
    match signals.commit(|| {
        r#loop::fiber_started(&log, env!("CARGO_PKG_VERSION"), true)
            .and_then(|()| crate::session_extensions::written(&log, &extensions))
            .and_then(|()| r#loop::mcp_servers_started(&log, session_servers.failed, mcp.notices))
    }) {
        Err(code) => {
            session_servers.servers.stop();
            close(session, log, &home, dir, &workspace, &*clock);
            return Ok(code);
        }
        Ok(Err(e)) => {
            session_servers.servers.stop();
            close(session, log, &home, dir, &workspace, &*clock);
            return Err(failed(e.code(), e));
        }
        Ok(Ok(())) => {}
    }
    let inbox_wake = session.inbox_wake();
    let rewrites = session.rewrites();
    let admission = session.admission();
    // The fetch runs a prompt row through its server, starting a lazy
    // one as a first tool call does (`docs/mcp.md`, "Prompts and
    // resources"). No logic lives here beyond that call.
    let fetch_prompts = session_servers.prompts.clone();
    let fetch: r#loop::FetchPrompt = Arc::new(move |server, prompt, text, cancel| {
        fetch_prompts.get(server, prompt, text, cancel)
    });
    let code = run_turn(
        &session,
        &log,
        dir,
        prompt,
        one_turn,
        cancel,
        |inbox, cancel| {
            crate::finish(
                Loop::resume(
                    r#loop::Session {
                        log: Arc::clone(&log),
                        provider,
                        model,
                        prompt: prompt_inputs,
                        inbox,
                        tools: r#loop::capped(tools, &caps),
                        permissions,
                    },
                    folded,
                )
                .map(|looped| {
                    let looped = crate::session_extensions::hooked(looped.jobs(jobs), &extensions);
                    looped
                        .reviewer_notes(reviewer_notes)
                        .handoff(handoff)
                        .on_handoff(forget)
                        .switcher(switching.closure(door), switchable)
                        .repository_code(offer)
                        .approved_code(load)
                        .skill_set(skill_set)
                        .server_prompts(r#loop::ServerPrompts {
                            rows: prompt_rows,
                            fetch,
                        })
                        .inbox_wake(inbox_wake)
                        .on_rewrite(rewrites)
                        .admission(admission)
                }),
                budget,
                idle,
                warm,
                // `fiber ask --resume` runs one turn with no client.
                !one_turn,
                reviewer,
                limits,
                retry,
                cancel,
            )
            .map(|()| None)
        },
    );
    session_servers.servers.stop();
    close(session, log, &home, dir, &workspace, &*clock);
    Ok(code)
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

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nothing => write!(f, "Nothing"),
            Self::Shared { .. } => write!(f, "Shared(..)"),
            Self::Made(_) => write!(f, "Made(..)"),
        }
    }
}

/// Enters the recorded worktree: a present one is shared for the
/// process's life, and a missing one with a recorded repository is made
/// again at the same path and branch before anything else runs. The
/// recreation arms the shutdown first, so a signal during the
/// hook-running `git worktree add` kills it and rolls back. A share or
/// recreation failure carries no isolation: nothing was made. A
/// downgrade failure after the worktree was made reaches the caller, so
/// its isolation is ended after the session lock is released.
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
                    Ok(doors::Recreated::Present(shared)) => Ok(Held::Shared { _shared: shared }),
                    Err(error) => Err(error),
                }
            }
            None => Ok(Held::Nothing),
        },
        Ok(_) | Err(_) => match doors::share(path) {
            Ok(shared) => Ok(Held::Shared { _shared: shared }),
            Err(failure) => Err(doors::RecreateError {
                failure,
                isolation: None,
            }),
        },
    }
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
