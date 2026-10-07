//! `fiber sessions export` and `fiber sessions delete` (`docs/invocation.md`,
//! "Deleting and pruning"): export copies a session's log lines and its
//! `artifacts/` into a new directory; delete asks the hub to remove a
//! session, after asking the person.

use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};

use contract::shapes::Failure;
use contract::{ErrorCode, HubLine, SessionId};
use serde_json::{Value, json};

use crate::approve::{confirmed, say};
use crate::{fail, failed, project_of, usage};

/// What the delete prompt asks.
const DELETE_PROMPT: &str = "delete? [y/N]";

/// The usage failure when there is nobody to ask.
const DELETE_NOBODY: &str =
    "`fiber sessions delete` has no terminal to ask on. Pass `--yes` to delete without asking.";

/// The `id` of the one command `delete` sends.
const DELETE_ID: &str = "c_delete";

/// `fiber sessions export <id> [<path>]` in the current directory: resolves
/// the session, copies it, and prints the export directory's path.
pub fn export(selector: &str, path: Option<&Path>) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            run(&home, &workspace, selector, path, &mut io::stdout())
        });
    match ran {
        Ok(_) => 0,
        Err(e) => fail(e),
    }
}

/// Resolves `selector` in the current directory's project and exports the
/// session into `path`, defaulting to the full session id under the current
/// directory. A relative `path` is taken from the current directory. Gives
/// the export directory's absolute path.
fn run(
    home: &Path,
    workspace: &Path,
    selector: &str,
    path: Option<&Path>,
    out: &mut dyn Write,
) -> Result<PathBuf, Failure> {
    let (sessions, _) = project_of(home, workspace)?;
    let project = doors::project(workspace);
    let id = log::resolve(&sessions, selector, &|started| {
        doors::project(Path::new(started)) == project
    })
    .map_err(|e| failed(e.code(), e))?;
    let target = match path {
        Some(path) => workspace.join(path),
        None => workspace.join(&id.0),
    };
    log::export(&sessions.join(&id.0), &target).map_err(|e| failed(e.code(), e))?;
    writeln!(out, "{}", target.display())
        .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
    Ok(target)
}

/// `fiber sessions delete [--cascade] [--yes] <id>` in the current
/// directory: resolves the session, asks unless `yes`, and sends one
/// `delete` to the hub `connect` reaches, starting one when none runs.
pub fn delete(
    selector: &str,
    cascade: bool,
    yes: bool,
    connect: &mut dyn FnMut() -> io::Result<doors::hub::Hub>,
) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            let ask = Ask {
                yes,
                terminal,
                input: &mut stdin.lock(),
                err: &mut io::stderr(),
            };
            delete_run(
                &home,
                &workspace,
                selector,
                cascade,
                ask,
                &mut io::stdout(),
                connect,
            )
        });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// How `delete` confirms: `yes` skips the question; otherwise it is asked
/// on `err` and read from `input`, which only a `terminal` may answer.
pub(crate) struct Ask<'a> {
    pub(crate) yes: bool,
    pub(crate) terminal: bool,
    pub(crate) input: &'a mut dyn BufRead,
    pub(crate) err: &'a mut dyn Write,
}

/// Resolves `selector` in the current directory's project, lists what the
/// delete removes on `out` and asks unless `yes`, then sends `delete` and
/// prints each id it sent once the hub accepts. Without `yes` and without
/// a terminal nothing is read and the hub is never reached.
fn delete_run(
    home: &Path,
    workspace: &Path,
    selector: &str,
    cascade: bool,
    ask: Ask<'_>,
    out: &mut dyn Write,
    connect: &mut dyn FnMut() -> io::Result<doors::hub::Hub>,
) -> Result<(), Failure> {
    if !ask.yes && !ask.terminal {
        return Err(usage(DELETE_NOBODY));
    }
    let (sessions, _) = project_of(home, workspace)?;
    let project = doors::project(workspace);
    let id = log::resolve(&sessions, selector, &|started| {
        doors::project(Path::new(started)) == project
    })
    .map_err(|e| failed(e.code(), e))?;
    let mut listed = vec![id.clone()];
    if cascade {
        listed.extend(log::dependents(home, &id));
    }
    if !ask.yes {
        print_ids(out, &listed)?;
        if !confirmed(
            DELETE_PROMPT,
            DELETE_NOBODY,
            ask.terminal,
            ask.input,
            ask.err,
        )? {
            say(ask.err, "nothing deleted\n");
            return Ok(());
        }
    }
    let (stream, _) =
        connect().map_err(|e| failed(ErrorCode::IoFailed, format!("the hub: {e}")))?;
    let mut read = BufReader::new(stream);
    send_delete(&mut read, DELETE_ID, &id, cascade)?;
    print_ids(out, &listed)
}

/// Sends `delete` for `id` and reads until its answer, skipping every
/// other line. An end before the answer is `io_failed`; a rejection is a
/// failure with its code and message.
pub(crate) fn send_delete(
    read: &mut BufReader<std::os::unix::net::UnixStream>,
    command_id: &str,
    id: &SessionId,
    cascade: bool,
) -> Result<(), Failure> {
    let mut args = serde_json::Map::new();
    args.insert("session".to_owned(), Value::String(id.0.clone()));
    if cascade {
        args.insert("cascade".to_owned(), Value::Bool(true));
    }
    let mut line = json!({"id": command_id, "command": "delete", "args": args}).to_string();
    line.push('\n');
    let lost = |e: io::Error| failed(ErrorCode::IoFailed, format!("the hub: {e}"));
    read.get_mut().write_all(line.as_bytes()).map_err(lost)?;
    let mut buf = String::new();
    loop {
        buf.clear();
        if read.read_line(&mut buf).map_err(lost)? == 0 {
            return Err(failed(
                ErrorCode::IoFailed,
                "the hub closed the connection before answering `delete`",
            ));
        }
        let Ok(answer) = serde_json::from_str::<HubLine>(&buf) else {
            continue;
        };
        let field = |key: &str| answer.payload.get(key).and_then(Value::as_str);
        if field("command_id") != Some(command_id) {
            continue;
        }
        match answer.kind.as_str() {
            "command_accepted" => return Ok(()),
            "command_rejected" => {
                let code = field("code")
                    .and_then(|code| serde_json::from_value(Value::String(code.to_owned())).ok())
                    .unwrap_or(ErrorCode::IoFailed);
                let message = field("message").unwrap_or("the hub refused `delete`");
                return Err(failed(code, message));
            }
            _ => {}
        }
    }
}

fn print_ids(out: &mut dyn Write, ids: &[SessionId]) -> Result<(), Failure> {
    for id in ids {
        writeln!(out, "{}", id.0)
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
