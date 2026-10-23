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
#[allow(
    clippy::print_stderr,
    reason = "prints the usage sentence (docs/code-quality.md, \"Lints\")"
)]
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

/// `fiber resume`: with an id, the session it names on one hub; with
/// none, home at that hub's session list. The tty is checked first, then
/// the hub, then the selector: an unknown hub or a missing token fails
/// before the terminal starts and starts no hub. A local target resolves
/// the selector in this project's log as before; a listed one resolves it
/// against the hub's `sessions` answer across every project. `credential`
/// is `--credential`'s label, carried on the opening subscribe so the
/// session starts or switches to it
/// (`docs/model-routing.md`, "Which credential a session uses").
pub(crate) fn resume(
    selector: Option<String>,
    credential: Option<String>,
    hub: Option<String>,
    fiber: Result<PathBuf, String>,
) -> i32 {
    if let Some(code) = needs_tty() {
        return code;
    }
    let target = match target(hub.as_deref(), Vec::new()) {
        Ok(target) => target,
        Err(e) => return crate::fail(e),
    };
    let Some(selector) = selector else {
        return crate::terminal(fiber, target, tui::OpenAt::List, None, Vec::new());
    };
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(e) => return crate::fail(crate::failed(e.code(), e)),
    };
    let clock = crate::clock::System;
    let mut local = || {
        let workspace = std::env::current_dir().map_err(|e| {
            crate::failed(
                contract::ErrorCode::IoFailed,
                format!("the current directory: {e}"),
            )
        })?;
        let project = doors::project(&workspace);
        let sessions = log::sessions_dir(&home, &project);
        let in_project = in_project(&project, &doors::project);
        log::resolve(&sessions, &selector, &in_project).map_err(|e| crate::failed(e.code(), e))
    };
    let mut listed = |name: &str| {
        let mut connect = || crate::connect_hub(&home, &target, &fiber, &clock);
        ::cli::resolve_listed(&selector, name, &mut connect).map_err(Failure::from)
    };
    let id = match route(&target, &mut local, &mut listed) {
        Ok(id) => id,
        Err(e) => return crate::fail(e),
    };
    log::crash::attach(&id);
    crate::terminal(
        fiber,
        target,
        tui::OpenAt::Session(id, credential),
        None,
        Vec::new(),
    )
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
    log::crash::attach(&id);
    crate::terminal(
        fiber,
        doors::hub::Target::Local,
        tui::OpenAt::Session(id, None),
        None,
        Vec::new(),
    )
}

/// `fiber` with no arguments on one hub: the tty, then the target, then
/// the terminal. A listed target opens home on that hub; without a flag
/// or a default the local hub opens the launch directory as before.
pub(crate) fn home(
    fiber: Result<PathBuf, String>,
    hub: Option<String>,
    model: Option<String>,
    overrides: Vec<String>,
) -> i32 {
    if let Some(code) = needs_tty() {
        return code;
    }
    let target = match target(
        hub.as_deref(),
        crate::per_run(model.clone(), overrides.clone()),
    ) {
        Ok(target) => target,
        Err(e) => return crate::fail(e),
    };
    crate::terminal(fiber, target, tui::OpenAt::Home, model, overrides)
}

/// The per-run layer entries that select a hub: one whose dotted key's
/// first segment is `hub` or `hubs` exactly, so `hub.default` and
/// `hubs."box".address` ride while `hubbub`, `hub_x` and `a.hub` stay
/// with the terminal run. A key the path parser refuses rides nowhere,
/// and an entry with no `=` names no key.
pub(crate) fn hub_entries(layer: Vec<String>) -> Vec<String> {
    layer
        .into_iter()
        .filter(|entry| {
            entry
                .split_once('=')
                .and_then(|(key, _)| config::path::parse(key))
                .is_some_and(|segments| {
                    matches!(segments.first().map(String::as_str), Some("hub" | "hubs"))
                })
        })
        .collect()
}

/// The hub one client command talks to: `--hub` wins, else the
/// `hub.default` the run's layer carries, else the local hub. The layer
/// is the run's own, so `-c hub.default=box` reads like `--hub box` for
/// one run, while an unrelated bad `-c` never reaches the terminal's
/// own load.
pub(crate) fn target(
    named: Option<&str>,
    layer: Vec<String>,
) -> Result<doors::hub::Target, Failure> {
    let home = config::fiber_home_from_env().map_err(|e| crate::failed(e.code(), e))?;
    let workspace = std::env::current_dir().map_err(|e| {
        crate::failed(
            contract::ErrorCode::IoFailed,
            format!("the current directory: {e}"),
        )
    })?;
    target_in(&home, &workspace, named, layer)
}

/// `target` with its directories passed in, so tests run without process
/// state: Fiber home's files and the launch directory's repository file
/// load as the process would read them.
fn target_in(
    home: &Path,
    workspace: &Path,
    named: Option<&str>,
    layer: Vec<String>,
) -> Result<doors::hub::Target, Failure> {
    let project = config::ProjectKey::new(log::project_key(&doors::project(workspace)))
        .map_err(|e| crate::failed(e.code(), e))?;
    let config = config::Config::load(config::Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides: hub_entries(layer),
    })
    .map_err(|e| crate::failed(e.code(), e))?;
    doors::hub::target(&config, home, named).map_err(|e| crate::failed(e.code(), e))
}

/// Picks the session `resume` opens: the project's own log for a local
/// target, the hub's `sessions` answer for a listed one. Only the taken
/// side runs, so a listed hub never reads the local log and a local
/// resume never connects.
pub(crate) fn route(
    target: &doors::hub::Target,
    local: &mut dyn FnMut() -> Result<SessionId, Failure>,
    listed: &mut dyn FnMut(&str) -> Result<SessionId, Failure>,
) -> Result<SessionId, Failure> {
    match target {
        doors::hub::Target::Local => local(),
        doors::hub::Target::Listed { name, .. } => listed(name),
    }
}

/// One configuration notice as the terminal prints it on a listed hub:
/// the client's file owns no session startup notice, so each of the
/// client's own notices prints once to stderr before the terminal takes
/// the screen.
pub(crate) fn notice_lines(notices: &[contract::events::Notice]) -> Vec<String> {
    notices
        .iter()
        .map(|notice| format!("fiber: {}", notice.message))
        .collect()
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

/// The project test `resolve` and `most_recent` take for `project`, and
/// the launch-project check in `resume` reuses: the identity of each
/// workspace, through `identity`. Each distinct workspace runs it once,
/// so a project of many sessions in one workspace runs one `git`.
pub(crate) fn in_project<'a>(
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
