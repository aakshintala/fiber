//! The internal session command (`docs/invocation.md`, "Commands and
//! flags" and "Processes"), and the one function that builds and runs
//! every new session, `fiber ask`'s included.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::clock::wall_ms;
use contract::events::Parent;
use contract::shapes::{Failure, Worktree};
use contract::{ErrorCode, SessionId};
use doors::Session;
use log::Log;
use r#loop::Loop;

use crate::{
    Parts, ask_failed, ask_permissions, cli, crash, failed, finish, late_emit, mcp_servers,
    parts_with, run_turn, session_extensions, shutdown,
};

/// The internal session command: one session process bound at
/// `run/<session_id>` (`docs/invocation.md`, "Processes"). The workspace
/// is entered before signals are installed or any thread starts, so the
/// shared path reads it as the current directory exactly as `ask` does.
/// With `--worktree` it moves once more, into the new worktree, after
/// that: the signals' bound thread reads no relative path. With `--resume`
/// the workspace is the one the log recorded, and the session is resumed
/// instead of started. Stdin is never read.
pub(crate) fn run(
    args: cli::SessionArgs,
    clock: Arc<dyn contract::clock::Clock>,
    fiber: Result<PathBuf, String>,
) -> i32 {
    if let Err(e) = std::env::set_current_dir(&args.workspace) {
        return ask_failed(failed(
            ErrorCode::IoFailed,
            format!("{}: {e}", args.workspace.display()),
        ));
    }
    // As `ask`: a signal while starting exits at once.
    let signals = match doors::Signals::install(Arc::clone(&clock)) {
        Ok(signals) => signals,
        Err(e) => return ask_failed(failed(ErrorCode::IoFailed, format!("signals: {e}"))),
    };
    if args.resume {
        return crate::resume::session_resume(
            SessionId(args.id),
            args.model,
            clock,
            &signals,
            fiber,
        );
    }
    if let Some(from) = args.rewound_from {
        return crate::rewind::session_rewound(
            SessionId(args.id),
            SessionId(from),
            clock,
            &signals,
            fiber,
        );
    }
    // A delegate's stdin is its lifeline: the parent holds the write end
    // open and never writes to it (`docs/delegates.md`, "Lifetime").
    let parent = args
        .parent
        .zip(args.delegate_id)
        .map(|(session_id, delegate_id)| Parent {
            session_id: SessionId(session_id),
            delegate_id: contract::JobId(delegate_id),
        });
    if parent.is_some() {
        signals.lifeline(Box::new(std::io::stdin()));
    }
    new_session(
        SessionId(args.id),
        args.model,
        args.prompt,
        parent.is_some(),
        args.worktree,
        clock,
        &signals,
        fiber,
        None,
        parent,
    )
}

/// One function prepares every new session (`docs/invocation.md`,
/// "Processes"): `fiber ask` and the internal session command differ
/// only in the id's source (minted vs `--id`), the workspace (the current
/// directory on entry, applied by chdir before the call) and the first
/// deliveries (prompt plus `close` vs an optional prompt). With `worktree`
/// the process arms, creates the worktree and enters it before the session
/// runs, then ends the worktree however the run went. Case sessions share
/// their clock and host script with extension setup and execution.
#[allow(
    clippy::too_many_arguments,
    reason = "session preparation carries worktree and case inputs to the shared runner"
)]
pub(crate) fn new_session(
    id: SessionId,
    model: Option<String>,
    prompt: Option<String>,
    one_turn: bool,
    worktree: bool,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    fiber: Result<PathBuf, String>,
    case_run: Option<Arc<crate::case::run::CaseRun>>,
    parent: Option<Parent>,
) -> i32 {
    let clock = case_run.as_ref().map_or(clock, |case| case.session_clock());
    let host = case_run.as_ref().map(|case| case.host_script());
    if !worktree {
        return match run_new(
            id, model, prompt, one_turn, None, clock, signals, fiber, host, case_run, parent, None,
        ) {
            Ok(code) => code,
            Err(failure) => report(signals, failure),
        };
    }
    // As `parts_with` would: a failure here leaves no session.
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let launch = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            return ask_failed(failed(
                ErrorCode::IoFailed,
                format!("the current directory: {e}"),
            ));
        }
    };
    // Its own reads: the worktree's `git worktree add` runs through them,
    // so a signal kills git and its hook together.
    let reads = Arc::new(crate::switch::Reads::default());
    shutdown::arm_isolating(signals, &reads);
    let isolation = match doors::isolate(&launch, &home, &id, Arc::clone(&clock), &|command| {
        reads.command(command)
    }) {
        Ok(isolation) => isolation,
        Err(failure) => return report(signals, failure),
    };
    if let Err(e) = std::env::set_current_dir(isolation.path()) {
        let failure = failed(
            ErrorCode::IoFailed,
            format!("{}: {e}", isolation.path().display()),
        );
        isolation.end();
        return report(signals, failure);
    }
    let worktree = isolation.worktree();
    let result = run_new(
        id,
        model,
        prompt,
        one_turn,
        Some(worktree),
        clock,
        signals,
        fiber,
        host,
        case_run,
        parent,
        None,
    );
    isolation.end();
    match result {
        Ok(code) => code,
        Err(failure) => report(signals, failure),
    }
}

