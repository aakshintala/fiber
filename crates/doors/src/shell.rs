//! A driver `shell` with `send` false, run on the connection's reader thread
//! (`docs/architecture.md`, "One inbox"). The loop never sees it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use contract::ErrorCode;
use contract::clock::Wake;
use contract::commands::Shell;
use contract::emit::Emit;
use contract::events::Event;
use contract::shapes::{ContentPart, Process};
use contract::tool::{Bound, Cancel, Output};
use serde_json::{Map, Value};

use crate::mint;
use crate::session::Gate;

const SEND: &str = "`send` is not built in this Fiber yet.";
const COULD_NOT_START: &str = "The command could not start.";

pub(crate) enum Answer {
    Unknown,
    Rejected {
        code: ErrorCode,
        message: String,
    },
    Accepted {
        output: String,
        artifact: Option<String>,
        process: Process,
    },
}

/// The cancel signal one driver shell sees.
pub(crate) struct ShellCancel {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<Weak<dyn Wake>>>,
}

impl ShellCancel {
    fn new() -> Self {
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

/// Runs `command` on the caller's thread. The registry lock is not held
/// across [`Tool::run`].
pub(crate) fn run(gate: &Arc<Gate>, command: &Shell) -> Answer {
    let Some(tool) = gate.driver_shell() else {
        return Answer::Unknown;
    };
    if command.send {
        return Answer::Rejected {
            code: ErrorCode::InvalidArguments,
            message: SEND.to_owned(),
        };
    }
    let cancel = Arc::new(ShellCancel::new());
    gate.track_shell(Arc::clone(&cancel));
    let mut arguments = Map::new();
    arguments.insert("command".to_owned(), Value::String(command.command.clone()));
    let output = tool.run(&arguments, cancel.as_ref(), &Silent);
    // A panic aborts the process, so nothing later cancels this shell.
    gate.untrack_shell(&cancel);
    let bound = tool.bound();
    answered(gate, output, bound)
}

fn answered(gate: &Gate, output: Output, bound: Bound) -> Answer {
    let Some(process) = output.process else {
        let message = match output.error {
            Some(error) => error.message,
            None => COULD_NOT_START.to_owned(),
        };
        return Answer::Rejected {
            code: ErrorCode::InvalidArguments,
            message,
        };
    };
    let full = joined(&output.content);
    let (kept, artifact) = cut(gate, &full, bound);
    Answer::Accepted {
        output: kept,
        artifact,
        process,
    }
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
