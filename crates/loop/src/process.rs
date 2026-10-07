//! The process boundary in a session's log (`docs/events.md`, "Process
//! boundary"): `fiber_started` before the session's first line, and
//! `fiber_exited`, folded from the log, after the loop has stopped. Only the
//! loop writes durable events (`docs/architecture.md`, "Streaming").

use std::path::Path;

use contract::events::{
    Event, ExtensionsLoaded, FiberExited, FiberStarted, FinalMessage, LoadedExtension,
    McpServerFailed, MessageOutcome, Notice, TurnOutcome,
};
use contract::shapes::{Failure, Question};
use contract::RequestId;
use log::Log;

use crate::Error;
use crate::usage::Ledger;

/// Writes `fiber_started`, the first line this process writes for the
/// session, naming the running Fiber's `version`. `resumed` is whether the
/// session already existed (`docs/invocation.md`, "Lifecycle").
pub fn fiber_started(log: &Log, version: &str, resumed: bool) -> Result<(), Error> {
    let started = Event::FiberStarted(FiberStarted {
        version: version.to_owned(),
        resumed,
    });
    log.append(&started, None, None)?;
    Ok(())
}

/// Writes `extensions_loaded`, the whole set of extensions the session
/// loaded, then each notice loading them raised (`docs/events.md`,
/// `extensions_loaded`). Written after `fiber_started`, before the first
/// model request.
pub fn extensions_loaded(
    log: &Log,
    loaded: Vec<LoadedExtension>,
    notices: Vec<Notice>,
) -> Result<(), Error> {
    let event = Event::ExtensionsLoaded(ExtensionsLoaded { extensions: loaded });
    log.append(&event, None, None)?;
    for notice in notices {
        log.append(&Event::Notice(notice), None, None)?;
    }
    Ok(())
}

/// Writes one `mcp_server_failed` line per server that failed to start,
/// then each notice starting them raised (`docs/mcp.md`, "Starting
/// servers"). Written after `fiber_started` and `extensions_loaded`,
/// before the first model request.
pub fn mcp_servers_started(
    log: &Log,
    failed: Vec<McpServerFailed>,
    notices: Vec<Notice>,
) -> Result<(), Error> {
    for failure in failed {
        log.append(&Event::McpServerFailed(failure), None, None)?;
    }
    for notice in notices {
        log.append(&Event::Notice(notice), None, None)?;
    }
    Ok(())
}

/// What `fiber_exited` wrote: the exit code and the `error` it carried,
/// byte for byte the `error` field of the `fiber_exited` line just
/// appended (`docs/errors.md`, "What a caller gets").
pub struct Exited {
    /// The exit code written.
    pub code: i32,
    /// The `error` written, `None` on success and under a signal.
    pub error: Option<Failure>,
}

/// Writes `fiber_exited` once the loop has stopped: the final message and
/// the usage, folded from the session's log in `dir`, and `ran`'s error or
/// else, for a `one_turn` run (`fiber ask`), the last turn's. A session the
/// hub started does not fail when a turn did: its clients saw each
/// `turn_completed`. Returns what it wrote: exit code 1 with an `error`
/// (`docs/errors.md`, "What a caller gets"), otherwise 0. Under a
/// shutdown, `signal` is its exit code: it is the code written, with no
/// `error` and no final message, and `suspended_on` also names a request an
/// earlier process left unresolved (`docs/invocation.md`, "Shutdown").
pub fn fiber_exited(
    log: &Log,
    dir: &Path,
    ran: Result<(), Failure>,
    one_turn: bool,
    signal: Option<i32>,
) -> Result<Exited, Error> {
    let folded = log::lines(dir)
        .map_err(Error::from)
        .and_then(|lines| fold(lines, signal.is_some()));
    let (fold, unread) = match folded {
        Ok(fold) => (fold, None),
        Err(e) => (Fold::default(), Some(e)),
    };
    let unread = unread.map(|e| Failure {
        code: e.code(),
        message: e.to_string(),
        retry_after: None,
        provider: None,
    });
    let turn_error = if one_turn { fold.error } else { None };
    let error = ran.err().or(unread).or(turn_error);
    let (exit_code, error, final_message) = match signal {
        Some(code) => (code, None, None),
        None => (i32::from(error.is_some()), error, fold.final_message),
    };
    let exited = Event::FiberExited(FiberExited {
        exit_code,
        usage: fold.ledger.usage(),
        final_message,
        error: error.clone(),
        suspended_on: fold.suspended_on,
        questions: fold.questions,
    });
    log.append(&exited, None, None)?;
    Ok(Exited {
        code: exit_code,
        error,
    })
}

/// What `fiber_exited` reports, folded from the session's log.
#[derive(Default)]
struct Fold {
    final_message: Option<FinalMessage>,
    error: Option<Failure>,
    questions: Option<Vec<Question>>,
    ledger: Ledger,
    /// The latest approval or question this process left unresolved.
    suspended_on: Option<RequestId>,
}

/// Folds `lines`, one at a time. With `keep_open`, the requests raised
/// before the latest `fiber_started` and still unresolved stay open across
/// it. The first line that does not read ends the fold with its error.
fn fold(lines: log::Lines, keep_open: bool) -> Result<Fold, Error> {
    let mut fold = Fold::default();
    // Text parts of the open assistant message, joined in log order.
    // Every message takes them: messages never interleave, and a failed
    // call logs none, so the next message must not inherit them.
    let mut text = String::new();
    // Requests this process raised and has not resolved, oldest first.
    // `suspended_on` is the latest (`docs/events.md`, `fiber_exited`).
    let mut open = Vec::new();
    for line in lines {
        let line = line?;
        let event = Event::from_envelope(&line).map_err(Error::Unreadable)?;
        // Each process reports its own lines only: the fold restarts at
        // the latest `fiber_started` (`docs/events.md`, `fiber_exited`:
        // `usage` is "this process's model calls for the session").
        if matches!(&event, Some(Event::FiberStarted(_))) {
            fold = Fold::default();
            text.clear();
            if !keep_open {
                open.clear();
            }
            continue;
        }
        if let Some(Event::PermissionRequested(requested)) = &event {
            open.push(requested.request_id.clone());
        } else if let Some(Event::InteractionRequested(requested)) = &event {
            open.push(requested.request_id.clone());
        } else if let Some(Event::PermissionResolved(resolved)) = &event {
            if let Some(id) = &resolved.request_id {
                open.retain(|pending| pending != id);
            }
        } else if let Some(Event::InteractionResolved(resolved)) = &event {
            open.retain(|pending| pending != &resolved.request_id);
        }
        if let Some(Event::TurnStarted(_)) = &event {
            fold.final_message = None;
            fold.error = None;
            fold.questions = None;
        } else if let Some(Event::TextCompleted(part)) = &event {
            text.push_str(&part.text);
        } else if let Some(Event::AssistantMessageCompleted(message)) = &event {
            let part = std::mem::take(&mut text);
            if message.outcome == MessageOutcome::Completed
                && let Some(id) = &line.action_id
            {
                fold.final_message = Some(FinalMessage {
                    final_action_id: id.clone(),
                    text: part,
                });
            }
        } else if let Some(Event::TurnCompleted(turn)) = &event {
            if turn.outcome == TurnOutcome::Failed {
                fold.final_message = None;
            }
            fold.error = turn.error.clone();
            fold.questions = turn.questions.clone();
        } else if let Some(Event::UsageRecorded(call)) = &event {
            fold.ledger.record(call);
        }
    }
    fold.suspended_on = open.last().cloned();
    Ok(fold)
}
