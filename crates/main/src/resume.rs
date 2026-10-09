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
    resumed_session(
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

/// The internal session command with `--resume`: resumes the session `id`
/// in the current directory's project to serve clients
/// (`docs/invocation.md`, "The hub"). The hub attaches to a live session by
/// its socket, so a held lock fails instead of attaching.
pub(crate) fn session_resume(
    id: SessionId,
    overrides: Vec<String>,
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
    resumed_session(
        log, &dir, id, overrides, None, None, false, clock, signals, fiber,
    )
}

/// One function builds and runs every resumed session, as `new_session`
/// does every new one: `fiber ask --resume` runs `prompt` as one turn with
/// no client, and the session command serves clients that may answer
/// (`docs/permissions.md`, "Headless"). `log` holds the lock; every failure
/// before `fiber_started` is written leaves the log byte for byte as it
/// was.
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
) -> i32 {
    let folded = match r#loop::resumed(dir) {
        Ok(folded) => folded,
        Err(e) => return ask_failed(failed(e.code(), e)),
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
    let parts = match crate::parts_with(
        overrides,
        folded.model.as_deref(),
        crate::credential::Labels::new(credential.as_deref(), folded.credential.as_deref()),
        recorded_thinking,
        Arc::clone(&clock),
        None,
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
        Arc::clone(&log) as _,
    );
    // A resumed session declares `delegate_spawn` too, parented to itself.
    let fiber_path = match fiber.clone() {
        Ok(fiber_path) => fiber_path,
        Err(message) => return ask_failed(failed(ErrorCode::IoFailed, message)),
    };
    let Some(sessions) = dir.parent() else {
        return ask_failed(failed(
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
    crate::shutdown::arm(signals);
    let skills: Arc<dyn contract::skills::Skills> = Arc::new(r#loop::SkillReader::new(
        prompt_inputs.clone(),
        Path::new(&folded.workspace),
    ));
    let (tools, infos, driver, session_servers) = match crate::mcp_servers::session_tools(
        fiber,
        &home,
        Path::new(&folded.workspace),
        &dir.join("artifacts"),
        &clock,
        &jobs,
        &locks,
        mcp.specs,
        web_search.as_deref(),
        &delegates,
        skills,
        extensions.tools(),
    ) {
        Ok(built) => built,
        Err(e) => return ask_failed(e),
    };
    let forget = Arc::clone(&session_servers.forget);
    let hosted_stands = crate::switch::hosted_stands(&tools);
    let workspace = std::path::PathBuf::from(&folded.workspace);
    let offer = Arc::new(extensions::SessionOffer::new(&home, &project, &workspace));
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
    };
    let cancel = Arc::new(r#loop::TurnCancel::default());
    // A signal while armed: the log stays as it was.
    let reads = switching.reads();
    if let Some(code) = crate::shutdown::start(signals, &cancel, &session, jobs.clone(), reads) {
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
    let inbox_wake = session.inbox_wake();
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
                    Arc::clone(&log),
                    folded,
                    provider,
                    model,
                    prompt_inputs,
                    inbox,
                    r#loop::capped(tools, &caps),
                    permissions,
                )
                .map(|looped| {
                    let looped = crate::session_extensions::hooked(looped.jobs(jobs), &extensions);
                    looped
                        .reviewer_notes(reviewer_notes)
                        .handoff(handoff)
                        .on_handoff(forget)
                        .switcher(switching.closure(door), switchable)
                        .repository_code(offer)
                        .server_prompts(r#loop::ServerPrompts {
                            rows: prompt_rows,
                            fetch,
                        })
                        .inbox_wake(inbox_wake)
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
