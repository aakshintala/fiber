//! A driver `shell` with `send` false (`docs/architecture.md`, "One inbox").
//! The connection's reader checks it and registers its cancel, then runs it
//! on a thread of its own and reads on, so a `cancel` on the same connection
//! reaches it. The loop never sees it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use contract::ErrorCode;
use contract::clock::Wake;
use contract::commands::Shell;
use contract::emit::Emit;
use contract::events::{CommandResult, Event};
use contract::inbox::{Ack, Rejection};
use contract::shapes::ContentPart;
use contract::tool::{Bound, Cancel, Output, Tool};
use serde_json::{Map, Value};

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
    gate.track_shell(Arc::clone(&cancel));
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
        gate.untrack_shell(&self.cancel);
    }

    /// Unregisters a shell that never ran, after the reader answered it.
    pub(crate) fn abandon(self, gate: &Gate) {
        gate.untrack_shell(&self.cancel);
    }
}

fn answered(gate: &Gate, output: Output, bound: Bound) -> contract::inbox::Answer {
    let Some(process) = output.process else {
        let message = match output.error {
            Some(error) => error.message,
            None => COULD_NOT_START.to_owned(),
        };
        return Err(Rejection {
            code: ErrorCode::InvalidArguments,
            message,
        });
    };
    let full = joined(&output.content);
    let (kept, artifact) = cut(gate, &full, bound);
    Ok(Some(CommandResult::Shell {
        output: kept,
        artifact,
        process,
    }))
}

fn joined(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Unknown => None,
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