/// Builds and runs the session with an optional worktree. Every `Err` is
/// reported by [`report`], after the worktree ends when one was created.
#[allow(
    clippy::too_many_arguments,
    reason = "session execution carries its isolation, case and startup inputs"
)]
pub(crate) fn run_new(
    id: SessionId,
    model: Option<String>,
    prompt: Option<String>,
    one_turn: bool,
    worktree: Option<Worktree>,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    fiber: Result<PathBuf, String>,
    host: Option<Arc<extensions::HostScript>>,
    case_run: Option<Arc<crate::case::run::CaseRun>>,
    parent: Option<Parent>,
    rewound: Option<r#loop::Rewound>,
) -> Result<i32, Failure> {
    // A rewind's new session runs where the old one did, with the model,
    // credential and thinking level the old log's latest build recorded,
    // as a resume keeps its recorded ones (`docs/events.md`, "Rewind").
    let mut parts = match &rewound {
        Some(start) => {
            let home = config::fiber_home_from_env().map_err(|e| failed(e.code(), e))?;
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            let sessions = log::sessions_dir(&home, &doors::project(&workspace));
            let folded = r#loop::forked(&sessions.join(&start.from.session_id.0), start.from.seq)
                .map_err(|e| failed(e.code(), e))?;
            let thinking = folded
                .thinking
                .as_deref()
                .and_then(|level| level.parse::<contract::ThinkingLevel>().ok());
            parts_with(
                None,
                folded.model.as_deref(),
                folded.credential.as_deref(),
                thinking,
                Arc::clone(&clock),
                host,
            )?
        }
        None => parts_with(model, None, None, None, Arc::clone(&clock), host)?,
    };
    crash::attach(&id);
    let dir = parts.sessions.join(&id.0);
    // The session directory's log: the opening message's environment
    // names it.
    parts.prompt.session_log = dir.join("events.jsonl").display().to_string();
    let Parts {
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
        workspace,
        sessions,
        credential_files,
        extensions,
        locks,
        mut mcp,
        switching,
        switchable,
        resolve,
        web_search,
    } = parts;
    let (job_emit, jobs) = late_emit::registry(&dir, &clock);
    shutdown::arm(signals);
    // Every session declares `delegate_spawn`: a delegate is its own
    // session, so the child re-executes this binary.
    let fiber_path = fiber
        .clone()
        .map_err(|message| failed(ErrorCode::IoFailed, message))?;
    let delegates = crate::delegates::Delegates::new(
        fiber_path,
        home.clone(),
        id.clone(),
        workspace.clone(),
        sessions.clone(),
        jobs.clone(),
        Arc::clone(&clock),
        resolve,
    );
    // Before the log exists: a failure here, such as not finding the running
    // binary, leaves no session line; every server starts with the session too.
    let (tools, infos, driver, session_servers) = mcp_servers::session_tools(
        fiber,
        &home,
        &workspace,
        &dir.join("artifacts"),
        &clock,
        &jobs,
        &locks,
        mcp.specs,
        web_search.as_deref(),
        &delegates,
    )?;
    let forget = Arc::clone(&session_servers.forget);
    let hosted_stands = crate::switch::hosted_stands(&tools);
    let permissions = ask_permissions(
        &home,
        &project,
        workspace.to_string_lossy().into_owned(),
        credential_files,
        &clock,
    );
    let offer = Arc::new(extensions::SessionOffer::new(&home, &project, &workspace));
    let log = match Log::create(&sessions, id, Arc::clone(&clock)) {
        Ok(log) => Arc::new(log),
        Err(e) => {
            session_servers.servers.stop();
            return Err(failed(e.code(), e));
        }
    };
    job_emit.set(Arc::new(log::WeakEmit::new(&log)) as _);
    let event_output: Box<dyn io::Write + Send> = if case_run.is_some() {
        Box::new(io::sink())
    } else {
        Box::new(io::stdout())
    };
    let session = match Session::open(&home, &dir, &log, Arc::clone(&clock), infos, event_output) {
        Ok(session) => session,
        Err(e) => {
            session_servers.servers.stop();
            return Err(e);
        }
    };
    session.shell(driver);
    session.jobs(jobs.clone());
    session.images(Arc::clone(&session_servers.images));
    session.hooks(Arc::clone(&extensions) as Arc<dyn contract::hook::Hooks>);
    session.extensions(Arc::clone(&extensions) as Arc<dyn contract::extension::ExtensionDoor>);
    extensions.emit_to(Arc::new(log::WeakEmit::new(&log)));
    extensions.drive_to(session.driver());
    extensions.answerable(!one_turn);
    session.skills(r#loop::skills(&prompt_inputs, &workspace));
    // The session's MCP prompt rows, tagged with each server's name:
    // the `commands` answer lists them beside skills, and `/name` runs
    // them through the fetch below (`docs/mcp.md`, "Prompts and
    // resources"). Shadowed prompts join the startup notices.
    let prompt_rows = session_servers.prompts.commands();
    let listed = r#loop::commands(&prompt_inputs, &workspace, &prompt_rows);
    mcp.notices.extend(listed.notices);
    let mut all_commands = listed.rows;
    all_commands.extend(extensions.commands());
    session.commands(all_commands);
    let door = crate::switch::Door {
        declare: session.declarer(),
        hosted_stands,
    };
    let cancel = Arc::new(r#loop::TurnCancel::default());
    // A signal while armed: nothing was written, so nothing more is.
    let reads = switching.reads();
    if let Some(code) = shutdown::start(signals, &cancel, &session, jobs.clone(), reads) {
        session_servers.servers.stop();
        close(session, log, &home, &dir, &workspace, &*clock);
        return Ok(code);
    }
    let inbox_wake = session.inbox_wake();
    // The fetch runs a prompt row through its server, starting a lazy
    // one as a first tool call does (`docs/mcp.md`, "Prompts and
    // resources"). No logic lives here beyond that call.
    let fetch_prompts = session_servers.prompts.clone();
    let fetch: r#loop::FetchPrompt = Arc::new(move |server, prompt, text, cancel| {
        fetch_prompts.get(server, prompt, text, cancel)
    });
    let case_driver = case_run.as_ref().and_then(|case| {
        match case.start(session.driver(), Arc::clone(&log), Arc::clone(&cancel)) {
            Ok(driver) => Some(driver),
            Err(error) => {
                case.record_failure(format!("starting the case driver: {error}"));
                None
            }
        }
    });
    let run_one_turn = one_turn || (case_run.is_some() && case_driver.is_none());
    let code = run_turn(
        &session,
        &log,
        &dir,
        prompt,
        run_one_turn,
        cancel,
        |inbox, cancel| {
            // `Loop::start` writes `session_started`, which `fiber_started`
            // follows (`docs/events.md`); a rewind's start writes the
            // `session_started` that continues the old log instead, and a
            // delegate opens through `Loop::delegate`, naming its parent.
            let started = match (rewound, parent) {
                (Some(start), _) => Loop::rewound(
                    Arc::clone(&log),
                    start,
                    provider,
                    model,
                    prompt_inputs,
                    inbox,
                    r#loop::capped(tools, &caps),
                    permissions,
                ),
                (None, Some(parent)) => Loop::delegate(
                    Arc::clone(&log),
                    provider,
                    model,
                    prompt_inputs,
                    inbox,
                    r#loop::capped(tools, &caps),
                    permissions,
                    parent,
                ),
                (None, None) => Loop::start(
                    Arc::clone(&log),
                    provider,
                    model,
                    prompt_inputs,
                    inbox,
                    r#loop::capped(tools, &caps),
                    permissions,
                    worktree,
                ),
            };
            finish(
                started.and_then(|looped| {
                    r#loop::fiber_started(&log, env!("CARGO_PKG_VERSION"), false)?;
                    session_extensions::written(&log, &extensions)?;
                    r#loop::mcp_servers_started(&log, session_servers.failed, mcp.notices)?;
                    let looped = session_extensions::hooked(looped.jobs(jobs), &extensions);
                    Ok(looped
                        .reviewer_notes(reviewer_notes)
                        .handoff(handoff)
                        .on_handoff(forget)
                        .switcher(switching.closure(door), switchable)
                        .repository_code(offer)
                        .server_prompts(r#loop::ServerPrompts {
                            rows: prompt_rows,
                            fetch,
                        })
                        .inbox_wake(inbox_wake))
                }),
                budget,
                idle,
                warm,
                // Only one-turn `fiber ask` runs with no client: the
                // session command serves clients that may answer
                // (`docs/permissions.md`, "Headless").
                !run_one_turn,
                reviewer,
                limits,
                retry,
                cancel,
            )
        },
    );
    if let Some(case_driver) = case_driver {
        match case_driver.join() {
            Ok(()) => {}
            Err(_) => {
                if let Some(case) = &case_run {
                    case.record_failure("the case driver panicked".to_owned());
                }
            }
        }
    }
    session_servers.servers.stop();
    close(session, log, &home, &dir, &workspace, &*clock);
    Ok(code)
}

