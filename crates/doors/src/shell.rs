//! A driver `shell` with `send` false (`docs/architecture.md`, "One inbox").
//! The connection's reader checks it and registers its cancel, then runs it
//! on a thread of its own and reads on, so a `cancel` on the same connection
//! reaches it. The loop never sees it.

use contract::ErrorCode;
use contract::clock::Wake;
use contract::commands::Shell;
use contract::emit::Emit;
use contract::events::{CommandResult, Event};
use contract::inbox::{Ack, Rejection};
use contract::shapes::ContentPart;
use contract::tool::{Bound, Cancel, Output, Tool};
use log::{cut_joined, cut_notice};
use serde_json::{Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use crate::mint;
use crate::session::Gate;

const SEND: &str = "`send` is not built in this Fiber yet.";
pub(crate) const COULD_NOT_START: &str = "The command could not start.";

/// Why a `shell` does not start, answered on the reader.
pub(crate) enum Refused {
    Unknown,
    Rejected { code: ErrorCode, message: String },
}

/// A driver shell that passed its checks, with its cancel registered.
pub(crate) struct Running {
    tool: Arc<dyn Tool>,
    cancel: Arc<ShellCancel>,
    arguments: Map<String, Value>,
}

/// The cancel signal one driver shell sees.
pub(crate) struct ShellCancel {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<Weak<dyn Wake>>>,
}

impl ShellCancel {
    pub(crate) fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            wakers: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        let wakers = lock(&self.wakers).clone();
        for waker in wakers {
            if let Some(waker) = waker.upgrade() {
                waker.wake();
            }
        }
    }
}

impl Cancel for ShellCancel {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        lock(&self.wakers).push(waker);
    }
}

struct Silent;

impl Emit for Silent {
    fn emit(&self, _event: &Event) {}
}

/// Checks `command` and registers its cancel with `gate`, so a `cancel`
/// read after this returns reaches it. Called on the reader thread.
pub(crate) fn start(gate: &Arc<Gate>, command: &Shell) -> Result<Running, Refused> {
    let Some(tool) = gate.driver_shell() else {
        return Err(Refused::Unknown);
    };
    if command.send {
        return Err(Refused::Rejected {
            code: ErrorCode::InvalidArguments,
            message: SEND.to_owned(),
        });
    }
    let cancel = Arc::new(ShellCancel::new());
    gate.shells.track(Arc::clone(&cancel));
    let mut arguments = Map::new();
    arguments.insert("command".to_owned(), Value::String(command.command.clone()));
    Ok(Running {
        tool,
        cancel,
        arguments,
    })
}

impl Running {
    /// Runs the tool, answers through `ack`, then unregisters the cancel:
    /// once [`Gate`] no longer lists the shell, its answer is queued. The
    /// registry lock is not held across [`Tool::run`].
    pub(crate) fn finish(self, gate: &Gate, ack: Ack) {
        let output = self
            .tool
            .run(&self.arguments, self.cancel.as_ref(), &Silent);
        (ack.0)(answered(gate, output, self.tool.bound()));
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
