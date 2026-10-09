//! `fiber resume` and `fiber continue`: open a session in the terminal
//! (`docs/invocation.md`, "Commands and flags").

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use contract::SessionId;
use contract::shapes::Failure;

/// Whether the terminal refuses on `stdin_tty` and `stdout_tty`: without
/// a tty on both it is a usage error naming `fiber ask`, and nothing is
/// written to stdout.
pub(crate) fn tty_refusal(stdin_tty: bool, stdout_tty: bool) -> Option<&'static str> {
    if stdin_tty && stdout_tty {
        None
    } else {
        Some(
            "The terminal needs a tty; run `fiber ask \"<prompt>\"`. Run `fiber --help` for usage.",
        )
    }
}

/// Refuses without a tty on standard input and output, before anything is
/// read or started: `Some(2)` after printing the sentence on stderr.
pub(crate) fn needs_tty() -> Option<i32> {
    match tty_refusal(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    ) {
        None => None,
        Some(sentence) => {
            eprintln!("fiber: {sentence}");
            Some(2)
        }
    }
}

/// `fiber resume`: with an id, the session it names in this project;
/// with none, home at the session list. The tty is checked before the
/// selector is resolved; a selector nothing in this project answers is
/// the resolve error, before the terminal starts.
pub(crate) fn resume(selector: Option<String>, fiber: Result<PathBuf, String>) -> i32 {
    if let Some(code) = needs_tty() {
        return code;
    }
    let Some(selector) = selector else {
        return crate::terminal(fiber, tui::OpenAt::List);
    };
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(e) => return crate::fail(crate::failed(e.code(), e)),
    };
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(e) => {
            return crate::fail(crate::failed(
                contract::ErrorCode::IoFailed,
                format!("the current directory: {e}"),
            ));
        }
    };
    let project = doors::project(&workspace);
    let sessions = log::sessions_dir(&home, &project);
    let in_project = in_project(&project, &doors::project);
    let id = match log::resolve(&sessions, &selector, &in_project) {
        Ok(id) => id,
        Err(e) => return crate::fail(crate::failed(e.code(), e)),
    };
    crate::crash::attach(&id);
    crate::terminal(fiber, tui::OpenAt::Session(id))
}

/// `fiber continue`: the session used most recently in this project. The
/// tty is checked before any log is read; with no session it is a usage
/// error naming `fiber`, and the terminal never starts.
pub(crate) fn continue_latest(fiber: Result<PathBuf, String>) -> i32 {
    if let Some(code) = needs_tty() {
        return code;
    }
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(e) => return crate::fail(crate::failed(e.code(), e)),
    };
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(e) => {
            return crate::fail(crate::failed(
                contract::ErrorCode::IoFailed,
                format!("the current directory: {e}"),
            ));
        }
    };
    let project = doors::project(&workspace);
    let sessions = log::sessions_dir(&home, &project);
    let in_project = in_project(&project, &doors::project);
    let id = match select_latest(&sessions, &in_project) {
        Ok(id) => id,
        Err(e) => return crate::fail(e),
    };
    crate::crash::attach(&id);
    crate::terminal(fiber, tui::OpenAt::Session(id))
}

/// The session `continue` opens in `sessions`: the most recent one, or a
/// usage error naming `fiber` when there is none.
fn select_latest(sessions: &Path, in_project: &dyn Fn(&str) -> bool) -> Result<SessionId, Failure> {
    match log::most_recent(sessions, in_project) {
        Some(id) => Ok(id),
        None => Err(doors::failure(
            contract::ErrorCode::Usage,
            "No session in this project to continue; run `fiber` to start one. Run `fiber --help` for usage.",
        )),
    }
}

/// The project test `resolve` and `most_recent` take for `project`: the
/// identity of each workspace, through `identity`. Each distinct
/// workspace runs it once, so a project of many sessions in one
/// workspace runs one `git`.
fn in_project<'a>(
    project: &'a Path,
    identity: &'a dyn Fn(&Path) -> PathBuf,
) -> impl Fn(&str) -> bool + 'a {
    let seen: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
    move |workspace: &str| {
        if let Some(accepted) = seen.borrow().get(workspace) {
            return *accepted;
        }
        let accepted = identity(Path::new(workspace)) == project;
        seen.borrow_mut().insert(workspace.to_owned(), accepted);
        accepted
    }
}

#[cfg(test)]
#[path = "open_tests.rs"]
mod tests;
