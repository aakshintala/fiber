//! `fiber sessions export` and `fiber sessions delete` (`docs/invocation.md`,
//! "Deleting and pruning"): export copies a session's log lines and its
//! `artifacts/` into a new directory; delete asks the hub to remove a
//! session, after asking the person.

use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use contract::commands::hub::{Delete, HubCommand, HubCommandLine};
use contract::shapes::Failure;
use contract::{CommandId, ErrorCode, HubLine, SessionId};
use serde_json::Value;

use crate::approve::{confirmed, say};
use crate::{failed, project_of, usage, with_dirs, write_line};

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
    // The inner run's directory value is discarded once its output
    // succeeds; only the exit code leaves the wrapper.
    with_dirs(|home, workspace| run(home, workspace, selector, path, &mut io::stdout()).map(|_| ()))
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
    write_line(out, &target.display().to_string())?;
    Ok(target)
}

/// `fiber sessions delete [--cascade] [--yes] <id>` in the current
/// directory: resolves the session, asks unless `yes`, and sends one
/// `delete` to the hub `connect` reaches, starting one when none runs.
pub fn delete(
    selector: &str,
    cascade: bool,
    yes: bool,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    with_dirs(|home, workspace| {
        let ask = Ask {
            yes,
            terminal,
            input: &mut stdin.lock(),
            err: &mut io::stderr(),
        };
        delete_run(
            home,
            workspace,
            selector,
            cascade,
            ask,
            &mut io::stdout(),
            connect,
        )
    })
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
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
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
    let (stream, _) = connect().map_err(crate::sessions::hub_failure)?;
    let mut read = BufReader::new(stream);
    send_delete(&mut read, DELETE_ID, &id, cascade)?;
    print_ids(out, &listed)
}

/// Sends `delete` for `id` and reads until its answer, skipping every
/// other line. An end before the answer is `io_failed`; a rejection is a
/// failure with its code and message.
pub(crate) fn send_delete(
    read: &mut BufReader<UnixStream>,
    command_id: &str,
    id: &SessionId,
    cascade: bool,
) -> Result<(), Failure> {
    request(
        read,
        command_id,
        HubCommand::Delete(Delete {
            session: id.clone(),
            cascade: cascade.then_some(true),
            expect: None,
        }),
    )
    .map(drop)
}

/// Local connections keep their existing message; listed failures keep their code.
pub(crate) fn hub_failure(error: doors::Error) -> Failure {
    if let doors::Error::Local(error) = error {
        failed(ErrorCode::IoFailed, format!("the hub: {error}"))
    } else {
        failed(error.code(), error)
    }
}

/// The hub's answer to `command_id`, when `line` carries it: the accepted
/// `result`, `null` when it has none, or the rejection as a failure.
/// `None` for any other line. A rejection without a usable code is
/// `io_failed`; one without a message names the command.
pub(crate) fn answer_of(line: &HubLine, id: &str, command: &str) -> Option<Result<Value, Failure>> {
    if line.payload.get("command_id").and_then(Value::as_str) != Some(id) {
        return None;
    }
    match line.kind.as_str() {
        "command_accepted" => Some(Ok(line
            .payload
            .get("result")
            .cloned()
            .unwrap_or(Value::Null))),
        "command_rejected" => {
            let code = line
                .payload
                .get("code")
                .and_then(Value::as_str)
                .and_then(|code| serde_json::from_value(Value::String(code.to_owned())).ok())
                .unwrap_or(ErrorCode::IoFailed);
            let message = line
                .payload
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| format!("the hub refused `{command}`"), str::to_owned);
            Some(Err(failed(code, message)))
        }
        _ => None,
    }
}

/// Sends hub `command` as `command_id` and reads until its answer,
/// skipping every other line: the accepted `result`, `null` when it has
/// none. An end before the answer is `io_failed`; a rejection is a
/// failure with its code and message.
pub(crate) fn request(
    read: &mut BufReader<UnixStream>,
    command_id: &str,
    command: HubCommand,
) -> Result<Value, Failure> {
    let line = HubCommandLine {
        id: CommandId(command_id.to_owned()),
        command,
    };
    let bytes = serde_json::to_vec(&line).map_err(|e| {
        failed(
            ErrorCode::IoFailed,
            format!("a command could not be written as JSON: {e}"),
        )
    })?;
    // The name for the messages below, read from the serialised line so a
    // renamed variant cannot leave a stale name here.
    let name = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|value| {
            value
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let lost = |e: io::Error| failed(ErrorCode::IoFailed, format!("the hub: {e}"));
    support::framing::write_line(read.get_mut(), &bytes).map_err(lost)?;
    loop {
        let Some(buf) =
            support::framing::read_line(read, support::framing::MAX_LINE).map_err(lost)?
        else {
            return Err(failed(
                ErrorCode::IoFailed,
                format!("the hub closed the connection before answering `{name}`"),
            ));
        };
        let Ok(answer) = serde_json::from_slice::<HubLine>(&buf) else {
            continue;
        };
        if let Some(answered) = answer_of(&answer, command_id, &name) {
            return answered;
        }
    }
}

fn print_ids(out: &mut dyn Write, ids: &[SessionId]) -> Result<(), Failure> {
    for id in ids {
        write_line(out, &id.0)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
