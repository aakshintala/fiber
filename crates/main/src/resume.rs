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
use contract::diag::DebugLog;
use contract::events::Event;
use contract::shapes::{Failure, Worktree};
use doors::Session;
use log::Log;
use r#loop::Loop;

use crate::session_command::close;
use crate::{ask_failed, failed, run_turn};

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
    let mut parts = crate::parts_with_session(
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
    // The session directory's log: the opening message's
    // environment names it.
    parts.prompt.session_log = dir.events().display().to_string();
    let parts = parts;
    let crate::Parts {
        sources,
        provider,
        observe,
        reviewer_observe,
        diag,
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
        pending_mcp,
        switching,
        switchable,
        resolve,
        web_search,
        shell_read_only,
        sandbox,
        protection,
        repository,
        ..
    } = parts;
    // The log is already open: install the writer before any append the
    // resumed process makes. The level is checked once; its answer gates
    // both the install and the connection timing below.
    let timing = if diag.debugging() {
        log.install_timing(Arc::clone(&diag));
        Some(diag.timings())
    } else {
        None
    };
    // The recreated worktree's create line opens here, before the
    // session file attaches: a kept present worktree has no opener, so
    // it writes none.
    if let (Some(timing), Some(opened)) = (&timing, &opened) {
        opened(timing);
    }
    // The recorded workspace, not the launch directory. A failure here,
    // such as not finding the running binary, returns before `fiber_started`,
    // so the log stays as it was. Every configured stdio server starts too.
    // Jobs live and die with the process: a resumed session's registry
    // starts empty (`docs/tools.md`, "Background jobs").
    let jobs = jobs::Registry::new(
        dir.artifacts(),
        Arc::clone(&clock),
        Arc::new(log::WeakEmit::new(&log)) as _,
    );
    // The registry starts empty on a resumed session too: installing the
    // session's timing times a background start as `jobs.start`.
    if let Some(timing) = &timing {
        jobs.install_timing(Arc::clone(timing));
    }
    // A resumed session declares `delegate_spawn` too, parented to itself.
    let Some(sessions) = dir.path().parent() else {
        return Err(failed(
            ErrorCode::IoFailed,
            "the session directory has no parent",
        ));
    };
    let crate::switch::quota::QuotaReaders {
        door: quota,
        model: model_quota,
        notice,
    } = switching.quota_readers();
    let delegates = crate::delegates::Delegates::new(
        fiber_path.clone(),
        home.clone(),
        id.clone(),
        Path::new(&folded.workspace).to_path_buf(),
        sessions.to_path_buf(),
        Arc::clone(&jobs),
        Arc::clone(&clock),
        resolve,
    )
    .with_quota(model_quota);
    crate::shutdown::arm(signals, &reads);
    let skill_set = r#loop::SkillSet::new(prompt_inputs.clone(), Path::new(&folded.workspace));
    let skills: Arc<dyn contract::skills::Skills> = Arc::new(skill_set.reader());
    let web_search =
        crate::builtin::web_search(web_search.as_deref(), extensions.search_backend().as_ref())?;
    let pending_mcp = match pending_mcp {
        Some(pending) => pending,
        None => {
            let _ = extensions.finish_processes(&support::cancel::Token::new());
            return Err(failed(ErrorCode::IoFailed, "MCP startup was not prepared"));
        }
    };
    let (tools, infos, driver, session_servers) = match crate::mcp_servers::session_tools(
        fiber_path.clone(),
        &home,
        Path::new(&folded.workspace),
        &dir.artifacts(),
        &clock,
        &jobs,
        &locks,
        &project,
        pending_mcp,
        web_search,
        &delegates,
        skills,
        extensions.tools(),
        timing.clone(),
        shell_read_only.clone(),
        sandbox.clone(),
        Some(protection),
    ) {
        Ok(tools) => tools,
        Err(failure) => {
            let _ = extensions.finish_processes(&support::cancel::Token::new());
            return Err(failure);
        }
    };
    let delegates = std::sync::Arc::new(delegates);
    let forget = Arc::clone(&session_servers.forget);
    let hosted_stands = crate::switch::hosted_stands(&tools);
    let workspace = std::path::PathBuf::from(&folded.workspace);
    let offer = Arc::new(extensions::SessionOffer::new(
        &home,
        &project,
        &workspace,
        crate::session_extensions::fetch(&clock),
    ));
    let permissions = crate::ask_permissions(
        &home,
        &project,
        folded.workspace.clone(),
        credential_files,
        &clock,
    );
    // The durable log is held: the diagnostic writer takes the session's
    // file and id, so the door's socket timer and every later line share
    // them, while MCP and permission lines keep the pre-attach file.
    diag.attach(&id);
    let session = match Session::resume_with_timing(
        &home,
        dir.path(),
        &log,
        Arc::clone(&clock),
        infos,
        Box::new(io::stdout()),
        timing,
    ) {
        Ok(session) => session,
        Err(e) => {
            session_servers.servers.stop();
            return Err(Failure::from(e));
        }
    };
    session.shell(driver);
    session.notes_workspace(workspace.clone());
    session.jobs(jobs.clone());
    session.repository_code(Arc::clone(&offer) as Arc<dyn contract::repository::RepositoryCode>);
    session.images(Arc::clone(&session_servers.images));
    session.hooks(Arc::clone(&extensions) as Arc<dyn contract::hook::Hooks>);
    session.extensions(Arc::clone(&extensions) as Arc<dyn contract::extension::ExtensionDoor>);
    extensions.emit_to(Arc::new(log::WeakEmit::new(&log)));
    extensions.drive_to(session.driver());
    let switching = Arc::new(switching.diagnostics(Arc::clone(&diag)));
    let host_model = Arc::new(crate::model_host::ModelHost::new(
        switching.reacher(Arc::new(crate::connect)),
        budget,
        &retry,
        dir.path().to_path_buf(),
        id.clone(),
    ));
    extensions.model_to(host_model.clone());
    extensions.delegate_to(Arc::clone(&delegates) as Arc<dyn contract::extension::HostDelegate>);
    // A resume restores the flag from the log's first line: a supervised
    // resume stays unanswerable even when a person drives it
    // (`docs/permissions.md`, "Supervised sessions").
    let supervised = folded.supervised;
    let answerable = crate::session_command::effective_answerable(!one_turn, supervised);
    extensions.answerable(answerable);
    extensions.browse_with(
        Arc::new(extensions::SystemBrowser::default()),
        session.attended(),
    );
    session.skills(r#loop::skills(&prompt_inputs, Path::new(&folded.workspace)));
    // The session's MCP prompt rows, as a new session lists them: the
    // `commands` answer lists them beside skills, and `/name` runs them
    // through the fetch below (`docs/mcp.md`, "Prompts and resources").
    // Shadowed prompts join the startup notices.
    let prompt_rows = session_servers.prompts.commands();
    let listed = r#loop::commands(
        &prompt_inputs,
        Path::new(&folded.workspace),
        &prompt_rows,
        &extensions.commands(),
    );
    mcp.notices.extend(listed.notices);
    // The turn-start check measures the runnable set against these rows,
    // so a template added while the session was away still refreshes them.
    skill_set.sync_commands(&listed.runnable);
    session.commands(listed.rows);
    session.quota(quota);
    let door = crate::switch::Door {
        declare: session.declarer(),
        hosted_stands,
        backend: extensions.search_backend(),
    };
    let current = Arc::new(crate::session_extensions::Current::new(extensions));
    let (commands_refresh, commands_sync) =
        crate::session_command::command_update_wiring(&current, &session, &skill_set);
    let readers =
        switching.credential_readers(switching.clone().closure(door), model.reference.clone());
    session.credentials(readers.labels, readers.admitted);
    let switcher = readers.prepare;
    let (reload, load, servers, prompts, server_failures) = crate::reload::State::new(
        sources,
        prompt_inputs.clone(),
        current.clone(),
        switching.clone(),
        &tools,
        &session,
        host_model,
        Arc::clone(&delegates) as Arc<dyn contract::extension::HostDelegate>,
        session_servers,
        answerable,
        log.clone(),
        diag.clone(),
        shell_read_only,
        sandbox,
        std::sync::Arc::new(std::sync::Mutex::new(repository)),
        commands_sync,
    );
    // Every notice the startup already wrote, including the ones
    // `mcp_servers_started` writes after this read: the late preparer
    // drops the ones it would repeat and keeps the new ones.
    let written = crate::session_extensions::startup_written(&current.get(), &mcp.notices);
    let providers = reload.mcp_providers(&server_failures);
    let (reloader, late) = reload.preparers(written);
    session.mcp_servers(providers.refresh);
    session.mcp_login(providers.login);
    session.personal_source_check(crate::prompt_files::personal_source_check(
        prompt_inputs.home.clone(),
        prompt_inputs.agents_home.clone(),
        Arc::clone(&prompt_inputs.clock),
    ));
    let cancel = Arc::new(r#loop::TurnCancel::default());
    // A signal while armed: the log stays as it was. The commit below
    // decides the window between `start` and the first write: a recorded
    // signal writes nothing and exits with its code.
    let reads = switching.reads();
    if let Some(code) = crate::shutdown::start(signals, &cancel, &session, jobs.clone(), reads) {
        servers.stop();
        close(session, log, &home, dir.path(), &workspace, &*clock);
        return Ok(code);
    }
    match signals.commit(|| {
        r#loop::fiber_started(&log, env!("CARGO_PKG_VERSION"), true)
            .and_then(|()| crate::session_extensions::written(&log, &current.get()))
            .and_then(|()| {
                // The providers load reports the same shadow the
                // session's load reported: `written` wrote that half.
                let notices = crate::session_extensions::unwritten(mcp.notices, &current.get());
                r#loop::mcp_servers_started(&log, server_failures, notices)
            })
    }) {
        Err(code) => {
            servers.stop();
            close(session, log, &home, dir.path(), &workspace, &*clock);
            return Ok(code);
        }
        Ok(Err(e)) => {
            servers.stop();
            close(session, log, &home, dir.path(), &workspace, &*clock);
            return Err(failed(e.code(), e));
        }
        Ok(Ok(())) => {}
    }
    // The session file and id are already attached above, before the
    // door opened: request, extension and memory lines share them.
    let inbox_wake = session.inbox_wake();
    let rewrites = session.rewrites();
    let admission = session.admission();
    let steering = session.steering();
    // The fetch runs a prompt row through its server, starting a lazy
    // one as a first tool call does (`docs/mcp.md`, "Prompts and
    // resources"). No logic lives here beyond that call.
    let fetch_prompts = prompts.clone();
    let fetch: r#loop::FetchPrompt = Arc::new(move |server, prompt, text, cancel| {
        fetch_prompts.get(server, prompt, text, cancel)
    });
    let code = run_turn(
        &session,
        &log,
        dir.path(),
        prompt,
        one_turn,
        cancel,
        &diag,
        &current,
        |inbox, cancel, slot| {
            // A resume that fails leaves no loop to take the summary from:
            // the earlier process's open requests still reach the exit.
            *slot = Some(folded.exit_seed());
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
                    let looped = crate::session_extensions::hooked(
                        looped.jobs(jobs).quota(notice),
                        &current,
                    );
                    let looped = crate::session_extensions::released(looped, Arc::clone(&current));
                    looped
                        .diagnostics(Arc::clone(&diag))
                        .observed(observe)
                        .reviewer_observed(reviewer_observe)
                        .reviewer_notes(reviewer_notes)
                        .handoff(handoff)
                        .on_handoff(forget)
                        .switcher(switcher, switchable)
                        .reloader(reloader)
                        .reconnector(providers.reconnect)
                        .late_extensions(late)
                        .repository_code(offer)
                        .approved_code(load)
                        .skill_set(skill_set)
                        .commands_refresh(commands_refresh)
                        .server_prompts(r#loop::ServerPrompts {
                            rows: prompt_rows,
                            fetch,
                        })
                        .inbox_wake(inbox_wake)
                        .on_rewrite(rewrites)
                        .admission(admission)
                        .steering(steering)
                        .tool_search(crate::builtin::tool_search_factory())
                        .tool_rows(crate::builtin::tool_rows(
                            session.declarer(),
                            session.state_setter(),
                        ))
                }),
                budget,
                idle,
                warm,
                // `fiber ask --resume` runs one turn with no client.
                !one_turn,
                supervised,
                reviewer,
                limits,
                retry,
                cancel,
                slot,
            )
            .map(|()| None)
        },
    );
    servers.stop();
    close(session, log, &home, dir.path(), &workspace, &*clock);
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
