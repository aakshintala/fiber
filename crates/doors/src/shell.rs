//! A driver `shell` (`docs/architecture.md`, "One inbox"). The
//! connection's reader checks it and registers its cancel, then runs it
//! on a thread of its own and reads on, so a `cancel` on the same connection
//! reaches it. With `send` false the thread answers the command itself;
//! with `send` true it sends the shaped result to the loop's inbox, and
//! the loop writes it as `shell_command` (`docs/invocation.md`, "Shell").
//! The loop never runs it.

use contract::ErrorCode;
use contract::commands::Shell;
use contract::emit::Emit;
use contract::events::{CommandResult, Event, ShellCommand};
use contract::inbox::{Ack, Delivery, Rejection};
use contract::shapes::ContentPart;
use contract::tool::{Bound, Output, Tool};
use log::{cut_joined, cut_notice};
use serde_json::{Map, Value};
use std::sync::Arc;
use support::cancel::Token;

use crate::mint;
use crate::session::Gate;

pub(crate) const COULD_NOT_START: &str = "The command could not start.";
/// What a `shell` stopped before its command started says when `cancel`
/// stopped it: the tool's own words (`tool_util.rs`, `cancelled_before`).
pub(crate) const CANCELLED_BEFORE_START: &str = "Cancelled before it started.";

/// A driver shell that passed its checks, with its cancel registered.
pub(crate) struct Running {
    tool: Arc<dyn Tool>,
    cancel: Arc<Token>,
    arguments: Map<String, Value>,
    command: String,
    send: bool,
}

struct Silent;

impl Emit for Silent {
    fn emit(&self, _event: &Event) {}
}

/// Checks `command` and registers its cancel with `gate`, so a `cancel`
/// read after this returns reaches it. Called on the reader thread.
pub(crate) fn start(gate: &Arc<Gate>, command: &Shell) -> Option<Running> {
    let tool = gate.driver_shell()?;
    let cancel = Arc::new(Token::new());
    gate.shells.track(Arc::clone(&cancel));
    let mut arguments = Map::new();
    arguments.insert("command".to_owned(), Value::String(command.command.clone()));
    Some(Running {
        tool,
        cancel,
        arguments,
        command: command.command.clone(),
        send: command.send,
    })
}

impl Running {
    /// Runs the tool, answers through `ack`, then unregisters the cancel:
    /// once [`Gate`] no longer lists the shell, its answer is queued. With
    /// `send` true the shaped result goes to the loop's inbox instead of
    /// the ack, and the loop answers it when it writes `shell_command`. A
    /// result that never ran, or whose output never saved, answers the ack
    /// directly either way. The registry lock is not held across
    /// [`Tool::run`].
    pub(crate) fn finish(self, gate: &Gate, ack: Ack) {
        let output = self
            .tool
            .run(&self.arguments, self.cancel.as_ref(), &Silent);
        let answered = answered(gate, output, self.tool.bound());
        if self.send {
            match answered {
                Ok(Some(CommandResult::Shell {
                    output,
                    artifact,
                    process,
                })) => {
                    let line = ShellCommand {
                        command: self.command,
                        output,
                        artifact,
                        process,
                    };
                    gate.deliver(Delivery::Shell(line, ack));
                }
                answered => (ack.0)(answered),
            }
        } else {
            (ack.0)(answered);
        }
        // A panic aborts the process, so nothing later cancels this shell.
        gate.shells.untrack(&self.cancel);
    }

    /// Unregisters a shell that never ran, after the reader answered it.
    pub(crate) fn abandon(self, gate: &Gate) {
        gate.shells.untrack(&self.cancel);
    }
}

fn no_process(error: Option<contract::shapes::Failure>) -> Rejection {
    let message = match error {
        Some(error) => error.message,
        None => COULD_NOT_START.to_owned(),
    };
    Rejection {
        code: ErrorCode::InvalidArguments,
        message,
    }
}

