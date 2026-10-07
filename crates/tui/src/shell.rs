//! `!` commands: a draft that runs a shell command, and how its output
//! shows (`docs/tui.md`, "The input box"; `docs/invocation.md`, `shell`).

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
        } => Some(item(command, &output, &process)),
        CommandResult::Rewind { .. }
        | CommandResult::Tools { .. }
        | CommandResult::Commands { .. }
        | CommandResult::History { .. } => None,
    }
}

/// The item a `shell_command` line shows.
pub(crate) fn ran(line: &ShellCommand) -> String {
    item(&line.command, &line.output, &line.process)
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
