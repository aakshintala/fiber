//! `fiber ask --resume <id>`: sends the prompt to an existing session
//! (`docs/invocation.md`, "Lifecycle"); the internal session command's
//! `--resume` serves one to clients. The log's lock is held before the
//! lines it builds from are read, so no writer adds a line between the read
//! and the first write. A session another process holds is attached to
//! instead (`docs/invocation.md`, "Processes").

use std::io;
use std::path::Path;
use std::sync::Arc;

use contract::SessionId;
use doors::Session;
use log::Log;
use r#loop::Loop;

use crate::session_command::close;
use crate::{ask_failed, failed, run_turn};

/// `fiber ask --resume <selector>`: resolves the selector, opens the
/// session's log, folds what the resume needs, and runs one turn on it. A
/// held lock attaches instead of opening a second writer; every failure
/// before `fiber_started` is written leaves the log byte for byte as it was.
pub(crate) fn ask_resume(
    selector: String,
    model: Option<String>,
    prompt: String,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &doors::Signals,
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
    let id = match log::resolve(&sessions, &selector, &|started| {
        doors::project(Path::new(started)) == project
    }) {
        Ok(id) => id,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
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
            return match doors::attach(&home, &id, prompt, &mut io::stdout(), failed(e.code(), e)) {
                Ok(code) => code,
                Err(e) => ask_failed(e),
            };
        }
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let dir = sessions.join(&id.0);
    resumed_session(log, &dir, model, Some(prompt), true, clock, signals)
}

/// The internal session command with `--resume`: resumes the session `id`
/// in the current directory's project to serve clients
/// (`docs/invocation.md`, "The hub"). The hub attaches to a live session by
/// its socket, so a held lock fails instead of attaching.
pub(crate) fn session_resume(
    id: SessionId,
    model: Option<String>,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &doors::Signals,
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
    resumed_session(log, &dir, model, None, false, clock, signals)
}

/// One function builds and runs every resumed session, as `new_session`
/// does every new one: `fiber ask --resume` runs `prompt` as one turn with
/// no client, and the session command serves clients that may answer
/// (`docs/permissions.md`, "Headless"). `log` holds the lock; every failure
/// before `fiber_started` is written leaves the log byte for byte as it
/// was.
fn resumed_session(
    log: Arc<Log>,
    dir: &Path,
    model: Option<String>,
    prompt: Option<String>,
    one_turn: bool,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &doors::Signals,
) -> i32 {
    let folded = match r#loop::resumed(dir) {
        Ok(folded) => folded,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    // The recorded model first, `--model` and configuration after it. A
    // model that no longer resolves fails here, before any session line is
    // written. Configuration and the project key still come from the launch
    // directory; the workspace is the first `session_started`'s, wherever
    // the resume runs.
    let parts = match crate::parts_with(
        model,
        folded.model.as_deref(),
        folded.credential.as_deref(),
        Arc::clone(&clock),
    ) {
        Ok(mut parts) => {
            // The session directory's log: the opening message's
            // environment names it.
            parts.prompt.session_log = dir.join("events.jsonl").display().to_string();
            parts
        }
        Err(e) => return ask_failed(e),
    };
    let crate::Parts {
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
        credential_files,
        extensions,
        locks,
        mcp,
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
        Arc::clone(&log) as _,
    );
    crate::shutdown::arm(signals);
    let (tools, infos, driver, session_servers) = match crate::mcp_servers::session_tools(
        &home,
        Path::new(&folded.workspace),
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
    let workspace = std::path::PathBuf::from(&folded.workspace);
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
            return ask_failed(e);
        }
    };
    session.shell(driver);
    session.jobs(jobs.clone());
    session.hooks(Arc::clone(&extensions) as Arc<dyn contract::hook::Hooks>);
    session.commands(r#loop::commands(
        &prompt_inputs,
        Path::new(&folded.workspace),
    ));
    let cancel = Arc::new(r#loop::TurnCancel::default());
    // A signal while armed: the log stays as it was.
    if let Some(code) = crate::shutdown::start(signals, &cancel, &session, jobs.clone()) {
        session_servers.servers.stop();
        close(session, log, &home, dir, &workspace, &*clock);
        return code;
    }
    if let Err(e) = r#loop::fiber_started(&log, env!("CARGO_PKG_VERSION"), true)
        .and_then(|()| crate::session_extensions::written(&log, &extensions))
        .and_then(|()| r#loop::mcp_servers_started(&log, session_servers.failed, mcp.notices))
    {
        session_servers.servers.stop();
        close(session, log, &home, dir, &workspace, &*clock);
        return ask_failed(failed(e.code(), e));
    }
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
                    Arc::clone(&log),
                    folded,
                    provider,
                    model,
                    prompt_inputs,
                    inbox,
                    tools,
                    permissions,
                )
                .map(|looped| {
                    let looped = crate::session_extensions::hooked(looped.jobs(jobs), &extensions);
                    looped.handoff(handoff).on_handoff(forget)
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
        },
    );
    session_servers.servers.stop();
    close(session, log, &home, dir, &workspace, &*clock);
    code
}
