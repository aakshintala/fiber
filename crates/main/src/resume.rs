//! `fiber ask --resume <id>`: sends the prompt to an existing session
//! (`docs/invocation.md`, "Lifecycle"). The log's lock is held before the
//! lines it builds from are read, so no writer adds a line between the read
//! and the first write. A session another process holds is attached to
//! instead (`docs/invocation.md`, "Processes").

use std::io;
use std::path::Path;
use std::sync::Arc;

use doors::Session;
use log::Log;
use r#loop::Loop;

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
    let id = match log::resolve(&sessions, &selector) {
        Ok(id) => id,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let dir = sessions.join(&id.0);
    // `Log::open` takes the lock first; the lines below are read under it.
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
    let lines = match log::read(&dir) {
        Ok(lines) => lines,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let folded = match r#loop::resumed(&lines) {
        Ok(folded) => folded,
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    // The recorded model first, `--model` and configuration after it. A
    // model that no longer resolves fails here, before any session line is
    // written. Configuration and the project key still come from the launch
    // directory; the workspace is the first `session_started`'s, wherever
    // the resume runs.
    let parts = match crate::parts_with(model, folded.model.as_deref(), Arc::clone(&clock)) {
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
        home,
        project,
        ..
    } = parts;
    // The recorded workspace, not the launch directory. A failure here,
    // such as not finding the running binary, returns before `fiber_started`,
    // so the log stays as it was.
    let (tools, infos) = match crate::builtin::builtin(Path::new(&folded.workspace), &clock) {
        Ok(built) => built,
        Err(e) => return ask_failed(e),
    };
    let permissions = crate::ask_permissions(&home, &project, folded.workspace, &clock);
    let session = match Session::resume(&home, &dir, &log, clock, infos, Box::new(io::stdout())) {
        Ok(session) => session,
        Err(e) => return ask_failed(e),
    };
    if let Err(e) = r#loop::fiber_started(&log, env!("CARGO_PKG_VERSION"), true) {
        session.close(log);
        return ask_failed(failed(e.code(), e));
    }
    let code = run_turn(&session, &log, &dir, prompt, |inbox, cancel| {
        crate::finish(
            Loop::resume(
                Arc::clone(&log),
                &lines,
                provider,
                model,
                prompt_inputs,
                inbox,
                tools,
                permissions,
            ),
            budget,
            reviewer,
            limits,
            retry,
            cancel,
        )
    });
    session.close(log);
    code
}
