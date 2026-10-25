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

use crate::CliError;
use crate::approve::{confirmed, say};
use crate::{project_of, with_dirs, write_line};

/// What the delete prompt asks.
const DELETE_PROMPT: &str = "delete? [y/N]";

/// The usage failure when there is nobody to ask.
pub(crate) const DELETE_NOBODY: &str =
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
) -> Result<PathBuf, CliError> {
    let (sessions, _) = project_of(home, workspace)?;
    let project = doors::project(workspace);
    let id = log::resolve(&sessions, selector, &|started| {
        doors::project(Path::new(started)) == project
    })?;
    let target = match path {
        Some(path) => workspace.join(path),
        None => workspace.join(&id.0),
    };
    log::export(&sessions.join(&id.0), &target)?;
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
) -> Result<(), CliError> {
    if !ask.yes && !ask.terminal {
        return Err(CliError::DeleteNobody);
    }
    let (sessions, _) = project_of(home, workspace)?;
    let project = doors::project(workspace);
    let id = log::resolve(&sessions, selector, &|started| {
        doors::project(Path::new(started)) == project
    })?;
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
    let (stream, _) = connect().map_err(CliError::Hub)?;
    let mut read = BufReader::new(stream);
    delete_value(&mut read, DELETE_ID, &id, cascade)?;
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
    // debt: a later #923 part deletes this wrapper once prune calls `delete_value`.
    delete_value(read, command_id, id, cascade).map_err(Failure::from)
}

/// The typed core behind [`send_delete`]: sends `delete` for `id` and
/// reads until its answer, skipping every other line.
fn delete_value(
    read: &mut BufReader<UnixStream>,
    command_id: &str,
    id: &SessionId,
    cascade: bool,
) -> Result<(), CliError> {
    request_value(
        read,
        command_id,
        HubCommand::Delete(Delete {
            session: id.clone(),
            cascade: cascade.then_some(true),
            expect: None,
            dry_run: None,
        }),
    )
    .map(drop)
}

/// Local connections keep their existing message; listed failures keep their code.
pub(crate) fn hub_failure(error: doors::Error) -> Failure {
    // debt: a later #923 part deletes this wrapper once its callers build `CliError::Hub` directly.
    Failure::from(CliError::Hub(error))
}

/// The hub's answer to `command_id`, when `line` carries it: the accepted
/// `result`, `null` when it has none, or the rejection as a failure.
/// `None` for any other line. A rejection without a usable code is
/// `io_failed`; one without a message names the command.
pub(crate) fn answer_of(line: &HubLine, id: &str, command: &str) -> Option<Result<Value, Failure>> {
    // debt: a later #923 part deletes this wrapper once hub_status returns `CliError`.
    matched(line.clone(), id, || command.to_owned()).map(|r| r.map_err(Failure::from))
}

/// The answer body over an owned line: moves an accepted `result` out of
/// the payload instead of cloning it. `command` runs at most once, only
/// for a rejection with no `message`.
fn matched(
    mut line: HubLine,
    id: &str,
    command: impl FnOnce() -> String,
) -> Option<Result<Value, CliError>> {
    if line.payload.get("command_id").and_then(Value::as_str) != Some(id) {
        return None;
    }
    match line.kind.as_str() {
        "command_accepted" => Some(Ok(line.payload.remove("result").unwrap_or(Value::Null))),
        "command_rejected" => {
            let code = line
                .payload
                .get("code")
                .and_then(Value::as_str)
                .and_then(|code| serde_json::from_value(Value::String(code.to_owned())).ok())
                .unwrap_or(ErrorCode::IoFailed);
            let message = match line.payload.remove("message") {
                Some(Value::String(message)) => message,
                _ => format!("the hub refused `{}`", command()),
            };
            Some(Err(CliError::Rejected { code, message }))
        }
        _ => None,
    }
}

/// The name for the messages below, read from the serialised line so a
/// renamed variant cannot leave a stale name here.
fn name_of(bytes: &[u8]) -> String {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Sends hub `command` as `command_id` and reads until its answer,
/// skipping every other line: the accepted `result`, `null` when it has
/// none. An end before the answer is `io_failed`; a rejection is a
/// failure with its code and message.
pub(crate) fn request_value(
    read: &mut BufReader<UnixStream>,
    command_id: &str,
    command: HubCommand,
) -> Result<Value, CliError> {
    let line = HubCommandLine {
        id: CommandId(command_id.to_owned()),
        command,
    };
    let bytes = serde_json::to_vec(&line).map_err(CliError::CommandEncode)?;
    let lost = |e: io::Error| CliError::Hub(doors::Error::Local(e));
    support::framing::write_line(read.get_mut(), &bytes).map_err(lost)?;
    loop {
        let Some(buf) =
            support::framing::read_line(read, support::framing::MAX_LINE).map_err(lost)?
        else {
            return Err(CliError::HubClosed {
                command: name_of(&bytes),
            });
        };
        let Ok(answer) = serde_json::from_slice::<HubLine>(&buf) else {
            continue;
        };
        if let Some(answered) = matched(answer, command_id, || name_of(&bytes)) {
            return answered;
        }
    }
}

/// The `Failure` wrapper over [`request_value`] for the callers outside
/// the sessions family, which still return `Failure`.
pub(crate) fn request(
    read: &mut BufReader<UnixStream>,
    command_id: &str,
    command: HubCommand,
) -> Result<Value, Failure> {
    // debt: a later #923 part deletes this wrapper once hub_pair, hub_token, hub_refresh and hub_list return `CliError`.
    request_value(read, command_id, command).map_err(Failure::from)
}

fn print_ids(out: &mut dyn Write, ids: &[SessionId]) -> Result<(), CliError> {
    for id in ids {
        write_line(out, &id.0)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
