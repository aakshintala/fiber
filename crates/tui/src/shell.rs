//! `!` commands: a draft that runs a shell command, and how its output
//! shows (`docs/tui.md`, "The input box"; `docs/invocation.md`, `shell`).

use contract::ErrorCode;
use contract::SessionId;
use contract::events::{CommandResult, ShellCommand};
use contract::shapes::Process;
use serde_json::{Value, json};

use crate::app::session_command;

/// A draft that runs a shell command: the command, and `send`, whether
/// its output goes with the next prompt. `!!cmd` shows the output only to
/// the person; `!cmd` sends it. `None` for any other draft, and for a bang
/// with no command, which is an ordinary prompt.
pub(crate) fn parse(text: &str) -> Option<(&str, bool)> {
    let text = text.trim();
    let (rest, send) = match text.strip_prefix("!!") {
        Some(rest) => (rest, false),
        None => (text.strip_prefix('!')?, true),
    };
    let command = rest.trim();
    (!command.is_empty()).then_some((command, send))
}

/// The `shell` command line for `session`.
pub(crate) fn command(id: &str, session: &SessionId, command: &str, send: bool) -> Value {
    let args = json!({"command": command, "send": send});
    session_command(id, "shell", session, Some(args))
}

/// The item a `shell` answer shows for the draft `text` that sent it:
/// only `!!`'s, since `!`'s output shows from its `shell_command`.
pub(crate) fn answered(text: &str, result: Option<CommandResult>) -> Option<String> {
    let (command, false) = parse(text)? else {
        return None;
    };
    match result? {
        CommandResult::Shell {
            output, process, ..
        } => Some(row(command, "only you see this", &output, &process)),
        CommandResult::Rewind { .. }
        | CommandResult::Tools { .. }
        | CommandResult::McpServers { .. }
        | CommandResult::Commands { .. }
        | CommandResult::Skills { .. }
        | CommandResult::Quota { .. }
        | CommandResult::Credentials(_)
        | CommandResult::ProposeNotes { .. }
        | CommandResult::CredentialSwitch { .. }
        | CommandResult::History { .. }
        | CommandResult::ArtifactPage { .. }
        | CommandResult::OfferedFile { .. }
        | CommandResult::Start { .. }
        | CommandResult::Status { .. }
        | CommandResult::Subscribe { .. }
        | CommandResult::Import { .. }
        | CommandResult::PlaceholderEnv { .. }
        | CommandResult::Call(_)
        | CommandResult::Hooks { .. }
        | CommandResult::Stop { .. } => None,
    }
}

/// The item a `shell_command` line shows. Only a `!` run with `send`
/// true writes a `shell_command` line, so the row always carries the
/// sent note.
pub(crate) fn ran(line: &ShellCommand) -> String {
    row(
        &line.command,
        "sent with your next prompt",
        &line.output,
        &line.process,
    )
}

/// The single transcript row for a shell run: `! <command>`, the kind
/// note, the output verbatim, then how it ended. Unlike `item()`, the
/// row always names a successful exit.
pub(crate) fn row(command: &str, note: &str, output: &str, process: &Process) -> String {
    let body = item(command, output, process);
    let head = format!("! {command}");
    let rest = body.strip_prefix(head.as_str()).unwrap_or_default();
    let mut text = format!("{head}\n{note}{rest}");
    if process.signal.is_none() && process.exit_code == Some(0) {
        text.push_str("\nexit 0");
    }
    text
}

/// The transcript row for a command that could not start: `! <command>`
/// then `<code>: <message>` exactly as it arrived.
pub(crate) fn error_row(command: &str, code: &ErrorCode, message: &str) -> String {
    let code = code.name();
    format!("! {command}\n{code}: {message}")
}

/// The conversation item for a command and how it ended: `! <command>`,
/// its output, then the exit code when not 0, or the signal.
pub(crate) fn item(command: &str, output: &str, process: &Process) -> String {
    let mut text = format!("! {command}");
    let output = output.trim_end();
    if !output.is_empty() {
        text.push('\n');
        text.push_str(output);
    }
    match (&process.signal, process.exit_code) {
        (Some(signal), _) => text.push_str(&format!("\nsignal {signal}")),
        (None, Some(code)) if code != 0 => text.push_str(&format!("\nexit {code}")),
        (None, Some(_) | None) => {}
    }
    text
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