/// Prints a startup failure, asking about a recorded signal first: a
/// signal that arrived before `fiber_started` exits with its code and
/// writes nothing (`docs/invocation.md`, "Shutdown").
pub(crate) fn report(signals: &doors::Signals, failure: Failure) -> i32 {
    signals.recorded().unwrap_or_else(|| ask_failed(failure))
}

#[cfg(test)]
#[path = "session_command_tests.rs"]
mod tests;

/// Ends a session process, however it ran: closes the door side, then
/// appends the session's `recent.jsonl` row with what it stopped on, its
/// last `session_status` (`docs/state.md`, "Recently exited sessions").
/// A session `close` deleted, never prompted, leaves nothing behind.
pub(crate) fn close(
    session: Session,
    log: Arc<Log>,
    home: &Path,
    dir: &Path,
    workspace: &Path,
    clock: &dyn contract::clock::Clock,
) {
    let status = log
        .latest("session_status")
        .and_then(|line| serde_json::from_value(line.payload.into()).ok());
    session.close(log);
    if !dir.is_dir() {
        return;
    }
    let name = |path: Option<&Path>| {
        path.and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let row = hub::RecentRow {
        session_id: SessionId(name(Some(dir))),
        ts: wall_ms(clock.wall()),
        // `projects/<key>/sessions/<id>`: the key names the grandparent.
        project: name(dir.parent().and_then(Path::parent)),
        workspace: workspace.to_string_lossy().into_owned(),
        name: status
            .as_ref()
            .map(|status: &contract::events::SessionStatus| status.name.clone())
            .unwrap_or_default(),
        how: hub::Left::Exited,
        status,
    };
    // A row that cannot be written loses only the listing, not the log.
    hub::append(home, &row).unwrap_or(());
}
