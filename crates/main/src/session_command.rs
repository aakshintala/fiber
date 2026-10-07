//! The internal session command (`docs/invocation.md`, "Commands and
//! flags" and "Processes"), and the one function that builds and runs
//! every new session, `fiber ask`'s included.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::clock::wall_ms;
use contract::{ErrorCode, SessionId};
use doors::Session;
use log::Log;
use r#loop::Loop;

use crate::{
    Parts, ask_failed, ask_permissions, cli, crash, failed, finish, late_emit, mcp_servers,
    parts_with, run_turn, session_extensions, shutdown, stop_and_fail,
};

/// The internal session command: one session process bound at
/// `run/<session_id>` (`docs/invocation.md`, "Processes"). The workspace
/// is entered before signals are installed or any thread starts, so the
/// shared path reads it as the current directory exactly as `ask` does.
/// With `--resume` the workspace is the one the log recorded, and the
/// session is resumed instead of started. Stdin is never read.
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
    new_session(
        SessionId(args.id),
        args.model,
        args.prompt,
        false,
        clock,
        &signals,
        fiber,
    )
}

/// One function builds and runs every new session (`docs/invocation.md`,
/// "Processes"): `fiber ask` and the internal session command differ
/// only in the id's source (minted vs `--id`), the workspace (the current
/// directory on entry, applied by chdir before the call) and the first
/// deliveries (prompt plus `close` vs an optional prompt).
pub(crate) fn new_session(
    id: SessionId,
    model: Option<String>,
    prompt: Option<String>,
    one_turn: bool,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &doors::Signals,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let mut parts = match parts_with(model, None, None, Arc::clone(&clock)) {
        Ok(parts) => parts,
        Err(e) => return ask_failed(e),
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
        budget,
        retry,
        handoff,
        idle,
        warm,
        home,
        project,
        workspace,
        sessions,
        credential_files,
        extensions,
        locks,
        mcp,
        web_search,
    } = parts;
    let (job_emit, jobs) = late_emit::registry(&dir, &clock);
    shutdown::arm(signals);
    // Before the log exists: a failure here, such as not finding the running
    // binary, leaves no session line; every server starts with the session too.
    let (tools, infos, driver, session_servers) = match mcp_servers::session_tools(
        fiber,
        &home,
        &workspace,
        &dir.join("artifacts"),
        &clock,
        &jobs,
        &locks,
        mcp.specs,
        web_search.as_deref(),
    ) {
        Ok(built) => built,
        Err(e) => return ask_failed(e),
    };
    let forget = Arc::clone(&session_servers.forget);
    let permissions = ask_permissions(
        &home,
        &project,
        workspace.to_string_lossy().into_owned(),
        credential_files,
        &clock,
    );
    let log = match Log::create(&sessions, id, Arc::clone(&clock)) {
        Ok(log) => Arc::new(log),
        Err(e) => return stop_and_fail(session_servers, failed(e.code(), e)),
    };
    job_emit.set(Arc::clone(&log) as _);
    let session = match Session::open(
        &home,
        &dir,
        &log,
        Arc::clone(&clock),
        infos,
        Box::new(io::stdout()),
    ) {
        Ok(session) => session,
        Err(e) => return stop_and_fail(session_servers, e),
    };
    session.shell(driver);
    session.jobs(jobs.clone());
    session.hooks(Arc::clone(&extensions) as Arc<dyn contract::hook::Hooks>);
    session.extensions(Arc::clone(&extensions) as Arc<dyn contract::extension::ExtensionDoor>);
    extensions.emit_to(Arc::new(log::WeakEmit::new(&log)));
    let mut all_commands = r#loop::commands(&prompt_inputs, &workspace);
    all_commands.extend(extensions.commands());
    session.commands(all_commands);
    let cancel = Arc::new(r#loop::TurnCancel::default());
    // A signal while armed: nothing was written, so nothing more is.
    if let Some(code) = shutdown::start(signals, &cancel, &session, jobs.clone()) {
        session_servers.servers.stop();
        close(session, log, &home, &dir, &workspace, &*clock);
        return code;
    }
    let code = run_turn(
        &session,
        &log,
        &dir,
        prompt,
        one_turn,
        cancel,
        |inbox, cancel| {
            finish(
                // `Loop::start` writes `session_started`, which `fiber_started`
                // follows (`docs/events.md`).
                Loop::start(
                    Arc::clone(&log),
                    provider,
                    model,
                    prompt_inputs,
                    inbox,
                    tools,
                    permissions,
                )
                .and_then(|looped| {
                    r#loop::fiber_started(&log, env!("CARGO_PKG_VERSION"), false)?;
                    session_extensions::written(&log, &extensions)?;
                    r#loop::mcp_servers_started(&log, session_servers.failed, mcp.notices)?;
                    let looped = session_extensions::hooked(looped.jobs(jobs), &extensions);
                    Ok(looped.handoff(handoff).on_handoff(forget))
                }),
                budget,
                idle,
                warm,
                // Only one-turn `fiber ask` runs with no client: the
                // session command serves clients that may answer
                // (`docs/permissions.md`, "Headless").
                !one_turn,
                reviewer,
                limits,
                retry,
                cancel,
            )
        },
    );
    session_servers.servers.stop();
    close(session, log, &home, &dir, &workspace, &*clock);
    code
}

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