fn answered(gate: &Gate, output: Output, bound: Bound) -> contract::inbox::Answer {
    if let Some(spilled) = output.spilled {
        return answered_spilled(gate, spilled, output.process, output.error);
    }
    let Some(process) = output.process else {
        if output.error.is_none() {
            return Err(stopped_before_start(gate));
        }
        return Err(no_process(output.error));
    };
    let full = joined(&output.content);
    let (kept, artifact) = cut(gate, &full, bound);
    Ok(Some(CommandResult::Shell {
        output: kept,
        artifact,
        process,
    }))
}

/// A shell whose tool never started a process and reports no error: the
/// tool's `cancelled_before`, whose cancel token fired before `Tool::run`
/// (a real failure to start carries its error). Admission closes before
/// the shells are sealed on every `close` path, and never on `cancel`, so
/// a closed admission is a `close` stop, answered `closing` as a running
/// shell is, and an open one is a `cancel` stop, answered `cancelled`.
fn stopped_before_start(gate: &Gate) -> Rejection {
    if gate.admission.admit(|| ()).is_none() {
        return Rejection {
            code: ErrorCode::Closing,
            message: crate::client::ENDED.to_owned(),
        };
    }
    Rejection {
        code: ErrorCode::Cancelled,
        message: CANCELLED_BEFORE_START.to_owned(),
    }
}

/// A shell that already spilled its output past 1 MiB: the spilled file is
/// the artifact, shaped without a second write or cut. Text parts beside
/// the spill are ignored; the command's process is kept as it ended.
fn answered_spilled(
    gate: &Gate,
    spilled: contract::tool::Spilled,
    process: Option<contract::shapes::Process>,
    error: Option<contract::shapes::Failure>,
) -> contract::inbox::Answer {
    let contract::tool::Spilled {
        head,
        tail,
        removed,
        file,
        ..
    } = spilled;
    match file {
        Ok(kept) => {
            let Some(process) = process else {
                return Err(no_process(error));
            };
            let Some(log) = gate.log.upgrade() else {
                return Err(Rejection {
                    code: ErrorCode::Closing,
                    message: crate::client::ENDED.to_owned(),
                });
            };
            let artifact = spill_artifact(&log, &kept.path);
            let output = if removed == 0 {
                format!("{head}{tail}")
            } else {
                let notice = cut_notice(removed, Ok(&kept.path));
                cut_joined(&head, &notice, &tail)
            };
            Ok(Some(CommandResult::Shell {
                output,
                artifact: Some(artifact),
                process,
            }))
        }
        Err(unsaved) => {
            if let Some(left) = unsaved.left.as_ref()
                && let Err(remove) = std::fs::remove_file(left)
            {
                return Err(Rejection {
                    code: ErrorCode::IoFailed,
                    message: format!(
                        "The partial output at {} could not be removed: {remove}.",
                        left.display()
                    ),
                });
            }
            let Some(process) = process else {
                return Err(no_process(error));
            };
            let notice = cut_notice(removed, Err(unsaved.message.as_str()));
            let output = cut_joined(&head, &notice, &tail);
            Ok(Some(CommandResult::Shell {
                output,
                artifact: None,
                process,
            }))
        }
    }
}

/// A spilled file as the driver's artifact: its path relative to the
/// session directory, or absolute when it is not under it.
fn spill_artifact(log: &log::Log, path: &std::path::Path) -> String {
    match path.strip_prefix(log.dir()) {
        Ok(relative) => relative.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

fn joined(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn cut(gate: &Gate, full: &str, bound: Bound) -> (String, Option<String>) {
    let cap = bound.start.saturating_add(bound.end);
    if full.len() <= cap {
        return (full.to_owned(), None);
    }
    let Some(log) = gate.log.upgrade() else {
        return (full.to_owned(), None);
    };
    let name = format!("{}.txt", mint("o_"));
    log.cut_output(full, bound, &name)
}
